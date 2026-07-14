use std::error::Error;
use std::fmt;

use rmux_core::command_parser::CommandParser as TmuxCommandParser;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandAliasSnapshotError {
    InvalidUtf8,
    InvalidEntry(String),
}

impl fmt::Display for CommandAliasSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 => formatter.write_str("invalid UTF-8 in command-alias options"),
            Self::InvalidEntry(message) => {
                write!(formatter, "invalid command-alias snapshot: {message}")
            }
        }
    }
}

impl Error for CommandAliasSnapshotError {}

pub(crate) fn decode_command_alias_definitions(
    output: &[u8],
) -> Result<Vec<String>, CommandAliasSnapshotError> {
    let rendered =
        std::str::from_utf8(output).map_err(|_| CommandAliasSnapshotError::InvalidUtf8)?;
    rendered
        .lines()
        .filter(|line| !line.is_empty())
        .map(parse_rendered_command_alias)
        .collect()
}

pub(crate) fn definition_matches_name(definition: &str, command_name: &str) -> bool {
    definition
        .split_once('=')
        .is_some_and(|(name, _)| name == command_name)
}

fn parse_rendered_command_alias(line: &str) -> Result<String, CommandAliasSnapshotError> {
    let parsed = TmuxCommandParser::new()
        .parse_one_group(&format!("set-option -s {line}"))
        .map_err(|error| invalid_entry(error.to_string()))?;
    let [command] = parsed.commands() else {
        return Err(invalid_entry("expected one option entry"));
    };
    let arguments = command.arguments();
    if !(2..=3).contains(&arguments.len())
        || arguments[0].as_string() != Some("-s")
        || !arguments[1]
            .as_string()
            .is_some_and(is_command_alias_option_name)
    {
        return Err(invalid_entry("invalid option entry shape"));
    }
    match arguments.get(2) {
        Some(value) => value
            .as_string()
            .map(str::to_owned)
            .ok_or_else(|| invalid_entry("option value is not text")),
        None => Ok(String::new()),
    }
}

fn is_command_alias_option_name(name: &str) -> bool {
    if name == "command-alias" {
        return true;
    }
    name.strip_prefix("command-alias[")
        .and_then(|name| name.strip_suffix(']'))
        .is_some_and(|index| !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()))
}

fn invalid_entry(message: impl fmt::Display) -> CommandAliasSnapshotError {
    CommandAliasSnapshotError::InvalidEntry(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::{decode_command_alias_definitions, definition_matches_name};

    #[test]
    fn rendered_snapshot_round_trips_quoted_control_characters() {
        let rendered = br#"command-alias[7] "probe=display-message -p \"space ; dollar \$HOME slash\\ quote' double\\\"\n\r\t\""
command-alias[8] simple=display-message
"#;

        assert_eq!(
            decode_command_alias_definitions(rendered).expect("rendered aliases"),
            [
                "probe=display-message -p \"space ; dollar $HOME slash\\ quote' double\\\"\n\r\t\""
                    .to_owned(),
                "simple=display-message".to_owned(),
            ]
        );
    }

    #[test]
    fn alias_name_matching_does_not_confuse_command_prefixes() {
        assert!(definition_matches_name(
            "list-sessions=display-message -p alias",
            "list-sessions"
        ));
        assert!(!definition_matches_name(
            "list-sessions-long=display-message -p alias",
            "list-sessions"
        ));
        assert!(!definition_matches_name("malformed", "malformed"));
    }
}
