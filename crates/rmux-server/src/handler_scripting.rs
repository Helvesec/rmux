use rmux_core::{
    command_parser::{CommandArgument, CommandParseError, ParsedCommand, ParsedCommands},
    command_queue::CommandQueue,
    PaneGeometry, PaneId, ENVIRON_HIDDEN,
};
use rmux_proto::request::Request;
use rmux_proto::{
    CommandOutput, DisplayMessageRequest, HookName, PaneTarget, ResizePaneAdjustment,
    ResizePaneRequest, Response, RmuxError, ScopeSelector, Target,
};
use std::collections::VecDeque;
use std::future::Future;

use super::attach_support::ActiveAttachIdentity;
use super::client_support::capture_switch_client_target_identity;
use super::control_support::{
    control_queue_eof_action, current_control_queue_identity, with_control_queue_identity,
    ControlClientIdentity, ControlQueueEofAction,
};
#[cfg(windows)]
use super::pane_support::format_references_pane_pid;
use super::{
    active_session_target, current_expected_attach_identity, expected_attach_follows_registration,
    rebase_expected_attach_session_after_switch, validate_expected_attach_identity, RequestHandler,
};
use crate::control::ControlCommandResult;
use crate::hook_runtime::capture_inline_hooks;
use crate::mouse::{AttachedMouseEvent, MouseLocation};

#[path = "handler_scripting/buffer_parse.rs"]
mod buffer_parse;
#[path = "handler_scripting/client_parse.rs"]
mod client_parse;
#[path = "handler_scripting/command_args.rs"]
mod command_args;
#[path = "handler_scripting/config_engine/mod.rs"]
mod config_engine;
#[path = "handler_scripting/config_parse.rs"]
mod config_parse;
#[path = "handler_scripting/display_parse.rs"]
mod display_parse;
#[path = "handler_scripting/format_context.rs"]
mod format_context;
#[path = "handler_scripting/hook_commands.rs"]
mod hook_commands;
#[path = "handler_scripting/key_parse.rs"]
mod key_parse;
#[path = "handler_scripting/layout_parse.rs"]
mod layout_parse;
#[path = "handler_scripting/list_commands_runtime.rs"]
mod list_commands_runtime;
#[path = "handler_scripting/list_parse.rs"]
mod list_parse;
#[path = "handler_scripting/mode_parse.rs"]
mod mode_parse;
#[path = "handler_scripting/new_window_runtime.rs"]
mod new_window_runtime;
#[path = "handler_scripting/pane_parse.rs"]
mod pane_parse;
#[path = "handler_scripting/parser_context.rs"]
mod parser_context;
#[path = "handler_scripting/prompt_parse.rs"]
mod prompt_parse;
#[path = "handler_scripting/prompt_runtime.rs"]
mod prompt_runtime;
#[path = "handler_scripting/queue.rs"]
mod queue;
#[path = "handler_scripting/queue_exact_target.rs"]
mod queue_exact_target;
#[path = "handler_scripting/queue_lifecycle_target.rs"]
mod queue_lifecycle_target;
#[path = "handler_scripting/queue_parse.rs"]
mod queue_parse;
#[path = "handler_scripting/queue_special_target.rs"]
mod queue_special_target;
#[path = "handler_scripting/request_parse.rs"]
mod request_parse;
#[path = "handler_scripting/runtime.rs"]
mod runtime;
#[path = "handler_scripting/session_parse.rs"]
mod session_parse;
#[path = "handler_scripting/shell_parse.rs"]
mod shell_parse;
#[path = "handler_scripting/shell_runtime.rs"]
mod shell_runtime;
#[path = "handler_scripting/source_files.rs"]
mod source_files;
#[path = "handler_scripting/source_internal.rs"]
mod source_internal;
#[path = "handler_scripting/source_runtime.rs"]
mod source_runtime;
#[path = "handler_scripting/split_window_runtime.rs"]
mod split_window_runtime;
#[path = "handler_scripting/targets.rs"]
mod targets;
#[path = "handler_scripting/tmux_compat.rs"]
mod tmux_compat;
#[path = "handler_scripting/tokens.rs"]
mod tokens;
#[path = "handler_scripting/values.rs"]
mod values;
#[path = "handler_scripting/wait_for_runtime.rs"]
mod wait_for_runtime;
#[path = "handler_scripting/window_parse.rs"]
mod window_parse;

