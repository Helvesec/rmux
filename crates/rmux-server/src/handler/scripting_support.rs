use rmux_core::{
    command_parser::{CommandArgument, CommandParseError, ParsedCommand, ParsedCommands},
    command_queue::CommandQueue,
    PaneId, ENVIRON_HIDDEN,
};
use rmux_proto::request::Request;
use rmux_proto::{
    CommandOutput, DisplayMessageRequest, HookName, PaneTarget, Response, RmuxError, ScopeSelector,
    Target,
};
use std::collections::VecDeque;
use std::future::Future;

use super::attach_support::ActiveAttachIdentity;
use super::client_support::capture_switch_client_target_identity;
use super::control_support::{
    control_command_response_has_individual_guards, control_command_response_stream_is_active,
    control_queue_eof_action, current_control_queue_identity, send_control_command_response_event,
    with_control_command_response_capture, with_control_queue_identity, ControlClientIdentity,
    ControlQueueEofAction, ManagedClient,
};
#[cfg(windows)]
use super::pane_support::format_references_pane_pid;
use super::{
    active_session_target, current_expected_attach_identity, expected_attach_follows_registration,
    rebase_expected_attach_session_after_switch, validate_expected_attach_identity, RequestHandler,
};
use crate::client_names::control_client_name;
use crate::control::{
    ControlCommandResponseEvent, ControlCommandResult, ControlQueueCommandOrigin,
};
use crate::hook_runtime::capture_inline_hooks;
use crate::mouse::AttachedMouseEvent;

async fn maybe_capture_control_command_response<T, F>(capture: bool, future: F) -> T
where
    F: Future<Output = T>,
{
    if capture {
        with_control_command_response_capture(future).await
    } else {
        future.await
    }
}

mod buffer_parse;
mod client_parse;
mod command_args;
mod config_engine;
mod config_parse;
mod control_input;
#[cfg(test)]
#[path = "../handler_scripting/display_panes_session_tests.rs"]
mod display_panes_session_tests;
mod display_parse;
mod floating_mouse;
mod floating_move;
mod format_context;
mod hook_commands;
mod invocation;
mod invocation_input;
mod key_parse;
mod layout_parse;
mod list_commands_runtime;
mod list_parse;
mod mode_parse;
mod mouse_resize;
pub(in crate::handler) mod new_pane;
mod new_pane_input_admission;
mod new_window_runtime;
mod pane_parse;
mod pane_placement;
mod parser_context;
mod prompt_parse;
mod prompt_runtime;
mod queue;
mod queue_current_session_transition;
mod queue_exact_target;
mod queue_lifecycle_target;
mod queue_parse;
mod queue_session_rename;
mod queue_special_target;
mod queue_transfer_identity;
mod queued_send_keys_client;
mod read_only_client_action;
mod request_parse;
mod run_shell_dispatch;
mod runtime;
mod runtime_admission;
mod runtime_execution;
mod session_parse;
mod shell_parse;
mod shell_runtime;
mod source_files;
mod source_input_admission;
mod source_input_queue_status;
mod source_internal;
mod source_runtime;
mod split_window_admission;
mod split_window_runtime;
mod targets;
mod tmux_compat;
mod tokens;
mod values;
mod wait_for_runtime;
mod window_parse;
mod window_snapshot_parse;

