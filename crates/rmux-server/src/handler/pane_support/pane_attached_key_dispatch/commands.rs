use rmux_core::command_parser::{CommandArgument, ParsedCommand, ParsedCommands};
use rmux_proto::{
    CommandOutput, DisplayPanesRequest, ErrorResponse, Response, RmuxError, SessionId, SessionName,
    Target,
};
use tokio::task::JoinError;

use crate::handler::overlay_support::AttachedHelpContext;
use crate::hook_runtime::{current_hook_execution, current_hook_formats, with_hook_execution};
use crate::mouse::AttachedMouseEvent;

use super::super::super::{
    attach_support::{ActiveAttachIdentity, InputPathWork},
    scripting_support::QueueExecutionContext,
    with_expected_attach_and_session_identity, with_expected_session_identity, RequestHandler,
};

pub(super) struct AttachedBindingCommandContext {
    pub(super) attach_pid: u32,
    pub(super) live_identity: Option<ActiveAttachIdentity>,
    pub(super) requester_pid: u32,
    pub(super) session_name: SessionName,
    pub(super) session_id: SessionId,
    pub(super) client_name: String,
    pub(super) attached_live_input: bool,
    pub(super) dispatch_target: Target,
    pub(super) mouse_target: Option<Target>,
    pub(super) mouse_event: Option<AttachedMouseEvent>,
    pub(super) commands: ParsedCommands,
}

