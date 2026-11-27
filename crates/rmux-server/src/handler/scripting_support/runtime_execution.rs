use rmux_core::{
    command_inventory::RMUX_EXTENSION_COMMANDS,
    command_parser::{CommandArgument, CommandParser, SOURCE_FILE_MAX_COMMAND_BYTES},
};
use rmux_proto::{
    HandshakeRequest, Request, Response, RmuxError, RuntimeCommandExecutionRequest,
    RuntimeCommandExecutionResponse, RuntimeCommandExecutionV2Request,
    CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION,
};
use std::path::PathBuf;
use std::time::Duration;

use super::runtime_admission::RuntimeAdmission;

use super::super::target_support::{
    requester_environment_context, with_requester_environment_memo,
};
use super::command_args::command_arguments_as_strings;
use super::list_parse::parse_queued_list_windows_all;
use super::parser_context::command_parser_from_state;
use super::queue::QueueInvocation;
use super::request_parse::parse_queue_invocation;
use super::targets::{
    command_has_independent_target, queue_target_find_context, QueueTargetFindContextInput,
};
use super::tokens::{normalize_compact_short_options, CommandTokens};
use super::{QueueExecutionContext, QueueMode, RequestHandler};
use crate::listener::{request_quiesce_behavior, RequestQuiesceBehavior};

impl RequestHandler {
    pub(in crate::handler) async fn handle_runtime_command_execution(
        &self,
        requester_pid: u32,
        offer: RuntimeCommandExecutionRequest,
    ) -> Response {
        let admission = RuntimeAdmission::legacy(&offer.handshake);
        let result = match with_requester_environment_memo(self.runtime_command_execution(
            requester_pid,
            offer.handshake,
            offer.arguments,
            offer.caller_cwd,
            admission,
        ))
        .await
        {
            Ok(result) => result,
            Err(error) => RuntimeCommandExecutionResponse::Unsupported(error),
        };
        Response::RuntimeCommandExecution(Box::new(result))
    }

    pub(in crate::handler) async fn handle_runtime_command_execution_v2(
        &self,
        requester_pid: u32,
        offer: RuntimeCommandExecutionV2Request,
    ) -> Response {
        let result = match with_requester_environment_memo(self.runtime_command_execution(
            requester_pid,
            offer.handshake,
            offer.arguments,
            offer.caller_cwd,
            RuntimeAdmission::Declared(offer.intent),
        ))
        .await
        {
            Ok(result) => result,
            Err(error) => RuntimeCommandExecutionResponse::Unsupported(error),
        };
        Response::RuntimeCommandExecution(Box::new(result))
    }

