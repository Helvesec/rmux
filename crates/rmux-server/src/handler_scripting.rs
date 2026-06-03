use rmux_core::{
    command_parser::{CommandParseError, ParsedCommand, ParsedCommands},
    command_queue::CommandQueue,
    LifecycleEvent, ENVIRON_HIDDEN,
};
use rmux_proto::request::Request;
use rmux_proto::{CommandOutput, Response, RmuxError, ScopeSelector};
use std::collections::VecDeque;

use super::RequestHandler;
use crate::control::ControlCommandResult;

#[path = "handler_scripting/buffer_parse.rs"]
mod buffer_parse;
#[path = "handler_scripting/client_parse.rs"]
mod client_parse;
#[path = "handler_scripting/command_args.rs"]
mod command_args;
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
#[path = "handler_scripting/queue_parse.rs"]
mod queue_parse;
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

pub(super) use self::format_context::format_context_for_target;
pub(in crate::handler) use self::parser_context::command_parser_from_state;
pub(super) use self::prompt_parse::{ParsedPromptHistoryCommand, PromptHistoryAction};
use self::queue::{queue_action_from_response, remove_group_contexts, QueueInvocation, QueueMode};
pub(super) use self::queue::{QueueCommandAction, QueueExecutionContext};
use self::request_parse::parse_queue_invocation;
#[cfg(test)]
pub(crate) use self::request_parse::parse_request_from_parts;
pub(super) use self::runtime::spawn_background_async;
use self::targets::{
    implicit_pane_target, implicit_session_name, implicit_split_target, implicit_window_target,
    is_unsupported_named_layout, marked_pane_target, parse_layout_name, parse_move_window_target,
    parse_new_window_target_argument, parse_pane_target, parse_select_layout_target,
    parse_session_name, parse_split_window_target, parse_target_arg, parse_window_target,
    queue_target_find_context,
};

const SOURCE_FILE_NESTING_LIMIT: usize = 50;

