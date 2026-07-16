use rmux_core::command_parser::{CommandArgument, ParsedCommand, ParsedCommands};
use rmux_proto::{
    DisplayPanesRequest, ErrorResponse, Response, RmuxError, SessionId, SessionName, Target,
};
use tokio::task::AbortHandle;

use crate::handler::overlay_support::AttachedHelpContext;
use crate::hook_runtime::{current_hook_formats, hooks_disabled, with_hook_execution};
use crate::mouse::AttachedMouseEvent;

use super::super::super::{
    attach_support::ActiveAttachIdentity,
    attached_client_name,
    scripting_support::{spawn_background_async, QueueExecutionContext},
    with_expected_attach_and_session_identity, with_expected_session_identity, RequestHandler,
};

pub(super) struct AttachedBindingCommandContext {
    pub(super) attach_pid: u32,
    pub(super) live_identity: Option<ActiveAttachIdentity>,
    pub(super) requester_pid: u32,
    pub(super) session_name: SessionName,
    pub(super) session_id: SessionId,
    pub(super) attached_live_input: bool,
    pub(super) dispatch_target: Target,
    pub(super) mouse_target: Option<Target>,
    pub(super) mouse_event: Option<AttachedMouseEvent>,
    pub(super) commands: ParsedCommands,
}

struct AbortAttachedBindingOnDrop(Option<AbortHandle>);

impl AbortAttachedBindingOnDrop {
    fn new(abort_handle: AbortHandle) -> Self {
        Self(Some(abort_handle))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for AbortAttachedBindingOnDrop {
    fn drop(&mut self) {
        if let Some(abort_handle) = self.0.take() {
            abort_handle.abort();
        }
    }
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
        .with_client_name(Some(attached_client_name(attach_pid)))
        .with_mouse_target(mouse_target)
        .with_mouse_event(mouse_event);

    if attached_live_input && parsed_commands_are_plain_display_panes(&commands) {
        let request = DisplayPanesRequest {
            target: session_name.clone(),
            duration_ms: None,
            non_blocking: true,
            no_command: false,
            template: None,
            target_client: Some(attached_client_name(attach_pid)),
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

        let handler = handler.clone();
        let _ = spawn_background_async("rmux-attached-prompt", move || async move {
            let execution = handler.execute_parsed_commands(requester_pid, commands, context);
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
    // attach request is cancelled instead of detaching work from its client.
    let task_handler = handler.clone();
    let task_commands = commands.clone();
    let task_session_name = session_name.clone();
    let inherited_hook_formats = hooks_disabled().then(current_hook_formats);
    let task = tokio::spawn(async move {
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
        match inherited_hook_formats {
            Some(formats) => with_hook_execution(formats, execution).await,
            None => execution.await,
        }
    });
    let mut abort_on_drop = AbortAttachedBindingOnDrop::new(task.abort_handle());
    let joined = task.await;
    abort_on_drop.disarm();
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
                            requester_pid,
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