#[async_recursion::async_recursion]
pub(super) async fn execute_attached_binding_commands(
    handler: &RequestHandler,
    command_context: AttachedBindingCommandContext,
) -> Result<(), RmuxError> {
    let AttachedBindingCommandContext {
        attach_pid,
        live_identity,
        requester_pid,
        session_name,
        session_id,
        client_name,
        attached_live_input,
        dispatch_target,
        mouse_target,
        mouse_event,
        commands,
    } = command_context;

    if live_identity.is_some_and(|identity| identity.attach_pid() != attach_pid) {
        return Err(RmuxError::Server(
            "attached binding identity changed client".to_owned(),
        ));
    }

    let context = QueueExecutionContext::without_caller_cwd()
        .with_implicit_current_target(Some(dispatch_target.clone()))
        .with_run_shell_canfail_fallback_target()
        .with_client_name(Some(client_name.clone()))
        .with_mouse_target(mouse_target)
        .with_mouse_event(mouse_event);

    if attached_live_input && parsed_commands_are_plain_display_panes(&commands) {
        let request = DisplayPanesRequest {
            target: session_name.clone(),
            duration_ms: None,
            non_blocking: true,
            no_command: false,
            template: None,
            target_client: Some(client_name),
        };
        let response = match live_identity {
            Some(identity) => {
                handler
                    .handle_display_panes_for_identity(identity, requester_pid, request)
                    .await
            }
            None => {
                with_expected_session_identity(
                    session_name.clone(),
                    session_id,
                    handler.handle_display_panes(requester_pid, request),
                )
                .await
            }
        };
        match response {
            Response::DisplayPanes(_) => return Ok(()),
            Response::Error(ErrorResponse { error }) => {
                let identity_current = match live_identity {
                    Some(identity) => handler.current_live_attach_input(identity).await,
                    None => true,
                };
                if identity_current {
                    handler
                        .report_attached_command_error(&session_name, attach_pid, &error)
                        .await;
                }
                return Ok(());
            }
            other => {
                return Err(RmuxError::Server(format!(
                    "display-panes binding returned unexpected {} response",
                    other.command_name()
                )));
            }
        }
    }

    if parsed_commands_block_for_prompt(&commands) {
        let prompt_started = if attached_live_input {
            match live_identity {
                Some(identity) => {
                    handler
                        .start_attached_prompt_binding_commands_for_identity(
                            identity,
                            session_name.clone(),
                            session_id,
                            requester_pid,
                            &commands,
                            &context,
                        )
                        .await?
                }
                None => {
                    handler
                        .start_attached_prompt_binding_commands(requester_pid, &commands, &context)
                        .await?
                }
            }
        } else {
            false
        };
        if prompt_started {
            return Ok(());
        }

        let task_handler = handler.clone();
        let _ = handler.spawn_background_task("rmux-attached-prompt", move || async move {
            let execution = task_handler.execute_parsed_commands(requester_pid, commands, context);
            let _ = match live_identity {
                Some(identity) => {
                    with_expected_attach_and_session_identity(
                        identity,
                        session_name,
                        session_id,
                        execution,
                    )
                    .await
                }
                None => with_expected_session_identity(session_name, session_id, execution).await,
            };
        });
        return Ok(());
    }

    // A binding can enter the full command-dispatch and renderer stack. Poll it
    // as a separate Tokio task so the attach input stack unwinds before that
    // work begins; awaiting the task preserves command order. Abort it if the
    // attach request is cancelled instead of detaching work from its client;
    // once a popup releases the input path, the task belongs to the background
    // continuation instead.
    let task_handler = handler.clone();
    let task_commands = commands.clone();
    let task_session_name = session_name.clone();
    let inherited_hook = current_hook_execution().map(|execution| {
        let formats = current_hook_formats();
        (execution, formats)
    });
    let work = async move {
        let command_execution =
            task_handler.execute_parsed_commands(requester_pid, task_commands, context);
        let execution = async move {
            match live_identity {
                Some(identity) => {
                    with_expected_attach_and_session_identity(
                        identity,
                        task_session_name,
                        session_id,
                        command_execution,
                    )
                    .await
                }
                None => {
                    with_expected_session_identity(task_session_name, session_id, command_execution)
                        .await
                }
            }
        };
        match inherited_hook {
            Some((hook_execution, formats)) => {
                with_hook_execution(hook_execution, formats, execution).await
            }
            None => execution.await,
        }
    };
    let completion = AttachedBindingCompletion {
        attach_pid,
        live_identity,
        session_name,
        session_id,
        attached_live_input,
        dispatch_target,
        commands,
    };
    match handler.run_input_path_work(attach_pid, work).await {
        InputPathWork::Finished(joined) => completion.finish(handler, joined).await,
        InputPathWork::Released(released) => {
            // The binding keeps its order and its result; only this input path stops waiting. With
            // no caller left to receive an error, the client reports it.
            let continuation_handler = handler.clone();
            let session_name = completion.session_name.clone();
            handler.spawn_background_task("rmux-released-binding", move || async move {
                // The client's ordinary input waits until this binding's result is handled.
                let (joined, _outstanding) = released.join().await;
                if let Err(error) = completion.finish(&continuation_handler, joined).await {
                    continuation_handler
                        .report_attached_command_error(&session_name, attach_pid, &error)
                        .await;
                }
            })
        }
    }
}

/// What reporting a binding's result needs once its commands have finished.
struct AttachedBindingCompletion {
    attach_pid: u32,
    live_identity: Option<ActiveAttachIdentity>,
    session_name: SessionName,
    session_id: SessionId,
    attached_live_input: bool,
    dispatch_target: Target,
    commands: ParsedCommands,
}

impl AttachedBindingCompletion {
    async fn finish(
        self,
        handler: &RequestHandler,
        joined: Result<Result<CommandOutput, RmuxError>, JoinError>,
    ) -> Result<(), RmuxError> {
        let Self {
            attach_pid,
            live_identity,
            session_name,
            session_id,
            attached_live_input,
            dispatch_target,
            commands,
        } = self;
        let execution = joined
            .map_err(|error| RmuxError::Server(format!("attached binding task failed: {error}")))?;

        match execution {
            Ok(output) => {
                if attached_live_input && parsed_commands_are_attached_help(&commands) {
                    if let Some(identity) = live_identity {
                        if !handler.current_live_attach_input(identity).await {
                            return Ok(());
                        }
                    }
                    if let Err(error) = handler
                        .show_attached_key_help_popup(
                            AttachedHelpContext {
                                attach_pid,
                                expected_identity: live_identity,
                                expected_session_name: &session_name,
                                expected_session_id: session_id,
                                target: &dispatch_target,
                            },
                            &output,
                        )
                        .await
                    {
                        handler
                            .report_attached_command_error(&session_name, attach_pid, &error)
                            .await;
                    }
                }
            }
            Err(error) => {
                if attached_live_input {
                    if let Some(identity) = live_identity {
                        if !handler.current_live_attach_input(identity).await {
                            return Ok(());
                        }
                    }
                    handler
                        .report_attached_command_error(&session_name, attach_pid, &error)
                        .await;
                    return Ok(());
                }
                return Err(error);
            }
        }

        Ok(())
    }
}

