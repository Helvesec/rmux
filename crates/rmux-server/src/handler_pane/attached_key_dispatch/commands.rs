use rmux_core::command_parser::{CommandArgument, ParsedCommand, ParsedCommands};
use rmux_proto::{RmuxError, SessionName, Target};

use super::super::super::{
    scripting_support::{spawn_background_async, QueueExecutionContext},
    RequestHandler,
};

pub(super) struct AttachedBindingCommandContext {
    pub(super) attach_pid: u32,
    pub(super) requester_pid: u32,
    pub(super) session_name: SessionName,
    pub(super) attached_live_input: bool,
    pub(super) dispatch_target: Target,
    pub(super) mouse_target: Option<Target>,
    pub(super) commands: ParsedCommands,
}

#[async_recursion::async_recursion]
pub(super) async fn execute_attached_binding_commands(
    handler: &RequestHandler,
    command_context: AttachedBindingCommandContext,
) -> Result<(), RmuxError> {
    let AttachedBindingCommandContext {
        attach_pid,
        requester_pid,
        session_name,
        attached_live_input,
        dispatch_target,
        mouse_target,
        commands,
    } = command_context;

    let context = QueueExecutionContext::without_caller_cwd()
        .with_current_target(Some(dispatch_target.clone()))
        .with_mouse_target(mouse_target);

    if parsed_commands_block_for_prompt(&commands) {
        if attached_live_input
            && handler
                .start_attached_prompt_binding_commands(requester_pid, &commands, &context)
                .await?
        {
            return Ok(());
        }

        let handler = handler.clone();
        let _ = spawn_background_async("rmux-attached-prompt", move || async move {
            let _ = handler
                .execute_parsed_commands(requester_pid, commands, context)
                .await;
        });
        return Ok(());
    }

    match handler
        .execute_parsed_commands(requester_pid, commands.clone(), context)
        .await
    {
        Ok(output) => {
            if attached_live_input && parsed_commands_open_attached_output(&commands) {
                if let Err(error) = handler
                    .show_attached_command_output_popup(
                        attach_pid,
                        requester_pid,
                        dispatch_target,
                        "list-keys (q/Esc=close)",
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

fn parsed_commands_open_attached_output(commands: &ParsedCommands) -> bool {
    commands
        .commands()
        .iter()
        .any(|command| command.name() == "list-keys")
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
}
