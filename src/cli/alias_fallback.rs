use std::ffi::OsString;
use std::path::Path;

use rmux_client::Connection;
use rmux_core::command_inventory::has_tmux_command_candidate;
use rmux_proto::OptionScopeSelector;

use crate::cli_response::expect_command_output;

use super::command_runner::run_queued_server_command_with_connection;
use super::ExitFailure;

pub(super) fn run_unknown_command_through_server_aliases(
    args: &[OsString],
    socket_path: &Path,
    connection: &mut Connection,
) -> Result<i32, ExitFailure> {
    let command_args = command_arguments(args)
        .ok_or_else(|| ExitFailure::new(1, "invalid UTF-8 in command arguments".to_owned()))?;
    if command_args.is_empty() {
        return Err(ExitFailure::new(1, "missing command".to_owned()));
    }
    let command_name = &command_args[0];
    if !has_tmux_command_candidate(command_name)
        && !server_has_command_alias(connection, command_name)?
    {
        return Err(ExitFailure::new(
            1,
            format!("unknown command: {command_name}"),
        ));
    }
    let queue_command = command_args
        .iter()
        .map(|argument| tmux_quote_argument(argument))
        .collect::<Vec<_>>()
        .join(" ");
    run_queued_server_command_with_connection(connection, socket_path, "source-file", queue_command)
        .map_err(normalize_alias_fallback_error)
}

fn server_has_command_alias(
    connection: &mut Connection,
    command_name: &str,
) -> Result<bool, ExitFailure> {
    let response = connection
        .show_options(
            OptionScopeSelector::ServerGlobal,
            Some("command-alias".to_owned()),
            true,
            false,
            true,
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "show-options")?;
    let definitions = std::str::from_utf8(output.stdout())
        .map_err(|_| ExitFailure::new(1, "invalid UTF-8 in command-alias options".to_owned()))?;
    Ok(command_alias_output_contains(definitions, command_name))
}

fn command_alias_output_contains(definitions: &str, command_name: &str) -> bool {
    definitions.lines().any(|definition| {
        definition
            .split_once('=')
            .is_some_and(|(name, _)| name == command_name)
    })
}

fn normalize_alias_fallback_error(error: ExitFailure) -> ExitFailure {
    let Some(message) = strip_source_file_stdin_line_prefix(error.message()) else {
        return error;
    };
    ExitFailure::new(error.exit_code(), message.to_owned())
}

fn strip_source_file_stdin_line_prefix(message: &str) -> Option<&str> {
    let rest = message.strip_prefix("-:")?;
    let (line, message) = rest.split_once(": ")?;
    line.bytes()
        .all(|byte| byte.is_ascii_digit())
        .then_some(message)
}

fn command_arguments(args: &[OsString]) -> Option<Vec<String>> {
    let mut index = 1;
    while index < args.len() {
        let argument = args[index].to_str()?;
        if argument == "--" {
            return args_to_strings(&args[index + 1..]);
        }
        if !argument.starts_with('-') || argument == "-" {
            return args_to_strings(&args[index..]);
        }

        match argument {
            "-c" | "-f" | "-L" | "-S" | "-T" => index += 1,
            value if value.starts_with("-L") && value.len() > 2 => {}
            value if value.starts_with("-S") && value.len() > 2 => {}
            _ => {}
        }
        index += 1;
    }
    Some(Vec::new())
}

fn args_to_strings(args: &[OsString]) -> Option<Vec<String>> {
    args.iter().map(os_string_to_string).collect()
}

fn os_string_to_string(value: &OsString) -> Option<String> {
    value.to_str().map(str::to_owned)
}

fn tmux_quote_argument(argument: &str) -> String {
    if argument == ";" {
        return argument.to_owned();
    }
    if let Some(base) = argument.strip_suffix(';') {
        if let Some(escaped_base) = base.strip_suffix('\\') {
            return tmux_quote_value(&format!("{escaped_base};"));
        }
        return format!("{};", tmux_quote_value(base));
    }
    tmux_quote_value(argument)
}

fn tmux_quote_value(argument: &str) -> String {
    if argument.is_empty() {
        return "''".to_owned();
    }
    if argument
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '='))
    {
        return argument.to_owned();
    }
    format!("'{}'", argument.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsStr::new).map(OsString::from).collect()
    }

    #[test]
    fn command_arguments_skip_top_level_socket_options() {
        assert_eq!(
            command_arguments(&args(&["rmux", "-L", "demo", "hi", "there"])),
            Some(vec!["hi".to_owned(), "there".to_owned()])
        );
        assert_eq!(
            command_arguments(&args(&["rmux", "-Sdemo.sock", "hi"])),
            Some(vec!["hi".to_owned()])
        );
    }

    #[test]
    fn tmux_quote_preserves_command_separators_and_quotes_values() {
        assert_eq!(tmux_quote_argument(""), "''");
        assert_eq!(tmux_quote_argument(";"), ";");
        assert_eq!(tmux_quote_argument("xyz;"), "xyz;");
        assert_eq!(tmux_quote_argument("hello world;"), "'hello world';");
        assert_eq!(tmux_quote_argument("xyz\\;"), "'xyz;'");
        assert_eq!(tmux_quote_argument("display-message"), "display-message");
        assert_eq!(tmux_quote_argument("hello world"), "'hello world'");
        assert_eq!(tmux_quote_argument("semi;colon"), "'semi;colon'");
        assert_eq!(tmux_quote_argument("it's"), "'it'\\''s'");
    }

    #[test]
    fn alias_output_lookup_requires_an_exact_alias_name() {
        let definitions = "say=display-message -p\nstatus=show-messages -JT\n";
        assert!(command_alias_output_contains(definitions, "say"));
        assert!(!command_alias_output_contains(definitions, "sa"));
        assert!(!command_alias_output_contains(definitions, "FOO=bar"));
    }

    #[test]
    fn builtin_candidate_lookup_preserves_parser_diagnostics() {
        assert!(has_tmux_command_candidate("list"));
        assert!(has_tmux_command_candidate("send-keys"));
        assert!(!has_tmux_command_candidate("FOO=bar"));
    }

    #[test]
    fn alias_fallback_errors_strip_synthetic_source_file_prefix() {
        assert_eq!(
            strip_source_file_stdin_line_prefix("-:1: unknown command: nope"),
            Some("unknown command: nope")
        );
        assert_eq!(
            strip_source_file_stdin_line_prefix("unknown command: nope"),
            None
        );
    }
}