pub(super) use self::format_context::{
    apply_client_loop_context, client_name_for_pid, format_context_for_target,
    format_context_for_target_with_server_values, global_format_context,
    render_start_directory_template, render_target_argument,
};
pub(in crate::handler) use self::parser_context::command_parser_from_state;
pub(super) use self::prompt_parse::{ParsedPromptHistoryCommand, PromptHistoryAction};
use self::queue::{
    captures_attached_client_transition, queue_action_from_response, remove_group_contexts,
    QueueInvocation, QueueMode,
};
pub(in crate::handler) use self::queue::{
    rename_pane_target_session, rename_target_session, rename_window_target_session,
};
pub(super) use self::queue::{QueueCommandAction, QueueExecutionContext};
pub(in crate::handler) use self::queue_current_session_transition::record_queued_new_session_transition;
use self::queue_current_session_transition::QueuedCurrentSessionTransition;
use self::queue_exact_target::QueueExactTargetCapture;
#[cfg(test)]
pub(crate) use self::queue_exact_target::{
    install_queue_exact_target_capture_pause, pause_after_queue_exact_target_capture,
    QueueExactTargetCapturePause,
};
use self::queue_lifecycle_target::{QueueLifecycleTargetCapture, QueueLifecycleTargetPlan};
use self::queue_session_rename::QueuedSessionRename;
use self::queue_special_target::QueueSpecialTargetPlan;
pub(in crate::handler) use self::read_only_client_action::{
    read_only_client_action, ReadOnlyClientAction,
};
use self::request_parse::parse_queue_invocation;
#[cfg(test)]
pub(crate) use self::request_parse::parse_request_from_parts;
use self::targets::{
    command_has_independent_target, implicit_pane_target, implicit_session_name,
    implicit_split_target, implicit_window_target, marked_pane_target,
    marked_pane_target_or_current, parse_layout_name, parse_move_window_target,
    parse_new_window_target_argument, parse_pane_target, parse_select_layout_target,
    parse_session_name, parse_session_target, parse_split_window_target, parse_target_arg,
    parse_window_target, queue_target_find_context, resolve_target_argument_with_spec,
    QueueTargetFindContextInput,
};
use super::target_support::requester_environment_context;

const SOURCE_FILE_NESTING_LIMIT: usize = 50;
pub(in crate::handler) const CONTROL_QUEUE_INSERTED_COMMAND_LIMIT: usize = 1024;
pub(in crate::handler) const CONTROL_QUEUE_STDOUT_LIMIT: usize = 4 * 1024 * 1024;
// Command-mode nesting is queue-driven; this bounds hostile expansion without
// rejecting the deep chains accepted by the tmux oracle.
pub(in crate::handler) const RUN_SHELL_COMMAND_NESTING_LIMIT: usize = 1024;

tokio::task_local! {
    static QUEUED_COMMAND_CONTEXT: QueueExecutionContext;
    static QUEUED_DISPLAY_TARGET_CLIENT: QueuedDisplayTargetClient;
}

#[derive(Clone)]
pub(in crate::handler) enum QueuedDisplayTargetClient {
    Missing,
    Attached(ActiveAttachIdentity),
    Control(ControlClientIdentity),
    ResolutionError(RmuxError),
}

pub(in crate::handler) fn queued_display_target_client() -> Option<QueuedDisplayTargetClient> {
    QUEUED_DISPLAY_TARGET_CLIENT.try_with(Clone::clone).ok()
}

async fn with_queued_display_target_client<T, F>(
    target_client: Option<QueuedDisplayTargetClient>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    match target_client {
        Some(target_client) => {
            QUEUED_DISPLAY_TARGET_CLIENT
                .scope(target_client, future)
                .await
        }
        None => future.await,
    }
}

impl RequestHandler {
    async fn capture_queued_display_target_client(
        &self,
        requester_pid: u32,
        invocation: &mut QueueInvocation,
    ) -> Option<QueuedDisplayTargetClient> {
        let target_client = match invocation {
            QueueInvocation::Request(Request::DisplayPanes(request)) => {
                return self
                    .capture_parsed_display_panes_client(requester_pid, request)
                    .await;
            }
            QueueInvocation::Request(Request::DisplayMessage(_)) => None,
            QueueInvocation::Request(Request::DisplayMessageExt(request)) => {
                request.target_client.as_deref()
            }
            _ => return None,
        };
        if target_client.is_none() {
            let control_session = self.control_session_name(requester_pid).await;
            return current_expected_attach_identity()
                .map(QueuedDisplayTargetClient::Attached)
                .or_else(|| {
                    current_control_queue_identity(requester_pid)
                        .filter(|_| control_session.is_some())
                        .map(QueuedDisplayTargetClient::Control)
                });
        }
        Some(
            match self
                .find_named_client(
                    requester_pid,
                    target_client.expect("target client was checked above"),
                    "display-message",
                )
                .await
            {
                Ok(Some(ManagedClient::Attach { pid, attach_id })) => {
                    let active_attach = self.active_attach.lock().await;
                    match active_attach
                        .by_pid
                        .get(&pid)
                        .filter(|active| active.id == attach_id)
                    {
                        Some(active) => QueuedDisplayTargetClient::Attached(active.identity(pid)),
                        None => QueuedDisplayTargetClient::Missing,
                    }
                }
                Ok(Some(ManagedClient::Control(identity))) => {
                    QueuedDisplayTargetClient::Control(identity)
                }
                Ok(None) => QueuedDisplayTargetClient::Missing,
                Err(error) => QueuedDisplayTargetClient::ResolutionError(error),
            },
        )
    }
}

