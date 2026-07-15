use std::ffi::OsString;
use std::path::Path;

use rmux_client::{connect, resolve_socket_path, resolve_tmux_compatible_socket_path, Connection};
use rmux_core::{
    command_inventory::{has_tmux_command_candidate, RMUX_EXTENSION_COMMANDS},
    command_parser::CommandParser,
};
use rmux_proto::OptionScopeSelector;

use crate::cli_args::{
    parse, scan_top_level_command, Command, RuntimeCommandGroup, TopLevelCommandScan,
};
use crate::cli_response::expect_command_output;
use crate::command_alias_snapshot::{decode_command_alias_definitions, definition_matches_name};
use crate::runtime_command_expansion::{
    expand_runtime_command_segment, RuntimeCommandExpansionError,
};

use super::command_runner::run_queued_server_command_with_connection;
use super::ExitFailure;

pub(super) enum RuntimeCommandResolution {
    Canonical(Vec<RuntimeCommandGroup>),
    LegacyDirect,
    LegacyServerDispatch(i32),
}

struct RuntimeCommandInvocation {
    arguments: Vec<String>,
    groups: Vec<Vec<String>>,
    control_mode: u8,
}

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
    run_raw_command_through_server(&command_args, socket_path, connection)
}

fn run_raw_command_through_server(
    command_args: &[String],
    socket_path: &Path,
    connection: &mut Connection,
) -> Result<i32, ExitFailure> {
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
    Ok(server_command_alias_definitions(connection)?
        .iter()
        .any(|definition| definition_matches_name(definition, command_name)))
}

fn server_command_alias_definitions(
    connection: &mut Connection,
) -> Result<Vec<String>, ExitFailure> {
    let response = connection
        .show_options(
            OptionScopeSelector::ServerGlobal,
            Some("command-alias".to_owned()),
            false,
            false,
            true,
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "show-options")?;
    decode_command_alias_definitions(output.stdout())
        .map_err(|error| ExitFailure::new(1, error.to_string()))
}

pub(super) fn runtime_command_resolution_for_invocation(
    args: &[OsString],
    invoked_as_tmux: bool,
) -> Result<Option<RuntimeCommandResolution>, ExitFailure> {
    let Some((scan, invocation)) = prepare_runtime_command_invocation(args) else {
        return Ok(None);
    };
    let socket_path = if invoked_as_tmux {
        resolve_tmux_compatible_socket_path(
            scan.socket_name.as_deref(),
            scan.socket_path.as_deref().map(Path::new),
        )
    } else {
        resolve_socket_path(
            scan.socket_name.as_deref(),
            scan.socket_path.as_deref().map(Path::new),
        )
    };
    let Some(socket_path) = socket_path.ok() else {
        return Ok(None);
    };
    let mut connection = match connect(&socket_path) {
        Ok(connection) => connection,
        Err(error) => {
            let failure = ExitFailure::from_client_connect(&socket_path, error);
            if failure.is_server_absent() {
                return Ok(None);
            }
            return Err(failure.with_socket_context(&socket_path));
        }
    };

    resolve_runtime_command_with_connection(&mut connection, &socket_path, invocation)
}

pub(super) fn runtime_command_resolution_after_startup(
    args: &[OsString],
    socket_path: &Path,
    connection: &mut Connection,
) -> Result<Option<RuntimeCommandResolution>, ExitFailure> {
    let Some((_, invocation)) = prepare_runtime_command_invocation(args) else {
        return Ok(None);
    };
    resolve_runtime_command_with_connection(connection, socket_path, invocation)
}

/// Parses only the first argv command group so a cold daemon can load aliases
/// used by later groups before the complete queue is parsed.
pub(super) fn first_cold_start_command(args: &[OsString]) -> Option<Command> {
    let (_, invocation) = prepare_runtime_command_invocation(args)?;
    let first_group = invocation.groups.first()?;
    let first_args =
        std::iter::once(OsString::from("rmux")).chain(first_group.iter().map(OsString::from));
    parse(first_args).ok()?.command
}

fn prepare_runtime_command_invocation(
    args: &[OsString],
) -> Option<(TopLevelCommandScan, RuntimeCommandInvocation)> {
    let scan = scan_top_level_command(args.get(1..).unwrap_or(&[])).ok()?;
    if scan.no_fork || scan.shell_command.is_some() || scan.command.is_empty() {
        return None;
    }
    let arguments = args_to_strings(&scan.command)?;
    if arguments.is_empty() {
        return None;
    }
    let groups = split_literal_command_groups(&scan.command)?;
    let control_mode = scan.control_mode;
    Some((
        scan,
        RuntimeCommandInvocation {
            arguments,
            groups,
            control_mode,
        },
    ))
}