    async fn runtime_command_execution(
        &self,
        requester_pid: u32,
        handshake: HandshakeRequest,
        arguments: Vec<String>,
        caller_cwd: Option<PathBuf>,
        admission: RuntimeAdmission,
    ) -> Result<RuntimeCommandExecutionResponse, RmuxError> {
        handshake.validate_against(&super::super::dispatch_support::supported_capabilities())?;
        if !handshake
            .required_capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION)
        {
            return Err(RmuxError::Server(
                "missing runtime command execution capability".to_owned(),
            ));
        }
        admission.validate_support(&handshake)?;
        let explicit_offer = admission.explicit();
        let mut parsed = {
            let state = self.state.lock().await;
            let parser = command_parser_from_state(&state)
                .with_exact_commands(RMUX_EXTENSION_COMMANDS)
                .with_max_command_bytes(SOURCE_FILE_MAX_COMMAND_BYTES);
            match parser.parse_arguments_with_assignments(&arguments) {
                Ok(parsed) => parsed,
                // The helper owns CLI parse diagnostics and validation precedence.
                Err(error) if explicit_offer => {
                    return Err(super::command_parse_error_to_rmux(error))
                }
                Err(_) => return Ok(RuntimeCommandExecutionResponse::Fallback),
            }
        };
        let [command] = parsed.commands() else {
            if parsed
                .commands()
                .iter()
                .any(|command| command.name() == "new-pane")
            {
                return Err(RmuxError::Server(
                    "CLI batches containing new-pane are not admitted; no commands executed"
                        .to_owned(),
                ));
            }
            if parsed.commands().iter().any(resize_pane_requests_position) {
                return Err(RmuxError::Server(
                    "CLI batches containing resize-pane -X/-Y are not admitted; no commands executed"
                        .to_owned(),
                ));
            }
            if parsed
                .commands()
                .iter()
                .any(|command| command.name() == "set-pane-placement")
            {
                return Err(RmuxError::Server(
                    "CLI batches containing set-pane-placement are not admitted; no commands executed"
                        .to_owned(),
                ));
            }
            if admission.requires_single_command() {
                return Err(RmuxError::Server(
                    "declared runtime operation requires exactly one command".to_owned(),
                ));
            }
            return Ok(RuntimeCommandExecutionResponse::Fallback);
        };
        admission.validate_command(command)?;
        // Compound/assignment validation and terminal/startup/shutdown transitions
        // still belong to the CLI. Refuse before effects, including assignments.
        if !parsed.assignments().is_empty()
            || command
                .arguments()
                .iter()
                .any(|arg| matches!(arg, CommandArgument::Commands(_)))
            || matches!(
                command.name(),
                "new-session" | "attach-session" | "start-server" | "kill-server" | "source-file"
            )
        {
            if explicit_offer {
                return Err(RmuxError::Server(
                    "compound runtime mutation offer is not admitted".to_owned(),
                ));
            }
            return Ok(RuntimeCommandExecutionResponse::Fallback);
        }
        let command_name = command.name().to_owned();
        let mut context = QueueExecutionContext::new(caller_cwd);
        let invocation = self
            .runtime_cli_invocation(requester_pid, command.clone(), &context)
            .await;
        let entry = match invocation
            .and_then(|invocation| RuntimeCliEntry::for_invocation(command, invocation))
        {
            Ok(Some(entry)) => entry,
            Err(error) if explicit_offer => return Err(error),
            _ => return Ok(RuntimeCommandExecutionResponse::Fallback),
        };
        if entry.queued {
            if command_name == "list-windows" {
                // The full CLI's all-session route rebuilds this argv before
                // sending its internal source queue. Preserve both that order
                // and the literal values, without expanding the live alias twice.
                let arguments = list_windows_cli_queue_arguments(command)?;
                parsed = CommandParser::new()
                    .with_command_aliases(std::iter::empty::<String>())
                    .with_max_command_bytes(SOURCE_FILE_MAX_COMMAND_BYTES)
                    .parse_arguments_with_assignments(&arguments)
                    .map_err(super::command_parse_error_to_rmux)?;
            }
        } else {
            context = context.for_direct_cli();
        }
        if let Err(error) = self
            .validate_sourced_command_syntax(requester_pid, &parsed, &context, true, false, true)
            .await
        {
            if explicit_offer {
                return Err(error);
            }
            return Ok(RuntimeCommandExecutionResponse::Fallback);
        }
        let mut shutdown = self.normal_request_shutdown_receiver();
        crate::listener::runtime_entry::resolve(entry.quiesce);
        let execution = self.execute_command_queue(
            requester_pid,
            parsed,
            &mut context,
            QueueMode::Detached,
            None,
        );
        // The listener holds the offer's drain barrier until this resolves.
        // Apply only the resolved CLI entry's policy, never blanket-cancel an
        // admitted mutation. Dropping the queue also drops its owned shell tree.
        let result = tokio::select! {
            result = execution => result,
            () = wait_for_runtime_shutdown(&mut shutdown, entry.shutdown_grace()),
                if entry.shutdown_grace().is_some() => {
                return Ok(RuntimeCommandExecutionResponse::Complete {
                    command_name,
                    stdout: Vec::new(),
                    exit_status: 1,
                    error: Some(RmuxError::Server("command interrupted by server shutdown".to_owned())),
                });
            }
        };
        Ok(RuntimeCommandExecutionResponse::Complete {
            command_name,
            stdout: result.stdout,
            exit_status: result
                .exit_status
                .unwrap_or(i32::from(result.error.is_some())),
            error: result.error,
        })
    }

    pub(super) async fn runtime_cli_invocation(
        &self,
        requester_pid: u32,
        command: rmux_core::command_parser::ParsedCommand,
        context: &QueueExecutionContext,
    ) -> Result<QueueInvocation, RmuxError> {
        let attached_session = self.current_session_candidate(requester_pid).await;
        let requester_environment = if context.current_target.is_none()
            && !command_has_independent_target(&command, None)
        {
            requester_environment_context(requester_pid, &self.socket_path())
        } else {
            Default::default()
        };
        let state = self.state.lock().await;
        let marked_target = state.marked_pane_target();
        let requester_environment =
            if command_has_independent_target(&command, marked_target.as_ref()) {
                Default::default()
            } else {
                requester_environment
            };
        let find_context = queue_target_find_context(QueueTargetFindContextInput {
            sessions: &state.sessions,
            options: &state.options,
            requester_environment,
            attached_session: attached_session.as_ref(),
            requester_owns_attached_session: false,
            current_target: context.current_target.as_ref(),
            current_target_follows_client: context.current_target_follows_client(),
            missing_current_target_fallback: context.missing_current_target_fallback(),
            mouse_target: context.mouse_target.as_ref(),
            marked_target: marked_target.as_ref(),
        })?;
        parse_queue_invocation(
            command,
            context.caller_cwd.as_deref(),
            &state.sessions,
            &state.options,
            &find_context,
            context.canfail_fallback_target(),
            context.run_shell_canfail_fallback_target(),
        )
    }
}

fn list_windows_cli_queue_arguments(
    command: &rmux_core::command_parser::ParsedCommand,
) -> Result<Vec<String>, RmuxError> {
    let arguments = command_arguments_as_strings(command.name(), command.arguments())?;
    let command = parse_queued_list_windows_all(CommandTokens::new(arguments))?
        .ok_or_else(|| RmuxError::Server("expected list-windows -a queue entry".to_owned()))?;
    Ok(rmux_core::cli_entry::list_windows_all_queue_arguments(
        command.hook_target.as_ref().map(|target| target.as_str()),
        command.format.as_deref(),
        command.filter.as_deref(),
        command.sort_order.as_deref(),
        command.reversed,
    ))
}