pub(in crate::handler) fn queued_command_has_mouse_origin() -> bool {
    queued_command_mouse_event().is_some()
}

pub(in crate::handler) fn queued_command_mouse_event() -> Option<AttachedMouseEvent> {
    queued_command_context().and_then(|context| context.mouse_event)
}

pub(in crate::handler) fn queued_command_exposes_hook_arguments() -> bool {
    QUEUED_COMMAND_CONTEXT
        .try_with(QueueExecutionContext::exposes_hook_arguments)
        .unwrap_or(true)
}

pub(in crate::handler) fn queued_command_context() -> Option<QueueExecutionContext> {
    QUEUED_COMMAND_CONTEXT.try_with(Clone::clone).ok()
}

struct QueuedCommandExecution {
    action: QueueCommandAction,
    response_frame_completed: bool,
    stdout_after_frame: bool,
    error_after_frame: bool,
    attached_switch_target: Option<Target>,
    current_session_transition: Option<QueuedCurrentSessionTransition>,
    session_rename: Option<QueuedSessionRename>,
}

mod assignment_admission;
mod command_admission;
mod command_entry;
mod command_execution;
mod control_outcome;
mod diagnostics;
mod prompt_binding;
mod queue_execution;
mod queue_output;
#[cfg(test)]
#[path = "../handler_scripting/send_keys_admission_probe.rs"]
pub(crate) mod send_keys_admission_probe;
use diagnostics::{
    aggregate_rmux_errors, command_parse_error_to_rmux, control_command_frame_outcome,
    parsed_command_count, source_file_context_error,
};

fn queue_invocation_allowed_for_read_only(invocation: &QueueInvocation) -> bool {
    matches!(
        invocation,
        QueueInvocation::Request(_)
            | QueueInvocation::QuietOnlyIfUnset(_)
            | QueueInvocation::RunShell(_)
            | QueueInvocation::NoOp
            | QueueInvocation::StartServer
            | QueueInvocation::ListCommands(_)
            | QueueInvocation::NewWindow(_)
            | QueueInvocation::ListPanesAll(_)
            | QueueInvocation::ListWindowsAll(_)
            | QueueInvocation::SplitWindow(_)
    )
}

fn apply_queue_context_to_request(
    mut request: Request,
    context: &QueueExecutionContext,
    run_shell_target_missing_canfail: bool,
) -> Request {
    match &mut request {
        Request::RunShell(run_shell) => {
            let follows_attached_registration = run_shell.background
                && current_expected_attach_identity().is_some()
                && !context.uses_explicit_current_target();
            if run_shell.target.is_none()
                && !run_shell_target_missing_canfail
                && !follows_attached_registration
            {
                if let Some(Target::Pane(target)) = context.current_target() {
                    run_shell.target = Some(target.clone());
                }
            }
            if context.source_file_depth != 0 {
                run_shell.source_depth = Some(context.source_file_depth);
            }
        }
        Request::CopyMode(copy_mode) if copy_mode.target.is_none() => {
            if let Some(Target::Pane(target)) = context.current_target() {
                copy_mode.target = Some(target.clone());
            }
        }
        _ => {}
    }
    request
}

#[cfg(test)]
#[path = "../handler_scripting/config_path_tests.rs"]
mod config_path_tests;

#[cfg(test)]
#[path = "../handler_scripting/control_queue_identity_tests.rs"]
mod control_queue_identity_tests;
