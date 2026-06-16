use rmux_core::command_parser::{CommandArgument, ParsedCommands};
use rmux_core::key_code_lookup_bits;
use rmux_proto::{PaneTarget, RmuxError, Target};

use super::super::{scripting_support::QueueExecutionContext, RequestHandler};
use super::target_is_in_copy_mode;
use crate::key_table::{
    default_key_table_name, lookup_key_table_binding, COPY_MODE_TABLE, COPY_MODE_VI_TABLE,
};

pub(in crate::handler) struct DirectCopyModeCommand {
    pub(in crate::handler) command: String,
    pub(in crate::handler) args: Vec<String>,
    pub(in crate::handler) repeat_count: usize,
}

pub(in crate::handler) fn direct_copy_mode_command(
    commands: &ParsedCommands,
) -> Option<DirectCopyModeCommand> {
    if !commands.assignments().is_empty() {
        return None;
    }
    let [command] = commands.commands() else {
        return None;
    };
    if !matches!(command.name(), "send" | "send-keys") {
        return None;
    }

    let mut args = command.arguments().iter();
    let mut copy_mode_command = false;
    let mut repeat_count = 1;
    let mut command_args = Vec::new();
    while let Some(argument) = args.next() {
        let value = argument.as_string()?;
        match value {
            "--" => {
                command_args.extend(copy_mode_argument_strings(args)?);
                break;
            }
            "-X" => copy_mode_command = true,
            "-N" => {
                repeat_count = args.next()?.as_string()?.parse::<usize>().ok()?.max(1);
            }
            value if value.starts_with("-N") && value.len() > 2 => {
                repeat_count = value[2..].parse::<usize>().ok()?.max(1);
            }
            value if value.starts_with('-') => return None,
            value => {
                command_args.push(value.to_owned());
                command_args.extend(copy_mode_argument_strings(args)?);
                break;
            }
        }
    }

    if !copy_mode_command {
        return None;
    }
    let command = command_args.first()?.clone();
    Some(DirectCopyModeCommand {
        command,
        args: command_args.into_iter().skip(1).collect(),
        repeat_count,
    })
}

fn copy_mode_argument_strings<'a>(
    args: impl Iterator<Item = &'a CommandArgument>,
) -> Option<Vec<String>> {
    args.map(|argument| argument.as_string().map(str::to_owned))
        .collect()
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_detached_copy_mode_key_code(
        &self,
        requester_pid: u32,
        target: PaneTarget,
        key: rmux_core::KeyCode,
    ) -> Result<bool, RmuxError> {
        let binding = {
            let state = self.state.lock().await;
            if !target_is_in_copy_mode(&state, &target) {
                return Ok(false);
            }
            let table_name = default_key_table_name(&state, &target);
            if !matches!(table_name.as_str(), COPY_MODE_TABLE | COPY_MODE_VI_TABLE) {
                return Ok(false);
            }
            lookup_key_table_binding(&state, &table_name, key_code_lookup_bits(key))
        };

        let Some(binding) = binding else {
            return Ok(true);
        };

        if let Some(command) = direct_copy_mode_command(binding.commands()) {
            Box::pin(self.execute_copy_mode_command(
                requester_pid,
                target,
                &command.command,
                &command.args,
                command.repeat_count,
            ))
            .await?;
            return Ok(true);
        }

        let context = QueueExecutionContext::without_caller_cwd()
            .with_current_target(Some(Target::Pane(target)));
        self.execute_parsed_commands(requester_pid, binding.commands().clone(), context)
            .await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_copy_mode_command_accepts_send_alias() {
        use rmux_core::command_parser::CommandParser;

        let parsed = CommandParser::new()
            .parse_one_group("send -N3 -X cancel")
            .unwrap();
        let command = direct_copy_mode_command(&parsed).unwrap();

        assert_eq!(command.command, "cancel");
        assert_eq!(command.args, Vec::<String>::new());
        assert_eq!(command.repeat_count, 3);
    }
}