fn resolve_runtime_command_with_connection(
    connection: &mut Connection,
    socket_path: &Path,
    invocation: RuntimeCommandInvocation,
) -> Result<Option<RuntimeCommandResolution>, ExitFailure> {
    let RuntimeCommandInvocation {
        arguments,
        groups,
        control_mode,
    } = invocation;
    let legacy_kill_fallback = queue_starts_with_kill_server(&groups);

    let canonical = match expand_runtime_command_segment(connection, &arguments) {
        Ok(Some(canonical)) => canonical,
        Ok(None) if control_mode != 0 => return Ok(None),
        Ok(None) => {
            let aliases = server_command_alias_definitions(connection)
                .map_err(|error| error.with_socket_context(socket_path))?;
            if !queue_uses_server_alias(&groups, &aliases) {
                return Ok(Some(RuntimeCommandResolution::LegacyDirect));
            }
            let exit_code = run_raw_command_through_server(&arguments, socket_path, connection)
                .map_err(|error| error.with_socket_context(socket_path))?;
            return Ok(Some(RuntimeCommandResolution::LegacyServerDispatch(
                exit_code,
            )));
        }
        Err(error) if legacy_kill_fallback && error.previous_wire_version().is_some() => {
            return Ok(None);
        }
        Err(error) => {
            let failure = match error {
                RuntimeCommandExpansionError::Client(error) => ExitFailure::from_client(error),
                RuntimeCommandExpansionError::Server(error) => {
                    ExitFailure::from_client(rmux_client::ClientError::Protocol(error))
                }
                RuntimeCommandExpansionError::Protocol(message) => ExitFailure::new(1, message),
            };
            let failure = if control_mode == 0 || failure.is_unsupported_wire_version() {
                failure
            } else {
                super::control_mode_error::exit_failure_for_count(
                    failure.exit_code(),
                    failure.message(),
                    control_mode,
                )
            };
            return Err(failure.with_socket_context(socket_path));
        }
    };

    Ok(Some(RuntimeCommandResolution::Canonical(vec![
        RuntimeCommandGroup::Canonical(canonical),
    ])))
}