#[derive(Clone, Copy)]
struct RuntimeCliEntry {
    queued: bool,
    quiesce: RequestQuiesceBehavior,
}

impl RuntimeCliEntry {
    fn for_invocation(
        command: &rmux_core::command_parser::ParsedCommand,
        invocation: QueueInvocation,
    ) -> Result<Option<Self>, RmuxError> {
        // Classify the parsed entry, not a second list of command names. Typed
        // requests share the listener's policy; source-queue owners retain
        // their parsed context. Conditional CLI owners are handled explicitly.
        let direct = |quiesce| {
            Some(Self {
                queued: false,
                quiesce,
            })
        };
        let queued = Some(Self {
            queued: true,
            quiesce: RequestQuiesceBehavior::CancelSafe,
        });
        Ok(match invocation {
            QueueInvocation::Request(request) | QueueInvocation::QuietOnlyIfUnset(request) => {
                if command.name() == "display-message"
                    && super::display_parse::display_message_uses_cli_queue(CommandTokens::new(
                        normalize_compact_short_options(
                            command.name(),
                            command_arguments_as_strings(command.name(), command.arguments())?,
                        ),
                    ))?
                {
                    queued
                } else {
                    direct(request_quiesce_behavior(&request))
                }
            }
            QueueInvocation::RunShell(shell) => {
                // Retain the typed CLI target boundary: relative targets need
                // the full client's resolution before its direct request.
                super::shell_parse::parse_run_shell(CommandTokens::new(
                    normalize_compact_short_options(
                        command.name(),
                        command_arguments_as_strings(command.name(), command.arguments())?,
                    ),
                ))?;
                direct(request_quiesce_behavior(&Request::RunShell(Box::new(
                    shell.request,
                ))))
            }
            // The full -k/-S owner first resolves and sends an explicit current
            // pane. The offer cannot substitute its implicit queue context.
            QueueInvocation::NewWindow(command)
                if command.kill_existing || command.select_existing =>
            {
                None
            }
            QueueInvocation::IfShell(_)
            | QueueInvocation::ListWindowsAll(_)
            | QueueInvocation::CommandPrompt(_)
            | QueueInvocation::ConfirmBefore(_)
            | QueueInvocation::ModeTree(_)
            | QueueInvocation::Overlay(_)
            | QueueInvocation::PromptHistory(_) => queued,
            QueueInvocation::RestoreWindowSnapshot(_)
            | QueueInvocation::NewWindow(_)
            | QueueInvocation::NewPane(_)
            | QueueInvocation::MoveFloatingPane(_)
            | QueueInvocation::SetPanePlacement(_)
            | QueueInvocation::SplitWindow(_)
            | QueueInvocation::MouseResizePane(_)
            | QueueInvocation::ListPanesAll(_)
            | QueueInvocation::ListCommands(_)
            | QueueInvocation::NoOp => direct(RequestQuiesceBehavior::Drain),
            QueueInvocation::SourceFile(_) | QueueInvocation::StartServer => None,
        })
    }

    fn shutdown_grace(self) -> Option<Duration> {
        match self.quiesce {
            RequestQuiesceBehavior::CancelSafe => Some(Duration::ZERO),
            RequestQuiesceBehavior::BoundedDrain => {
                Some(crate::listener::FOREGROUND_SHELL_SHUTDOWN_GRACE)
            }
            RequestQuiesceBehavior::Drain | RequestQuiesceBehavior::Upgrade => None,
        }
    }
}

pub(super) fn resize_pane_requests_position(
    command: &rmux_core::command_parser::ParsedCommand,
) -> bool {
    if command.name() != "resize-pane" {
        return false;
    }
    let Some(spec) = rmux_core::command_inventory::command_short_option_spec(command.name()) else {
        return false;
    };
    let mut index = 0;
    while index < command.arguments().len() {
        let Some(argument) = command.arguments()[index].as_string() else {
            index += 1;
            continue;
        };
        if argument == "--" || argument == "-" || !argument.starts_with('-') {
            break;
        }
        let Some(flags) = argument
            .strip_prefix('-')
            .filter(|flags| !flags.starts_with('-'))
        else {
            break;
        };
        let mut chars = flags.char_indices().peekable();
        while let Some((_, flag)) = chars.next() {
            if matches!(flag, 'X' | 'Y') {
                return true;
            }
            if spec.takes_value(flag) {
                if chars.peek().is_none()
                    && command
                        .arguments()
                        .get(index + 1)
                        .and_then(CommandArgument::as_string)
                        .is_some()
                {
                    index += 1;
                }
                break;
            }
            if !spec.is_boolean(flag) {
                break;
            }
        }
        index += 1;
    }
    false
}

async fn wait_for_runtime_shutdown(
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
    grace: Option<Duration>,
) {
    let _ = shutdown.wait_for(|closing| *closing).await;
    if let Some(grace) = grace {
        tokio::time::sleep(grace).await;
    }
}