pub(super) use self::format_context::{
    format_context_for_target, format_context_for_target_with_server_values, global_format_context,
    render_start_directory_template,
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
use self::queue_exact_target::QueueExactTargetCapture;
#[cfg(test)]
pub(crate) use self::queue_exact_target::{
    install_queue_exact_target_capture_pause, QueueExactTargetCapturePause,
};
use self::queue_lifecycle_target::{QueueLifecycleTargetCapture, QueueLifecycleTargetPlan};
use self::queue_special_target::QueueSpecialTargetPlan;
use self::request_parse::parse_queue_invocation;
#[cfg(test)]
pub(crate) use self::request_parse::parse_request_from_parts;
use self::targets::{
    implicit_pane_target, implicit_session_name, implicit_split_target, implicit_window_target,
    marked_pane_target, marked_pane_target_or_current, parse_layout_name, parse_move_window_target,
    parse_new_window_target_argument, parse_pane_target, parse_select_layout_target,
    parse_session_name, parse_split_window_target, parse_target_arg, parse_window_target,
    queue_target_find_context, resolve_target_argument_with_spec, QueueTargetFindContextInput,
};
use super::target_support::requester_environment_pane_id;

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
        invocation: &QueueInvocation,
    ) -> Option<QueuedDisplayTargetClient> {
        let QueueInvocation::Request(Request::DisplayMessageExt(request)) = invocation else {
            return None;
        };
        let target_client = request.target_client.as_deref()?;
        Some(
            match self
                .find_target_attach_client_identity(requester_pid, target_client, "display-message")
                .await
            {
                Ok(Some(identity)) => QueuedDisplayTargetClient::Attached(identity),
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

pub(in crate::handler) fn queued_command_context() -> Option<QueueExecutionContext> {
    QUEUED_COMMAND_CONTEXT.try_with(Clone::clone).ok()
}

struct QueuedCommandExecution {
    action: QueueCommandAction,
    attached_switch_target: Option<Target>,
}

impl RequestHandler {
    pub(in crate::handler) async fn attached_queue_target_for_registration(
        &self,
        identity: ActiveAttachIdentity,
    ) -> Result<Target, RmuxError> {
        let state = self.state.lock().await;
        let active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                active.id == identity.attach_id()
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| {
                RmuxError::Server(
                    "attached client identity changed before background command execution"
                        .to_owned(),
                )
            })?;
        let session = state
            .sessions
            .session(&active.session_name)
            .filter(|session| session.id() == active.session_id)
            .ok_or_else(|| RmuxError::SessionNotFound(active.session_name.to_string()))?;
        active_session_target(&state.sessions, session.name())
            .ok_or_else(|| RmuxError::Server("attached session has no current target".to_owned()))
    }

    #[cfg(test)]
    pub(crate) async fn execute_parsed_commands_for_test(
        &self,
        requester_pid: u32,
        commands: ParsedCommands,
    ) -> Result<CommandOutput, RmuxError> {
        self.execute_parsed_commands(
            requester_pid,
            commands,
            QueueExecutionContext::without_caller_cwd(),
        )
        .await
    }

    pub(super) async fn parse_command_string_one_group(
        &self,
        command: &str,
    ) -> Result<ParsedCommands, RmuxError> {
        let state = self.state.lock().await;
        let parser = command_parser_from_state(&state);
        parser
            .parse_one_group(command)
            .map_err(command_parse_error_to_rmux)
    }

    pub(crate) async fn parse_control_commands(
        &self,
        command: &str,
    ) -> Result<ParsedCommands, RmuxError> {
        self.parse_command_string_one_group(command).await
    }

    #[async_recursion::async_recursion]
    pub(super) async fn execute_parsed_commands(
        &self,
        requester_pid: u32,
        commands: ParsedCommands,
        context: QueueExecutionContext,
    ) -> Result<CommandOutput, RmuxError> {
        let result = self
            .execute_command_queue(requester_pid, commands, context, QueueMode::Detached, None)
            .await;
        match result.error {
            Some(error) => Err(error),
            None => Ok(CommandOutput::from_stdout(result.stdout)),
        }
    }

    pub(crate) async fn execute_control_commands_identity(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        commands: ParsedCommands,
    ) -> ControlCommandResult {
        self.execute_command_queue(
            requester_pid,
            commands,
            QueueExecutionContext::without_caller_cwd(),
            QueueMode::Control,
            Some(expected_control_id),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn execute_control_commands(
        &self,
        requester_pid: u32,
        commands: ParsedCommands,
    ) -> ControlCommandResult {
        let expected_control_id = match self.control_queue_client_id(requester_pid).await {
            Ok(control_id) => control_id,
            Err(error) => {
                return ControlCommandResult {
                    stdout: Vec::new(),
                    error: Some(error.clone()),
                    source_file_error: None,
                    execution_error: Some(error),
                    exit_status: Some(1),
                    server_shutdown_started: false,
                };
            }
        };
        self.execute_control_commands_identity(requester_pid, expected_control_id, commands)
            .await
    }

    pub(in crate::handler) async fn start_attached_prompt_binding_commands(
        &self,
        requester_pid: u32,
        commands: &ParsedCommands,
        context: &QueueExecutionContext,
    ) -> Result<bool, RmuxError> {
        self.start_attached_prompt_binding_commands_with_identity(
            requester_pid,
            None,
            commands,
            context,
        )
        .await
    }

    pub(in crate::handler) async fn start_attached_prompt_binding_commands_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_name: rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        requester_pid: u32,
        commands: &ParsedCommands,
        context: &QueueExecutionContext,
    ) -> Result<bool, RmuxError> {
        self.start_attached_prompt_binding_commands_with_identity(
            requester_pid,
            Some((identity, session_name, session_id)),
            commands,
            context,
        )
        .await
    }

    async fn start_attached_prompt_binding_commands_with_identity(
        &self,
        requester_pid: u32,
        identity: Option<(
            ActiveAttachIdentity,
            rmux_proto::SessionName,
            rmux_proto::SessionId,
        )>,
        commands: &ParsedCommands,
        context: &QueueExecutionContext,
    ) -> Result<bool, RmuxError> {
        if commands.commands().len() != 1 {
            return Ok(false);
        }

        if identity
            .as_ref()
            .is_some_and(|(identity, _, _)| identity.attach_pid() != requester_pid)
        {
            return Err(RmuxError::Server(
                "attached prompt identity changed client".to_owned(),
            ));
        }

        self.apply_parse_time_assignments(requester_pid, commands, None)
            .await?;
        let command = commands
            .commands()
            .first()
            .expect("single command checked")
            .clone();
        let attached_session = match identity.as_ref() {
            Some((identity, session_name, _)) => {
                if !self.current_live_attach_input(*identity).await {
                    return Err(RmuxError::Server("attached client disappeared".to_owned()));
                }
                Some(session_name.clone())
            }
            None => self.current_session_candidate(requester_pid).await,
        };
        let socket_path = self.socket_path();
        let requester_pane_id = context
            .current_target
            .is_none()
            .then(|| requester_environment_pane_id(requester_pid, &socket_path))
            .flatten();
        let invocation = {
            let state = self.state.lock().await;
            if identity
                .as_ref()
                .is_some_and(|(_, session_name, session_id)| {
                    state
                        .sessions
                        .session(session_name)
                        .is_none_or(|session| session.id() != *session_id)
                })
            {
                return Err(RmuxError::Server("attached session disappeared".to_owned()));
            }
            let marked_target = state.marked_pane_target();
            let find_context = queue_target_find_context(QueueTargetFindContextInput {
                sessions: &state.sessions,
                options: &state.options,
                requester_pane_id,
                attached_session: attached_session.as_ref(),
                current_target: context.current_target.as_ref(),
                missing_current_target_fallback: context.missing_current_target_fallback(),
                mouse_target: context.mouse_target.as_ref(),
                marked_target: marked_target.as_ref(),
            });
            let run_shell_canfail_fallback_target = context.run_shell_canfail_fallback_target();
            parse_queue_invocation(
                command,
                context.caller_cwd.as_deref(),
                &state.sessions,
                &state.options,
                &find_context,
                context.canfail_fallback_target(),
                run_shell_canfail_fallback_target,
            )
        }?;

        match invocation {
            QueueInvocation::CommandPrompt(command) => {
                match identity {
                    Some((identity, session_name, session_id)) => {
                        self.start_attached_command_prompt_binding_for_identity(
                            identity,
                            session_name,
                            session_id,
                            requester_pid,
                            command,
                            context,
                        )
                        .await?;
                    }
                    None => {
                        self.start_attached_command_prompt_binding(requester_pid, command, context)
                            .await?;
                    }
                }
                Ok(true)
            }
            QueueInvocation::ConfirmBefore(command) => {
                match identity {
                    Some((identity, session_name, session_id)) => {
                        self.start_attached_confirm_before_binding_for_identity(
                            identity,
                            session_name,
                            session_id,
                            requester_pid,
                            command,
                            context,
                        )
                        .await?;
                    }
                    None => {
                        self.start_attached_confirm_before_binding(requester_pid, command, context)
                            .await?;
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    #[async_recursion::async_recursion]
    async fn execute_command_queue(
        &self,
        requester_pid: u32,
        commands: ParsedCommands,
        context: QueueExecutionContext,
        mode: QueueMode,
        expected_control_id: Option<u64>,
    ) -> ControlCommandResult {
        if let Err(error) = validate_expected_attach_identity(self, requester_pid).await {
            return ControlCommandResult {
                stdout: Vec::new(),
                error: Some(error.clone()),
                source_file_error: None,
                execution_error: Some(error),
                exit_status: Some(1),
                server_shutdown_started: false,
            };
        }
        let control_identity = expected_control_id
            .map(|control_id| ControlClientIdentity::new(requester_pid, control_id));
        if control_queue_eof_action(control_identity) == ControlQueueEofAction::StopFrame {
            return ControlCommandResult {
                stdout: Vec::new(),
                error: None,
                source_file_error: None,
                execution_error: None,
                exit_status: None,
                server_shutdown_started: false,
            };
        }
        if let Err(error) = self
            .apply_parse_time_assignments(requester_pid, &commands, expected_control_id)
            .await
        {
            return ControlCommandResult {
                stdout: Vec::new(),
                error: Some(error.clone()),
                source_file_error: None,
                execution_error: Some(error),
                exit_status: Some(1),
                server_shutdown_started: false,
            };
        }
        let mut queue = CommandQueue::from_parsed(commands);
        let mut contexts = VecDeque::from(vec![context; queue.len()]);
        let mut stdout = Vec::new();
        let mut errors = Vec::new();
        let mut source_file_errors = Vec::new();
        let mut execution_errors = Vec::new();
        let mut exit_status = None;
        let mut inserted_command_count = 0_usize;
        let mut server_shutdown_started = false;

        'command_queue: loop {
            if queue.is_empty() {
                break 'command_queue;
            }
            let expected_attach = match validate_expected_attach_identity(self, requester_pid).await
            {
                Ok(identity) => identity,
                Err(error) => {
                    execution_errors.push(error.clone());
                    errors.push(error);
                    exit_status = Some(1);
                    break 'command_queue;
                }
            };
            if expected_attach_follows_registration()
                && contexts
                    .iter()
                    .any(QueueExecutionContext::follows_attached_session)
            {
                let Some(identity) = expected_attach else {
                    let error = RmuxError::Server(
                        "background command lost its attached client identity".to_owned(),
                    );
                    execution_errors.push(error.clone());
                    errors.push(error);
                    exit_status = Some(1);
                    break 'command_queue;
                };
                let target = match self.attached_queue_target_for_registration(identity).await {
                    Ok(target) => target,
                    Err(error) => {
                        execution_errors.push(error.clone());
                        errors.push(error);
                        exit_status = Some(1);
                        break 'command_queue;
                    }
                };
                for context in &mut contexts {
                    if context.follows_attached_session() {
                        context.rebase_current_target_after_attached_switch(target.clone());
                    }
                }
            }
            if control_queue_eof_action(control_identity) == ControlQueueEofAction::StopFrame {
                break 'command_queue;
            }
            let Some(item) = queue.pop_front() else {
                break 'command_queue;
            };
            let item_context = contexts
                .pop_front()
                .expect("queue item context must stay aligned");
            let command_execution = self.execute_queued_command(
                requester_pid,
                item.command().clone(),
                &item_context,
                mode,
                expected_control_id,
            );
            let execute_and_check_eof = async {
                let command_action = command_execution.await;
                let eof_action = control_queue_eof_action(control_identity);
                (command_action, eof_action)
            };
            let (command_action, eof_action) = match expected_control_id {
                Some(_) => {
                    with_control_queue_identity(
                        control_identity.expect("control queues capture a client identity"),
                        execute_and_check_eof,
                    )
                    .await
                }
                None => execute_and_check_eof.await,
            };
            if eof_action == ControlQueueEofAction::StopFrame {
                break 'command_queue;
            }
            let command_action = command_action.map(|execution| {
                if let Some(target) = execution.attached_switch_target {
                    for context in &mut contexts {
                        context.rebase_current_target_after_attached_switch(target.clone());
                    }
                }
                execution.action
            });
            match command_action {
                Ok(QueueCommandAction::Normal {
                    output: Some(output),
                    error,
                    source_file_error,
                    exit_status: action_exit_status,
                }) => {
                    if let Err(error) = append_queue_stdout(&mut stdout, output.stdout(), mode) {
                        execution_errors.push(error.clone());
                        errors.push(error);
                        exit_status = Some(1);
                        break 'command_queue;
                    }
                    if let Some(status) = action_exit_status {
                        exit_status = Some(status);
                    }
                    if let Some(error) = source_file_error {
                        source_file_errors.push(error.clone());
                        errors.push(error);
                    }
                    if let Some(error) = error {
                        execution_errors.push(error.clone());
                        errors.push(error);
                    }
                }
                Ok(QueueCommandAction::Normal {
                    output: None,
                    error,
                    source_file_error,
                    exit_status: action_exit_status,
                }) => {
                    if let Some(status) = action_exit_status {
                        exit_status = Some(status);
                    }
                    if let Some(error) = source_file_error {
                        source_file_errors.push(error.clone());
                        errors.push(error);
                    }
                    if let Some(error) = error {
                        execution_errors.push(error.clone());
                        errors.push(error);
                    }
                }
                Ok(QueueCommandAction::InsertAfter {
                    batches,
                    output,
                    error,
                    source_file_error,
                    exit_status: action_exit_status,
                }) => {
                    if let Err(error) = validate_expected_attach_identity(self, requester_pid).await
                    {
                        execution_errors.push(error.clone());
                        errors.push(error);
                        exit_status = Some(1);
                        break 'command_queue;
                    }
                    if let Some(output) = output {
                        if let Err(error) = append_queue_stdout(&mut stdout, output.stdout(), mode)
                        {
                            execution_errors.push(error.clone());
                            errors.push(error);
                            exit_status = Some(1);
                            break 'command_queue;
                        }
                    }
                    if let Some(status) = action_exit_status {
                        exit_status = Some(status);
                    }
                    if let Some(error) = source_file_error {
                        source_file_errors.push(error.clone());
                        errors.push(error);
                    }
                    if let Some(error) = error {
                        execution_errors.push(error.clone());
                        errors.push(error);
                    }
                    let inserted = batches.iter().fold(0_usize, |count, (commands, _)| {
                        count.saturating_add(parsed_command_count(commands))
                    });
                    let next_inserted_count = inserted_command_count.saturating_add(inserted);
                    if mode == QueueMode::Control
                        && next_inserted_count > CONTROL_QUEUE_INSERTED_COMMAND_LIMIT
                    {
                        let error = RmuxError::Server(format!(
                            "control command queue inserted too many commands: {next_inserted_count} (maximum {CONTROL_QUEUE_INSERTED_COMMAND_LIMIT})"
                        ));
                        execution_errors.push(error.clone());
                        errors.push(error);
                        exit_status = Some(1);
                        break 'command_queue;
                    }
                    inserted_command_count = next_inserted_count;
                    let mut insertable_batches = Vec::with_capacity(batches.len());
                    for (commands, context) in batches {
                        if let Err(error) = self
                            .apply_parse_time_assignments(
                                requester_pid,
                                &commands,
                                expected_control_id,
                            )
                            .await
                        {
                            execution_errors.push(error.clone());
                            errors.push(error);
                            exit_status = Some(1);
                            continue;
                        }
                        insertable_batches.push((commands, context));
                    }
                    for (commands, context) in insertable_batches.into_iter().rev() {
                        let inserted = commands.commands().len();
                        queue.insert_after_current(commands);
                        for _ in 0..inserted {
                            contexts.push_front(context.clone());
                        }
                    }
                }
                Err(error) => {
                    execution_errors.push(error.clone());
                    errors.push(error);
                    remove_group_contexts(&queue, &mut contexts, item.group());
                    queue.remove_group(item.group());
                }
            }
            if item_context.source_file_depth > 0
                && !item_context.uses_explicit_current_target()
                && !contexts.is_empty()
            {
                let updated_target = self
                    .implicit_source_file_target(requester_pid)
                    .await
                    .map(Target::Pane);
                for context in &mut contexts {
                    if context.source_file_depth == item_context.source_file_depth
                        && !context.uses_explicit_current_target()
                    {
                        *context = context
                            .clone()
                            .with_implicit_current_target(updated_target.clone());
                    }
                }
            }
            if self.request_shutdown_if_pending() {
                server_shutdown_started = true;
                break 'command_queue;
            }
        }

        ControlCommandResult {
            stdout,
            error: aggregate_rmux_errors(errors),
            source_file_error: aggregate_rmux_errors(source_file_errors),
            execution_error: aggregate_rmux_errors(execution_errors),
            exit_status,
            server_shutdown_started,
        }
    }

    async fn execute_queued_command(
        &self,
        requester_pid: u32,
        command: ParsedCommand,
        context: &QueueExecutionContext,
        mode: QueueMode,
        expected_control_id: Option<u64>,
    ) -> Result<QueuedCommandExecution, RmuxError> {
        QUEUED_COMMAND_CONTEXT
            .scope(
                context.clone(),
                self.execute_queued_command_scoped(
                    requester_pid,
                    command,
                    context,
                    mode,
                    expected_control_id,
                ),
            )
            .await
    }

    #[async_recursion::async_recursion]
    async fn execute_queued_command_scoped(
        &self,
        requester_pid: u32,
        command: ParsedCommand,
        context: &QueueExecutionContext,
        mode: QueueMode,
        expected_control_id: Option<u64>,
    ) -> Result<QueuedCommandExecution, RmuxError> {
        let command_for_hooks = command.clone();
        if mode == QueueMode::Control {
            self.validate_control_queue_session_identity(
                requester_pid,
                expected_control_id.expect("control queues capture a client identity"),
            )
            .await?;
        }
        let attached_session = self.current_session_candidate(requester_pid).await;
        let socket_path = self.socket_path();
        let requester_pane_id = context
            .current_target
            .is_none()
            .then(|| requester_environment_pane_id(requester_pid, &socket_path))
            .flatten();
        let invocation = {
            let mut state = self.state.lock().await;
            (|| -> Result<_, RmuxError> {
                context.require_pinned_current_target(&state)?;
                let retained_target = context.retained_lifecycle_target().cloned();
                let lifecycle_plan = retained_target
                    .as_ref()
                    .map(|_| QueueLifecycleTargetPlan::for_command(&command_for_hooks))
                    .transpose()?
                    .flatten();
                let special_target_plan = QueueSpecialTargetPlan::for_command(&command_for_hooks)?;
                let retained_parse_target = match (&lifecycle_plan, retained_target.as_ref()) {
                    (Some(plan), Some(retained_target)) => {
                        plan.resolve_parse_target(retained_target, &state)?
                    }
                    _ => None,
                };
                let special_retained_parse_target = match special_target_plan.as_ref() {
                    Some(plan) => plan.resolve_parse_target(retained_target.as_ref(), &state)?,
                    None => None,
                };
                let current_target = if lifecycle_plan.is_some() {
                    retained_parse_target.as_ref()
                } else if special_retained_parse_target.is_some() {
                    special_retained_parse_target.as_ref()
                } else {
                    context.current_target.as_ref()
                };
                let marked_target = state.marked_pane_target();
                let find_context = queue_target_find_context(QueueTargetFindContextInput {
                    sessions: &state.sessions,
                    options: &state.options,
                    requester_pane_id,
                    attached_session: attached_session.as_ref(),
                    current_target,
                    missing_current_target_fallback: context.missing_current_target_fallback(),
                    mouse_target: context.mouse_target.as_ref(),
                    marked_target: marked_target.as_ref(),
                });
                let run_shell_canfail_fallback_target =
                    context.run_shell_canfail_fallback_target().or_else(|| {
                        (mode == QueueMode::Control)
                            .then(|| find_context.current())
                            .flatten()
                    });
                let queue_current_target = if lifecycle_plan.is_some() {
                    retained_parse_target
                        .as_ref()
                        .or(special_retained_parse_target.as_ref())
                } else {
                    special_retained_parse_target
                        .as_ref()
                        .or(retained_parse_target.as_ref())
                        .or(context.canfail_fallback_target())
                };
                let mut invocation = parse_queue_invocation(
                    command,
                    context.caller_cwd.as_deref(),
                    &state.sessions,
                    &state.options,
                    &find_context,
                    queue_current_target,
                    run_shell_canfail_fallback_target,
                )?;
                if let Some(plan) = lifecycle_plan.as_ref() {
                    plan.bind_implicit_target(&mut invocation, &state.sessions, &find_context)?;
                }
                let special_target_pending = match special_target_plan {
                    Some(plan) => plan.bind(
                        &mut invocation,
                        &state,
                        &find_context,
                        retained_target.clone(),
                    )?,
                    None => None,
                };
                drop(find_context);
                let special_target_binding = special_target_pending
                    .map(|pending| pending.capture(&mut state))
                    .transpose()?
                    .map(Box::new);
                if let QueueInvocation::NewWindow(command) = &mut invocation {
                    let witness = new_window_runtime::QueuedNewWindowTargetWitness::capture(
                        &mut state, command,
                    )?;
                    command.target_witness = Some(Box::new(witness));
                }
                let exact_target =
                    QueueExactTargetCapture::capture(&command_for_hooks, &invocation, &mut state)
                        .into_identity()?;
                let lifecycle_target = match (lifecycle_plan, retained_target) {
                    (Some(plan), Some(retained_target)) => {
                        plan.capture(&invocation, &mut state, retained_target)?
                    }
                    _ => QueueLifecycleTargetCapture::default(),
                };
                let (mut target_identities, retained_lifecycle_target, retained_lifecycle_identity) =
                    lifecycle_target.into_parts();
                if let Some(exact_target) = exact_target {
                    target_identities.push(exact_target);
                }
                target_identities.dedup();
                Ok::<_, RmuxError>((
                    invocation,
                    target_identities,
                    retained_lifecycle_target,
                    retained_lifecycle_identity,
                    special_target_binding,
                ))
            })()
        };
        let (
            invocation,
            target_identities,
            retained_lifecycle_target,
            retained_lifecycle_identity,
            special_target_binding,
        ) = match invocation {
            Ok(invocation) => invocation,
            Err(error) => {
                self.run_command_error_hook_for_parsed_command(
                    requester_pid,
                    &command_for_hooks,
                    context.current_target.clone(),
                    attached_session.as_ref(),
                )
                .await;
                return Err(source_file_context_error(
                    error,
                    &command_for_hooks,
                    context,
                ));
            }
        };
        let queued_display_target_client = self
            .capture_queued_display_target_client(requester_pid, &invocation)
            .await;
        #[cfg(test)]
        queue_exact_target::pause_after_queue_exact_target_capture(self, command_for_hooks.name())
            .await;
        let can_write = self.requester_can_write(requester_pid).await;
        if !can_write && !queue_invocation_allowed_for_read_only(&invocation) {
            return Err(RmuxError::Server("client is read-only".to_owned()));
        }
        let request_invocation = match &invocation {
            QueueInvocation::RunShell(command) => {
                !command.request.as_commands || command.request.background
            }
            QueueInvocation::Request(_)
            | QueueInvocation::NewWindow(_)
            | QueueInvocation::SplitWindow(_) => true,
            _ => false,
        };
        let queued_success_hook = match &invocation {
            QueueInvocation::ListWindowsAll(command) => Some((
                HookName::AfterListWindows,
                command.hook_target.clone().map(Target::Session),
            )),
            _ => None,
        };
        let hook_current_target = queued_success_hook
            .as_ref()
            .and_then(|(_, target)| target.clone())
            .or_else(|| context.current_target.clone());

        let mut attached_switch_target = None;
        let result = match invocation {
            QueueInvocation::NoOp => Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            }),
            QueueInvocation::Request(request) => {
                let explicit_target_run_shell = match &request {
                    Request::RunShell(request) if !request.as_commands => request.target.clone(),
                    _ => None,
                };
                let explicit_target_run_shell = match explicit_target_run_shell {
                    Some(target) => self
                        .pane_id_for_slot_target(&target)
                        .await
                        .map(|pane_id| (target, pane_id)),
                    None => None,
                };
                let request = apply_queue_context_to_request(request, context, false);
                let request = crate::server_access::apply_access_policy(request, can_write)?;
                let request_for_hooks = request.clone();
                let captures_client_transition = captures_attached_client_transition(&request);
                let dispatch = Box::pin(with_queued_display_target_client(
                    queued_display_target_client,
                    super::with_expected_stable_target_identities(
                        target_identities,
                        retained_lifecycle_target,
                        retained_lifecycle_identity,
                        self.dispatch_captured_with_client_name(
                            requester_pid,
                            u64::from(requester_pid),
                            request,
                            context.client_name.clone(),
                        ),
                    ),
                ));
                let ((outcome, inline_hooks), switch_client_capture) = if captures_client_transition
                {
                    capture_switch_client_target_identity(dispatch).await
                } else {
                    (dispatch.await, Default::default())
                };
                if captures_client_transition {
                    if let Response::SwitchClient(response) = &outcome.response {
                        let targeted_client =
                            switch_client_capture.targeted_client.ok_or_else(|| {
                                RmuxError::Server(
                                    "switch-client response omitted its targeted client identity"
                                        .to_owned(),
                                )
                            })?;
                        attached_switch_target = rebase_expected_attach_session_after_switch(
                            self,
                            requester_pid,
                            targeted_client,
                            &response.session_name,
                            switch_client_capture.committed_target,
                        )
                        .await?;
                    }
                }
                let targeted_output_delivered =
                    if let Some((target, pane_id)) = explicit_target_run_shell.as_ref() {
                        self.deliver_targeted_run_shell_output(
                            requester_pid,
                            target,
                            *pane_id,
                            &outcome.response,
                        )
                        .await
                    } else {
                        false
                    };
                let inline_hook_names = inline_hooks
                    .iter()
                    .map(|pending| pending.hook)
                    .collect::<Vec<_>>();
                self.run_inline_hooks(requester_pid, inline_hooks, Some(&command_for_hooks))
                    .await;
                self.run_request_hooks(
                    requester_pid,
                    &request_for_hooks,
                    &outcome.response,
                    Some(&command_for_hooks),
                    &inline_hook_names,
                )
                .await;
                let action = match mode {
                    QueueMode::Detached => queue_action_from_response(outcome.response),
                    QueueMode::Control => {
                        self.control_queue_action_from_outcome(
                            requester_pid,
                            expected_control_id.expect("control queues capture a client identity"),
                            request_for_hooks,
                            outcome,
                        )
                        .await
                    }
                };
                if targeted_output_delivered {
                    action.map(QueueCommandAction::without_output)
                } else {
                    action
                }
            }
            QueueInvocation::RunShell(command) => {
                let target_missing_canfail = command.target_missing_canfail;
                let inserts_commands = command.request.as_commands && !command.request.background;
                let explicit_target_run_shell = command.request.target.clone();
                let request = Request::RunShell(Box::new(command.request));
                let request =
                    apply_queue_context_to_request(request, context, target_missing_canfail);
                let request = crate::server_access::apply_access_policy(request, can_write)?;
                let Request::RunShell(request) = request else {
                    return Err(RmuxError::Server(
                        "queued run-shell lost its request payload".to_owned(),
                    ));
                };
                let client_name = context.client_name.clone();
                if inserts_commands {
                    self.execute_queued_run_shell_commands(
                        requester_pid,
                        *request,
                        client_name,
                        target_missing_canfail,
                        context,
                        special_target_binding.as_deref(),
                    )
                    .await
                } else {
                    let explicit_target_run_shell = match explicit_target_run_shell {
                        Some(target) => self
                            .pane_id_for_slot_target(&target)
                            .await
                            .map(|pane_id| (target, pane_id)),
                        None => None,
                    };
                    let request_for_hooks = Request::RunShell(request.clone());
                    let (outcome, inline_hooks) = capture_inline_hooks(async {
                        crate::pane_io::HandleOutcome::response(
                            self.handle_queued_run_shell_with_client_name(
                                requester_pid,
                                *request,
                                client_name,
                                target_missing_canfail,
                                context,
                                special_target_binding.as_deref(),
                            )
                            .await,
                        )
                    })
                    .await;
                    let targeted_output_delivered =
                        if let Some((target, pane_id)) = explicit_target_run_shell.as_ref() {
                            self.deliver_targeted_run_shell_output(
                                requester_pid,
                                target,
                                *pane_id,
                                &outcome.response,
                            )
                            .await
                        } else {
                            false
                        };
                    let inline_hook_names = inline_hooks
                        .iter()
                        .map(|pending| pending.hook)
                        .collect::<Vec<_>>();
                    self.run_inline_hooks(requester_pid, inline_hooks, Some(&command_for_hooks))
                        .await;
                    self.run_request_hooks(
                        requester_pid,
                        &request_for_hooks,
                        &outcome.response,
                        Some(&command_for_hooks),
                        &inline_hook_names,
                    )
                    .await;
                    let action = match mode {
                        QueueMode::Detached => queue_action_from_response(outcome.response),
                        QueueMode::Control => {
                            self.control_queue_action_from_outcome(
                                requester_pid,
                                expected_control_id
                                    .expect("control queues capture a client identity"),
                                request_for_hooks,
                                outcome,
                            )
                            .await
                        }
                    };
                    if targeted_output_delivered {
                        action.map(QueueCommandAction::without_output)
                    } else {
                        action
                    }
                }
            }
            QueueInvocation::StartServer => Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            }),
            QueueInvocation::ListCommands(command) => self.execute_queued_list_commands(command),
            QueueInvocation::NewWindow(command) => {
                super::with_expected_stable_target_identities(
                    target_identities,
                    retained_lifecycle_target,
                    retained_lifecycle_identity,
                    // Keep the transaction future out of every recursive queue frame.
                    Box::pin(self.execute_queued_new_window(requester_pid, command, context)),
                )
                .await
            }
            QueueInvocation::IfShell(command) => {
                self.execute_queued_if_shell(
                    requester_pid,
                    command,
                    context,
                    special_target_binding.as_deref(),
                )
                .await
            }
            QueueInvocation::SourceFile(command) => {
                self.execute_queued_source_file(
                    requester_pid,
                    command,
                    context,
                    special_target_binding.as_deref(),
                )
                .await
            }
            QueueInvocation::ListPanesAll(command) => {
                self.execute_queued_list_panes_all(command).await
            }
            QueueInvocation::ListWindowsAll(command) => {
                self.execute_queued_list_windows_all(command).await
            }
            QueueInvocation::SplitWindow(command) => {
                self.execute_queued_split_window(
                    requester_pid,
                    &command_for_hooks,
                    command,
                    context,
                )
                .await
            }
            QueueInvocation::MouseResizePane(target) => {
                self.execute_queued_mouse_resize_pane(requester_pid, target, context)
                    .await
            }
            QueueInvocation::CommandPrompt(command) => {
                self.execute_queued_command_prompt(requester_pid, command, context)
                    .await
            }
            QueueInvocation::ConfirmBefore(command) => {
                self.execute_queued_confirm_before(requester_pid, command, context)
                    .await
            }
            QueueInvocation::ModeTree(command) => {
                self.execute_queued_mode_tree(requester_pid, command, context)
                    .await
            }
            QueueInvocation::Overlay(command) => {
                self.execute_queued_overlay(requester_pid, command, context)
                    .await
            }
            QueueInvocation::PromptHistory(command) => {
                self.execute_queued_prompt_history(command).await
            }
        };

        if result.is_ok() {
            if let Some((hook, _)) = queued_success_hook {
                self.run_command_success_hook_for_parsed_command(
                    requester_pid,
                    hook,
                    &command_for_hooks,
                    hook_current_target.clone(),
                    attached_session.as_ref(),
                )
                .await;
            }
        }

        if result.is_err() && !request_invocation {
            self.run_command_error_hook_for_parsed_command(
                requester_pid,
                &command_for_hooks,
                hook_current_target,
                attached_session.as_ref(),
            )
            .await;
        }

        result
            .map(|action| QueuedCommandExecution {
                action,
                attached_switch_target,
            })
            .map_err(|error| source_file_context_error(error, &command_for_hooks, context))
    }

    async fn deliver_targeted_run_shell_output(
        &self,
        requester_pid: u32,
        target: &PaneTarget,
        pane_id: PaneId,
        response: &Response,
    ) -> bool {
        let Response::RunShell(response) = response else {
            return false;
        };
        let Some(output) = response.command_output() else {
            return self
                .current_target_for_stable_pane(pane_id, Some(target.session_name()))
                .await
                .is_some();
        };
        let message = String::from_utf8_lossy(output.stdout())
            .trim_end_matches(['\r', '\n'])
            .replace('#', "##");
        if message.is_empty() {
            return true;
        }
        matches!(
            self.handle_display_message_for_stable_pane(
                requester_pid,
                pane_id,
                DisplayMessageRequest {
                    target: Some(Target::Pane(target.clone())),
                    print: false,
                    message: Some(message),
                    empty_target_context: false,
                },
            )
            .await,
            Response::DisplayMessage(_)
        )
    }

    async fn pane_id_for_slot_target(&self, target: &PaneTarget) -> Option<PaneId> {
        let state = self.state.lock().await;
        state
            .sessions
            .session(target.session_name())
            .and_then(|session| {
                session.pane_id_in_window(target.window_index(), target.pane_index())
            })
    }

    async fn apply_parse_time_assignments(
        &self,
        requester_pid: u32,
        commands: &ParsedCommands,
        expected_control_id: Option<u64>,
    ) -> Result<(), RmuxError> {
        if commands.assignments().is_empty() {
            return Ok(());
        }

        let expected_control_id = expected_control_id.or_else(|| {
            current_control_queue_identity(requester_pid).map(ControlClientIdentity::control_id)
        });
        let (mut state, _active_control) = if let Some(control_id) = expected_control_id {
            let state = self.state.lock().await;
            let active_control = self.active_control.lock().await;
            RequestHandler::validate_control_queue_identity_locked(
                &state,
                &active_control,
                requester_pid,
                control_id,
            )?;
            if !active_control
                .by_pid
                .get(&requester_pid)
                .expect("validated control client remains registered while locked")
                .can_write
            {
                return Err(RmuxError::Server("client is read-only".to_owned()));
            }
            (state, Some(active_control))
        } else {
            if !self.requester_can_write(requester_pid).await {
                return Err(RmuxError::Server("client is read-only".to_owned()));
            }
            (self.state.lock().await, None)
        };
        for assignment in commands.assignments() {
            state.environment.set_with_flags(
                ScopeSelector::Global,
                assignment.name().to_owned(),
                assignment.value().to_owned(),
                if assignment.hidden() {
                    ENVIRON_HIDDEN
                } else {
                    0
                },
            );
        }
        Ok(())
    }

    async fn execute_queued_list_panes_all(
        &self,
        command: self::list_parse::ParsedListPanesAllCommand,
    ) -> Result<QueueCommandAction, RmuxError> {
        let mut session_names = {
            let state = self.state.lock().await;
            state
                .sessions
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        };
        session_names.sort_by_key(ToString::to_string);

        let mut stdout = Vec::new();
        for session_name in session_names {
            let response = self
                .handle_list_panes(rmux_proto::ListPanesRequest {
                    target: session_name,
                    target_window_index: None,
                    format: command.format.clone(),
                    filter: command.filter.clone(),
                    sort_order: command.sort_order.clone(),
                    reversed: command.reversed,
                })
                .await;
            let action = queue_action_from_response(response)?;
            if let QueueCommandAction::Normal {
                output: Some(output),
                error,
                source_file_error: _,
                exit_status: _,
            } = action
            {
                stdout.extend_from_slice(output.stdout());
                if let Some(error) = error {
                    return Err(error);
                }
            }
        }

        Ok(QueueCommandAction::Normal {
            output: Some(CommandOutput::from_stdout(stdout)),
            error: None,
            source_file_error: None,
            exit_status: None,
        })
    }

    async fn execute_queued_list_windows_all(
        &self,
        command: self::list_parse::ParsedListWindowsAllCommand,
    ) -> Result<QueueCommandAction, RmuxError> {
        #[cfg(windows)]
        if format_references_pane_pid(command.format.as_deref())
            || format_references_pane_pid(command.filter.as_deref())
        {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
        let session_names = {
            let state = self.state.lock().await;
            state
                .sessions
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        };
        let mut attached_counts = std::collections::HashMap::new();
        for session_name in session_names {
            attached_counts.insert(
                session_name.clone(),
                self.attached_count(&session_name).await,
            );
        }
        let response = {
            let state = self.state.lock().await;
            state.list_windows_all(crate::pane_terminals::ListWindowsAllSelection {
                socket_path: &self.socket_path(),
                format: command.format.as_deref(),
                attached_counts: &attached_counts,
                filter: command.filter.as_deref(),
                sort_order: command.sort_order.as_deref(),
                reversed: command.reversed,
            })?
        };
        queue_action_from_response(Response::ListWindows(response))
    }

    async fn execute_queued_mouse_resize_pane(
        &self,
        requester_pid: u32,
        target: PaneTarget,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        let fallback_event;
        let event = if let Some(event) = context.mouse_event.as_ref() {
            event
        } else if context.mouse_target.is_some() {
            fallback_event = {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&requester_pid)
                    .and_then(|active| active.mouse.current_event.clone())
            };
            let Some(event) = fallback_event.as_ref() else {
                return Ok(QueueCommandAction::Normal {
                    output: None,
                    error: None,
                    source_file_error: None,
                    exit_status: None,
                });
            };
            event
        } else {
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        };
        if event.location != MouseLocation::Border {
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        }

        let adjustment = {
            let state = self.state.lock().await;
            state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .and_then(|window| window.pane(target.pane_index()))
                .map(|pane| mouse_resize_adjustment(pane.geometry(), event))
        }
        .unwrap_or(ResizePaneAdjustment::NoOp);

        if adjustment == ResizePaneAdjustment::NoOp {
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        }

        queue_action_from_response(
            self.handle_resize_pane(ResizePaneRequest { target, adjustment })
                .await,
        )
    }
}

fn queue_invocation_allowed_for_read_only(invocation: &QueueInvocation) -> bool {
    matches!(
        invocation,
        QueueInvocation::Request(_)
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

fn mouse_resize_adjustment(
    geometry: PaneGeometry,
    event: &AttachedMouseEvent,
) -> ResizePaneAdjustment {
    let x = event.raw.x;
    let y = adjusted_mouse_y(event);
    let start_x = event.raw.lx;
    let start_y = adjusted_mouse_y_value(event, event.raw.ly);
    let right_border = geometry.x().saturating_add(geometry.cols());
    let bottom_border = geometry.y().saturating_add(geometry.rows());
    let started_on_right_border = start_x == right_border
        && start_y >= geometry.y().saturating_sub(1)
        && start_y <= bottom_border;
    let started_on_bottom_border = start_y == bottom_border
        && start_x >= geometry.x().saturating_sub(1)
        && start_x <= right_border;

    let horizontal_delta = x.abs_diff(start_x);
    let vertical_delta = y.abs_diff(start_y);

    if started_on_right_border
        || (!started_on_bottom_border && horizontal_delta >= vertical_delta && horizontal_delta > 0)
    {
        return ResizePaneAdjustment::AbsoluteWidth {
            columns: x.saturating_sub(geometry.x()).max(1),
        };
    }
    if started_on_bottom_border || vertical_delta > 0 {
        return ResizePaneAdjustment::AbsoluteHeight {
            rows: y.saturating_sub(geometry.y()).max(1),
        };
    }

    ResizePaneAdjustment::NoOp
}

fn adjusted_mouse_y(event: &AttachedMouseEvent) -> u16 {
    adjusted_mouse_y_value(event, event.raw.y)
}

fn adjusted_mouse_y_value(event: &AttachedMouseEvent, y: u16) -> u16 {
    match event.status_at {
        Some(0) if y >= event.status_lines => y.saturating_sub(event.status_lines),
        _ => y,
    }
}

fn append_queue_stdout(
    stdout: &mut Vec<u8>,
    bytes: &[u8],
    mode: QueueMode,
) -> Result<(), RmuxError> {
    let next_len = stdout.len().saturating_add(bytes.len());
    if mode == QueueMode::Control && next_len > CONTROL_QUEUE_STDOUT_LIMIT {
        return Err(RmuxError::Server(format!(
            "control command stdout exceeds {CONTROL_QUEUE_STDOUT_LIMIT} bytes"
        )));
    }
    stdout.extend_from_slice(bytes);
    Ok(())
}

fn parsed_command_count(commands: &ParsedCommands) -> usize {
    commands.commands().iter().fold(0_usize, |count, command| {
        command
            .arguments()
            .iter()
            .fold(
                count.saturating_add(1),
                |nested_count, argument| match argument {
                    CommandArgument::Commands(nested) => {
                        nested_count.saturating_add(parsed_command_count(nested))
                    }
                    CommandArgument::String(_) => nested_count,
                },
            )
    })
}

fn aggregate_rmux_errors(errors: Vec<RmuxError>) -> Option<RmuxError> {
    match errors.len() {
        0 => None,
        1 => Some(errors.into_iter().next().expect("single error")),
        _ => Some(RmuxError::Server(
            errors
                .into_iter()
                .map(rmux_error_message)
                .collect::<Vec<_>>()
                .join("\n"),
        )),
    }
}

fn source_file_context_error(
    error: RmuxError,
    command: &ParsedCommand,
    context: &QueueExecutionContext,
) -> RmuxError {
    let Some(current_file) = context.current_file.as_deref() else {
        return error;
    };
    let include_line_prefix = source_file_error_uses_line_prefix(command.name(), &error);
    let message = source_file_command_error_message(command.name(), error);
    if !include_line_prefix || has_source_file_line_prefix(&message) {
        return RmuxError::Server(message);
    }
    RmuxError::Server(format!("{}:{}: {}", current_file, command.line(), message))
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

fn source_file_command_error_message(command_name: &str, error: RmuxError) -> String {
    let message = match &error {
        RmuxError::InvalidTarget { value, reason }
            if matches!(command_name, "link-window" | "move-window")
                && reason == "window index already exists in session" =>
        {
            window_index_from_target(value)
                .map(|index| format!("index in use: {index}"))
                .unwrap_or_else(|| rmux_error_message_ref(&error))
        }
        _ => rmux_error_message_ref(&error),
    };
    if let Some(flag) = message.strip_prefix(&format!("unsupported {command_name} flag: ")) {
        return format!("command {command_name}: unknown flag {flag}");
    }
    if let Some(flag) = unexpected_flag_argument_for_command(&message, command_name) {
        return format!("command {command_name}: unknown flag {flag}");
    }
    if let Some(flag) = unsupported_flag_for_command(&message, command_name) {
        return format!("command {command_name}: unknown flag {flag}");
    }
    message
}

fn source_file_error_uses_line_prefix(command_name: &str, error: &RmuxError) -> bool {
    if !matches!(command_name, "link-window" | "move-window") {
        return true;
    }
    match error {
        RmuxError::InvalidTarget { reason, .. } => {
            reason != "window index already exists in session"
        }
        RmuxError::Server(message) => !message.starts_with("index in use: "),
        _ => true,
    }
}

fn window_index_from_target(value: &str) -> Option<&str> {
    let (_, window) = value.split_once(':')?;
    let window = window.split_once('.').map_or(window, |(window, _)| window);
    (!window.is_empty() && window.bytes().all(|byte| byte.is_ascii_digit())).then_some(window)
}

fn unexpected_flag_argument_for_command<'a>(
    message: &'a str,
    command_name: &str,
) -> Option<&'a str> {
    let flag = message.strip_prefix("unexpected argument '")?;
    let suffix = format!("' for {command_name}");
    let flag = flag.strip_suffix(&suffix)?;
    flag.starts_with('-').then_some(flag)
}

fn unsupported_flag_for_command<'a>(message: &'a str, command_name: &str) -> Option<&'a str> {
    let flag = message.strip_prefix("unsupported flag '")?;
    let suffix = format!("' for {command_name}");
    flag.strip_suffix(&suffix)
}

fn has_source_file_line_prefix(message: &str) -> bool {
    let Some((_, rest)) = message.split_once(':') else {
        return false;
    };
    let Some((line, _)) = rest.split_once(':') else {
        return false;
    };
    !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit())
}

fn rmux_error_message(error: RmuxError) -> String {
    match error {
        RmuxError::Server(message) => message,
        other => other.to_string(),
    }
}

fn rmux_error_message_ref(error: &RmuxError) -> String {
    match error {
        RmuxError::Server(message) => message.clone(),
        other => other.to_string(),
    }
}

impl RequestHandler {
    async fn control_queue_action_from_outcome(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        request: Request,
        outcome: crate::pane_io::HandleOutcome,
    ) -> Result<QueueCommandAction, RmuxError> {
        let control_identity = ControlClientIdentity::new(requester_pid, expected_control_id);
        if let Some(attach) = outcome.attach {
            if matches!(
                request,
                Request::AttachSession(_)
                    | Request::AttachSessionExt(_)
                    | Request::AttachSessionExt2(_)
                    | Request::AttachSessionExt3(_)
            ) {
                let Response::AttachSession(response) = &outcome.response else {
                    return Err(RmuxError::Server(
                        "attach-session upgrade requires an attach-session response".to_owned(),
                    ));
                };
                let session_id = attach.session_id;
                self.attach_control_session_for_queue(
                    control_identity,
                    &response.session_name,
                    Some(session_id),
                )
                .await?;
                self.emit_client_attached_identity(
                    requester_pid,
                    response.session_name.clone(),
                    session_id,
                )
                .await;
            }
        }

        if matches!(request, Request::NewSession(_) | Request::NewSessionExt(_)) {
            if let Response::NewSession(response) = &outcome.response {
                if !response.detached {
                    self.validate_control_session_for_queue(control_identity)
                        .await?;
                }
            }
        }

        queue_action_from_response(outcome.response)
    }
}

fn command_parse_error_to_rmux(error: CommandParseError) -> RmuxError {
    RmuxError::Server(error.to_string())
}

#[cfg(test)]
#[path = "handler_scripting/config_path_tests.rs"]
mod config_path_tests;

#[cfg(test)]
#[path = "handler_scripting/control_queue_identity_tests.rs"]
mod control_queue_identity_tests;
