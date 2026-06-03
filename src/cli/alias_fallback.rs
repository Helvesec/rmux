use std::ffi::OsString;
use std::path::Path;

use super::command_runner::run_queued_server_command;
use super::ExitFailure;

pub(super) fn run_unknown_command_through_server_aliases(
    args: &[OsString],
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let command_args = command_arguments(args)
        .ok_or_else(|| ExitFailure::new(1, "invalid UTF-8 in command arguments".to_owned()))?;
    if command_args.is_empty() {
        return Err(ExitFailure::new(1, "missing command".to_owned()));
    }
    let queue_command = command_args
        .iter()
        .map(|argument| tmux_quote_argument(argument))
        .collect::<Vec<_>>()
        .join(" ");
    run_queued_server_command(socket_path, "source-file", queue_command)
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
        assert_eq!(tmux_quote_argument(";"), ";");
        assert_eq!(tmux_quote_argument("display-message"), "display-message");
        assert_eq!(tmux_quote_argument("hello world"), "'hello world'");
        assert_eq!(tmux_quote_argument("it's"), "'it'\\''s'");
    }
}