impl RequestHandler {
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
            .execute_command_queue(requester_pid, commands, context, QueueMode::Detached)
            .await;
        match result.error {
            Some(error) => Err(error),
            None => Ok(CommandOutput::from_stdout(result.stdout)),
        }
    }

    pub(crate) async fn execute_control_commands(
        &self,
        requester_pid: u32,
        commands: ParsedCommands,
    ) -> ControlCommandResult {
        self.execute_command_queue(
            requester_pid,
            commands,
            QueueExecutionContext::without_caller_cwd(),
            QueueMode::Control,
        )
        .await
    }

    pub(in crate::handler) async fn start_attached_prompt_binding_commands(
        &self,
        requester_pid: u32,
        commands: &ParsedCommands,
        context: &QueueExecutionContext,
    ) -> Result<bool, RmuxError> {
        if commands.commands().len() != 1 {
            return Ok(false);
        }

        self.apply_parse_time_assignments(commands).await;
        let command = commands
            .commands()
            .first()
            .expect("single command checked")
            .clone();
        let attached_session = self.current_session_candidate(requester_pid).await;
        let invocation = {
            let state = self.state.lock().await;
            let marked_target = state.marked_pane_target();
            let find_context = queue_target_find_context(
                &state.sessions,
                &state.options,
                requester_pid,
                attached_session.as_ref(),
                context.current_target.as_ref(),
                context.mouse_target.as_ref(),
                marked_target.as_ref(),
            );
            parse_queue_invocation(
                command,
                context.caller_cwd.as_deref(),
                &state.sessions,
                &find_context,
            )
        }?;

        match invocation {
            QueueInvocation::CommandPrompt(command) => {
                self.start_attached_command_prompt_binding(requester_pid, command, context)
                    .await?;
                Ok(true)
            }
            QueueInvocation::ConfirmBefore(command) => {
                self.start_attached_confirm_before_binding(requester_pid, command, context)
                    .await?;
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
    ) -> ControlCommandResult {
        self.apply_parse_time_assignments(&commands).await;
        let mut queue = CommandQueue::from_parsed(commands);
        let mut contexts = VecDeque::from(vec![context; queue.len()]);
        let mut stdout = Vec::new();
        let mut errors = Vec::new();

        while let Some(item) = queue.pop_front() {
            let item_context = contexts
                .pop_front()
                .expect("queue item context must stay aligned");
            match self
                .execute_queued_command(requester_pid, item.command().clone(), &item_context, mode)
                .await
            {
                Ok(QueueCommandAction::Normal {
                    output: Some(output),
                    error,
                }) => {
                    stdout.extend_from_slice(output.stdout());
                    if let Some(error) = error {
                        errors.push(error);
                    }
                }
                Ok(QueueCommandAction::Normal {
                    output: None,
                    error,
                }) => {
                    if let Some(error) = error {
                        errors.push(error);
                    }
                }
                Ok(QueueCommandAction::InsertAfter {
                    batches,
                    output,
                    error,
                }) => {
                    if let Some(output) = output {
                        stdout.extend_from_slice(output.stdout());
                    }
                    if let Some(error) = error {
                        errors.push(error);
                    }
                    for (commands, context) in batches.into_iter().rev() {
                        self.apply_parse_time_assignments(&commands).await;
                        let inserted = commands.commands().len();
                        queue.insert_after_current(commands);
                        for _ in 0..inserted {
                            contexts.push_front(context.clone());
                        }
                    }
                }
                Err(error) => {
                    errors.push(error);
                    remove_group_contexts(&queue, &mut contexts, item.group());
                    queue.remove_group(item.group());
                }
            }
            let _ = self.request_shutdown_if_pending();
        }

        ControlCommandResult {
            stdout,
            error: aggregate_rmux_errors(errors),
        }
    }

    #[async_recursion::async_recursion]
    async fn execute_queued_command(
        &self,
        requester_pid: u32,
        command: ParsedCommand,
        context: &QueueExecutionContext,
        mode: QueueMode,
    ) -> Result<QueueCommandAction, RmuxError> {
        let command_for_hooks = command.clone();
        let attached_session = self.current_session_candidate(requester_pid).await;
        let invocation = {
            let state = self.state.lock().await;
            let marked_target = state.marked_pane_target();
            let find_context = queue_target_find_context(
                &state.sessions,
                &state.options,
                requester_pid,
                attached_session.as_ref(),
                context.current_target.as_ref(),
                context.mouse_target.as_ref(),
                marked_target.as_ref(),
            );
            parse_queue_invocation(
                command,
                context.caller_cwd.as_deref(),
                &state.sessions,
                &find_context,
            )
        };
        let invocation = match invocation {
            Ok(invocation) => invocation,
            Err(error) => {
                self.run_command_error_hook_for_parsed_command(
                    requester_pid,
                    &command_for_hooks,
                    context.current_target.clone(),
                    attached_session.as_ref(),
                )
                .await;
                return Err(error);
            }
        };
        let request_invocation = matches!(
            &invocation,
            QueueInvocation::Request(_)
                | QueueInvocation::NewWindow(_)
                | QueueInvocation::SplitWindow(_)
        );

        let result = match invocation {
            QueueInvocation::NoOp => Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
            }),
            QueueInvocation::Request(request) => {
                let can_write = self.requester_can_write(requester_pid).await;
                let request = crate::server_access::apply_access_policy(request, can_write)?;
                let request_for_hooks = request.clone();
                let (outcome, inline_hooks) = Box::pin(self.dispatch_captured(
                    requester_pid,
                    u64::from(requester_pid),
                    request,
                ))
                .await;
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
                match mode {
                    QueueMode::Detached => queue_action_from_response(outcome.response),
                    QueueMode::Control => {
                        self.control_queue_action_from_outcome(
                            requester_pid,
                            request_for_hooks,
                            outcome,
                        )
                        .await
                    }
                }
            }
            QueueInvocation::StartServer => Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
            }),
            QueueInvocation::NewWindow(command) => {
                self.execute_queued_new_window(requester_pid, command).await
            }
            QueueInvocation::IfShell(command) => {
                self.execute_queued_if_shell(requester_pid, command, context)
                    .await
            }
            QueueInvocation::SourceFile(command) => {
                self.execute_queued_source_file(requester_pid, command, context)
                    .await
            }
            QueueInvocation::ListPanesAll(command) => {
                self.execute_queued_list_panes_all(command).await
            }
            QueueInvocation::SplitWindow(command) => {
                self.execute_queued_split_window(requester_pid, &command_for_hooks, command)
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

        if result.is_err() && !request_invocation {
            self.run_command_error_hook_for_parsed_command(
                requester_pid,
                &command_for_hooks,
                context.current_target.clone(),
                attached_session.as_ref(),
            )
            .await;
        }

        result
    }

    async fn apply_parse_time_assignments(&self, commands: &ParsedCommands) {
        if commands.assignments().is_empty() {
            return;
        }

        let mut state = self.state.lock().await;
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
                })
                .await;
            let action = queue_action_from_response(response)?;
            if let QueueCommandAction::Normal {
                output: Some(output),
                error,
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
        })
    }
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

fn rmux_error_message(error: RmuxError) -> String {
    match error {
        RmuxError::Server(message) => message,
        other => other.to_string(),
    }
}

impl RequestHandler {
    async fn control_queue_action_from_outcome(
        &self,
        requester_pid: u32,
        request: Request,
        outcome: crate::pane_io::HandleOutcome,
    ) -> Result<QueueCommandAction, RmuxError> {
        if let Some(_attach) = outcome.attach {
            if matches!(
                request,
                Request::AttachSession(_) | Request::AttachSessionExt(_)
            ) {
                let Response::AttachSession(response) = &outcome.response else {
                    return Err(RmuxError::Server(
                        "attach-session upgrade requires an attach-session response".to_owned(),
                    ));
                };
                {
                    let mut state = self.state.lock().await;
                    if let Some(session) = state.sessions.session_mut(&response.session_name) {
                        session.touch_attached();
                    }
                }
                let _ = self
                    .set_control_session(requester_pid, Some(response.session_name.clone()))
                    .await?;
                self.emit_client_attached(requester_pid, response.session_name.clone())
                    .await;
            }
        }

        if matches!(request, Request::NewSession(_) | Request::NewSessionExt(_)) {
            if let Response::NewSession(response) = &outcome.response {
                if !response.detached
                    && self
                        .attach_control_to_existing_session(requester_pid, &response.session_name)
                        .await
                {
                    self.emit(LifecycleEvent::ClientSessionChanged {
                        session_name: response.session_name.clone(),
                        client_name: Some(requester_pid.to_string()),
                    })
                    .await;
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