fn parsed_commands_are_plain_display_panes(commands: &ParsedCommands) -> bool {
    let commands = commands.commands();
    commands.len() == 1
        && commands[0].name() == "display-panes"
        && commands[0].arguments().is_empty()
}

fn parsed_commands_block_for_prompt(commands: &ParsedCommands) -> bool {
    commands
        .commands()
        .iter()
        .any(parsed_command_blocks_for_prompt)
}

fn parsed_command_blocks_for_prompt(command: &ParsedCommand) -> bool {
    match command.name() {
        "display-panes" => !command
            .arguments()
            .iter()
            .filter_map(CommandArgument::as_string)
            .any(|argument| argument.starts_with('-') && argument.contains('b')),
        "command-prompt" => !command
            .arguments()
            .iter()
            .filter_map(CommandArgument::as_string)
            .any(|argument| {
                argument.starts_with('-') && (argument.contains('b') || argument.contains('i'))
            }),
        "confirm-before" => !command
            .arguments()
            .iter()
            .filter_map(CommandArgument::as_string)
            .any(|argument| argument.starts_with('-') && argument.contains('b')),
        _ => false,
    }
}

fn parsed_commands_are_attached_help(commands: &ParsedCommands) -> bool {
    let [command] = commands.commands() else {
        return false;
    };
    command.name() == "list-keys"
        && command
            .arguments()
            .iter()
            .filter_map(CommandArgument::as_string)
            .any(|argument| argument.starts_with('-') && argument[1..].contains('N'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_detection_handles_combined_flags() {
        use rmux_core::command_parser::CommandParser;

        let parsed = CommandParser::new()
            .parse_one_group("command-prompt -bF { display-message hi }")
            .unwrap();
        assert!(!parsed_commands_block_for_prompt(&parsed));

        let parsed = CommandParser::new()
            .parse_one_group("command-prompt -p test { display-message hi }")
            .unwrap();
        assert!(parsed_commands_block_for_prompt(&parsed));

        let parsed = CommandParser::new()
            .parse_one_group("confirm-before -by { kill-window }")
            .unwrap();
        assert!(!parsed_commands_block_for_prompt(&parsed));

        let parsed = CommandParser::new()
            .parse_one_group("display-panes")
            .unwrap();
        assert!(parsed_commands_block_for_prompt(&parsed));

        let parsed = CommandParser::new()
            .parse_one_group("display-panes -b")
            .unwrap();
        assert!(!parsed_commands_block_for_prompt(&parsed));
    }

    #[test]
    fn attached_help_detection_is_narrow_to_a_single_notes_listing() {
        use rmux_core::command_parser::CommandParser;

        let parser = CommandParser::new();
        let notes = parser.parse_one_group("list-keys -N").unwrap();
        assert!(parsed_commands_are_attached_help(&notes));

        let combined = parser.parse_one_group("list-keys -1N").unwrap();
        assert!(parsed_commands_are_attached_help(&combined));

        let plain = parser.parse_one_group("list-keys").unwrap();
        assert!(!parsed_commands_are_attached_help(&plain));

        let other = parser.parse_one_group("display-message -p hello").unwrap();
        assert!(!parsed_commands_are_attached_help(&other));

        let queue = parser
            .parse_one_group("list-keys -N ; display-message done")
            .unwrap();
        assert!(!parsed_commands_are_attached_help(&queue));
    }
}
