//! Parsing of the built-in `bind-key` commands that seed the default key tables.

use crate::command_parser::{
    parse_command_string, CommandArgument, CommandParseError, ParsedCommand, ParsedCommands,
};

use super::super::{key_string_lookup_string, KeyCode};

pub(super) fn parse_bind_command(
    command: &ParsedCommand,
) -> Result<(String, KeyCode, Option<String>, bool, ParsedCommands), CommandParseError> {
    let mut index = 0;
    let mut table_name: Option<String> = None;
    let mut note = None;
    let mut repeat = false;
    let arguments = command.arguments();

    while let Some(argument) = arguments.get(index) {
        let Some(value) = argument.as_string() else {
            break;
        };
        match value {
            "-T" => {
                index += 1;
                table_name = Some(
                    arguments
                        .get(index)
                        .and_then(|argument| argument.as_string())
                        .ok_or_else(|| CommandParseError::new(1, "bind-key missing -T key-table"))?
                        .to_owned(),
                );
            }
            _ if value.starts_with("-T") && value.len() > 2 => {
                table_name = Some(value[2..].to_owned());
            }
            "-n" => table_name = Some("root".to_owned()),
            "-N" => {
                index += 1;
                note = Some(
                    arguments
                        .get(index)
                        .and_then(|argument| argument.as_string())
                        .ok_or_else(|| CommandParseError::new(1, "bind-key missing -N note"))?
                        .to_owned(),
                );
            }
            _ if value.starts_with("-N") && value.len() > 2 => {
                note = Some(value[2..].to_owned());
            }
            "-r" => repeat = true,
            _ if value.starts_with('-') && value.len() > 1 => {
                return Err(CommandParseError::new(
                    1,
                    format!("unsupported default bind-key flag: {value}"),
                ));
            }
            _ => break,
        }
        index += 1;
    }

    let key_string = arguments
        .get(index)
        .and_then(|argument| argument.as_string())
        .ok_or_else(|| CommandParseError::new(1, "bind-key missing key"))?;
    index += 1;
    let key = key_string_lookup_string(key_string)
        .ok_or_else(|| CommandParseError::new(1, format!("unknown key: {key_string}")))?;

    let commands = match arguments.get(index) {
        Some(CommandArgument::Commands(commands)) => commands.clone(),
        Some(CommandArgument::String(command)) => parse_command_string(command)?,
        None => {
            return Err(CommandParseError::new(
                1,
                "default bind-key must include a command list",
            ))
        }
    };

    Ok((
        table_name.unwrap_or_else(|| "prefix".to_owned()),
        key,
        note,
        repeat,
        commands,
    ))
}
