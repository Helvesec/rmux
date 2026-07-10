use rmux_core::command_inventory::render_list_commands;
use rmux_core::command_parser::ParsedCommand;
use rmux_proto::{CommandOutput, RmuxError};

use super::command_args::command_arguments_as_strings;
use super::queue::QueueCommandAction;
use super::tokens::{parse_compact_flag_cluster, CommandTokens};
use super::values::unsupported_flag;
use super::RequestHandler;

#[derive(Debug, Clone)]
pub(super) struct ParsedListCommandsCommand {
    format: Option<String>,
    command: Option<String>,
}

pub(super) fn parse_queued_list_commands(
    command: ParsedCommand,
) -> Result<ParsedListCommandsCommand, RmuxError> {
    let arguments = command_arguments_as_strings(command.name(), command.arguments())?;
    let mut args = CommandTokens::new(arguments);
    let mut format = None;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-F" => {
                let _ = args.optional();
                format = Some(args.required("-F format")?);
            }
            token => {
                let Some(cluster) = parse_compact_flag_cluster(token, "", "F") else {
                    if token.starts_with('-') && token != "-" {
                        return Err(unsupported_flag("list-commands", token));
                    }
                    break;
                };
                let _ = args.optional();
                let flag = cluster
                    .into_iter()
                    .next()
                    .expect("nonempty compact list-commands flag");
                format = Some(flag.value_or_next(&mut args, "-F format")?);
            }
        }
    }

    let command = args.optional();
    args.no_extra("list-commands")?;
    Ok(ParsedListCommandsCommand { format, command })
}

impl RequestHandler {
    pub(super) fn execute_queued_list_commands(
        &self,
        command: ParsedListCommandsCommand,
    ) -> Result<QueueCommandAction, RmuxError> {
        let lines = render_list_commands(command.format.as_deref(), command.command.as_deref())
            .map_err(|error| RmuxError::Server(error.to_string()))?;
        let stdout = if lines.is_empty() {
            Vec::new()
        } else {
            format!("{}\n", lines.join("\n")).into_bytes()
        };
        Ok(QueueCommandAction::Normal {
            output: Some(CommandOutput::from_stdout(stdout)),
            error: None,
            source_file_error: None,
            exit_status: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use rmux_core::command_parser::CommandParser;

    use super::*;

    fn parse(command: &str) -> ParsedListCommandsCommand {
        let parsed = CommandParser::new()
            .parse_one_group(command)
            .expect("list-commands parses");
        parse_queued_list_commands(
            parsed
                .commands()
                .first()
                .expect("one parsed command")
                .clone(),
        )
        .expect("queued list-commands parses")
    }

    #[test]
    fn compact_format_flag_and_explicit_command_parse() {
        let parsed = parse("list-commands '-F#{command_list_name}' new-window");
        assert_eq!(parsed.format.as_deref(), Some("#{command_list_name}"));
        assert_eq!(parsed.command.as_deref(), Some("new-window"));
    }
}