/// Mirrors `CommandParser::parse_arguments`: argv is already tokenized, and
/// only an unescaped trailing semicolon terminates a command group.
fn split_literal_command_groups(arguments: &[OsString]) -> Option<Vec<Vec<String>>> {
    let mut groups = Vec::new();
    let mut current = Vec::new();

    for argument in arguments {
        let mut value = argument.to_str()?.to_owned();
        let mut ends_command = false;
        if value.ends_with(';') {
            value.pop();
            if value.ends_with('\\') {
                value.pop();
                value.push(';');
            } else {
                ends_command = true;
            }
        }

        if !ends_command || !value.is_empty() {
            current.push(value);
        }
        if ends_command && !current.is_empty() {
            groups.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        groups.push(current);
    }
    Some(groups)
}

fn queue_starts_with_kill_server(groups: &[Vec<String>]) -> bool {
    let Some(group) = groups.first() else {
        return false;
    };
    let parser = CommandParser::new()
        .with_command_aliases(std::iter::empty::<String>())
        .with_exact_commands(RMUX_EXTENSION_COMMANDS);
    let Ok(parsed) = parser.parse_arguments(group) else {
        return false;
    };
    matches!(parsed.commands(), [command] if command.name() == "kill-server" && command.arguments().is_empty())
}

fn queue_uses_server_alias(groups: &[Vec<String>], aliases: &[String]) -> bool {
    groups
        .iter()
        .filter_map(|group| alias_lookup_command_name(group))
        .any(|name| {
            aliases
                .iter()
                .any(|definition| definition_matches_name(definition, name))
        })
}

fn alias_lookup_command_name(group: &[String]) -> Option<&str> {
    let (first, tail) = group.split_first()?;
    if is_parse_time_assignment(first) {
        tail.first().map(String::as_str)
    } else {
        Some(first)
    }
}

// Keep this in lockstep with rmux-core's argv assignment grammar: an ASCII
// identifier followed by `=`, with the remainder belonging to the value.
fn is_parse_time_assignment(argument: &str) -> bool {
    let Some((name, _)) = argument.split_once('=') else {
        return false;
    };
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
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
    args.iter()
        .map(|value| value.to_str().map(str::to_owned))
        .collect()
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
    use std::ffi::OsStr;

    use super::*;

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
    fn argv_groups_preserve_escaped_semicolon_literals() {
        assert_eq!(
            split_literal_command_groups(&args(&[
                "display-message",
                "literal\\;",
                ";",
                "list-sessions;",
            ])),
            Some(vec![
                vec!["display-message".to_owned(), "literal;".to_owned()],
                vec!["list-sessions".to_owned()],
            ])
        );
    }

    #[test]
    fn tmux_quote_preserves_command_separators_and_quotes_values() {
        assert_eq!(tmux_quote_argument(""), "''");
        assert_eq!(tmux_quote_argument(";"), ";");
        assert_eq!(tmux_quote_argument("xyz;"), "xyz;");
        assert_eq!(tmux_quote_argument("hello world;"), "'hello world';");
        assert_eq!(tmux_quote_argument("xyz\\;"), "'xyz;'");
        assert_eq!(tmux_quote_argument("semi;colon"), "'semi;colon'");
        assert_eq!(tmux_quote_argument("it's"), "'it'\\''s'");
    }

    #[test]
    fn legacy_fallback_accepts_canonical_and_unambiguous_kill_server_prefixes() {
        assert!(queue_starts_with_kill_server(&[vec![
            "kill-server".to_owned(),
        ]]));
        assert!(queue_starts_with_kill_server(&[vec![
            "kill-serv".to_owned(),
        ]]));
        assert!(queue_starts_with_kill_server(&[
            vec!["kill-server".to_owned()],
            vec!["new-session".to_owned()],
        ]));
        assert!(!queue_starts_with_kill_server(&[vec![
            "kill-server".to_owned(),
            "unexpected".to_owned(),
        ]]));
        assert!(!queue_starts_with_kill_server(&[
            vec!["display-message".to_owned()],
            vec!["kill-server".to_owned()],
        ]));
    }

    #[test]
    fn cold_start_probe_parses_only_the_first_command_group() {
        let invocation = args(&[
            "rmux",
            "new-session",
            "-d",
            "-s",
            "alpha",
            ";",
            "runtime-alias",
        ]);
        assert!(matches!(
            first_cold_start_command(&invocation),
            Some(Command::NewSession(_))
        ));

        let invalid_first = args(&["rmux", "not-a-command", ";", "new-session", "-d"]);
        assert!(first_cold_start_command(&invalid_first).is_none());
    }

    #[test]
    fn legacy_fallback_detects_aliases_in_any_command_group() {
        let groups = vec![
            vec!["list-sessions".to_owned()],
            vec!["probe".to_owned(), "argument".to_owned()],
        ];
        let aliases = vec!["probe=display-message -p ok".to_owned()];

        assert!(queue_uses_server_alias(&groups, &aliases));
        assert!(!queue_uses_server_alias(
            &[vec!["list-sessions".to_owned()]],
            &aliases,
        ));
    }

    #[test]
    fn legacy_fallback_detects_alias_after_parse_time_assignment() {
        let groups = vec![vec!["FOO=x".to_owned(), "probe".to_owned()]];
        let aliases = vec!["probe=display-message -p $FOO".to_owned()];

        assert!(queue_uses_server_alias(&groups, &aliases));
        assert_eq!(alias_lookup_command_name(&groups[0]), Some("probe"));
        assert!(is_parse_time_assignment("FOO=x"));
        assert!(is_parse_time_assignment("_FOO=x=y"));
        assert!(!is_parse_time_assignment("1FOO=x"));
        assert!(!is_parse_time_assignment("FOO-BAR=x"));
    }

    #[test]
    fn legacy_fallback_keeps_nested_commands_and_builtin_abbreviations_server_owned() {
        let aliases = vec![
            "nested=display-message -p nested".to_owned(),
            "list-sessions=display-message -p override".to_owned(),
        ];

        assert!(!queue_uses_server_alias(
            &[vec![
                "if-shell".to_owned(),
                "-F".to_owned(),
                "1".to_owned(),
                "nested".to_owned(),
            ]],
            &aliases,
        ));
        assert!(!queue_uses_server_alias(
            &[vec!["list-sess".to_owned()]],
            &aliases,
        ));
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
