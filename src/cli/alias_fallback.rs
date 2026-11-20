use std::ffi::OsString;
use std::path::Path;

use rmux_client::{connect, resolve_socket_path, resolve_tmux_compatible_socket_path, Connection};
use rmux_core::{
    command_inventory::{has_tmux_command_candidate, RMUX_EXTENSION_COMMANDS},
    command_parser::{is_parse_time_assignment, CommandParser},
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

use super::ExitFailure;
pub(super) mod new_pane;
pub(super) mod resize_pane_position;
pub(super) mod set_pane_placement;
pub(super) mod source_file_input;
use new_pane::{
    run_legacy_checked as run_raw_command_through_server,
    run_legacy_checked_with_aliases as run_raw_command_through_server_with_aliases,
};

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

fn server_has_command_alias(
    connection: &mut Connection,
    command_name: &str,
) -> Result<bool, ExitFailure> {
    Ok(server_command_alias_definitions(connection)?
        .iter()
        .any(|definition| definition_matches_name(definition, command_name)))
}

pub(super) fn server_command_alias_definitions(
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
    deadline: Option<rmux_bootstrap::deadline::StartupDeadline>,
) -> Result<Option<RuntimeCommandResolution>, ExitFailure> {
    let Some((scan, invocation)) = prepare_runtime_command_invocation(args) else {
        return Ok(None);
    };
    if invocation_is_intrinsically_local(&invocation) {
        return Ok(None);
    }
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
    let connection = match deadline {
        Some(deadline) => rmux_client::connection::connect_with_deadline(&socket_path, deadline),
        None => connect(&socket_path),
    };
    let mut connection = match connection {
        Ok(connection) => connection,
        Err(error) => {
            let failure = ExitFailure::from_client_connect(&socket_path, error);
            if failure.is_server_absent() {
                return Ok(None);
            }
            return Err(failure.with_socket_context(&socket_path));
        }
    };

    let expansion = expand_runtime_command_segment(&mut connection, &invocation.arguments);
    if matches!(&expansion, Err(error) if error.is_unanswered_by_exiting_daemon()) {
        // Like an absent endpoint: a server-starting command then waits for
        // the exiting daemon through auto-start instead of failing on it.
        return Ok(None);
    }
    resolve_expanded_runtime_command(&mut connection, &socket_path, invocation, expansion)
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

/// Finds the first server-starting command among recognized argv groups so a
/// cold daemon can load aliases before the complete queue is parsed. Stop at
/// an invalid group: an unresolved alias cannot authorize starting a server.
pub(super) fn first_cold_start_command(args: &[OsString]) -> Option<Command> {
    let (_, invocation) = prepare_runtime_command_invocation(args)?;
    let mut remaining = invocation.arguments.as_slice();
    while !remaining.is_empty() {
        let group = first_raw_command_group(remaining);
        remaining = &remaining[group.len()..];
        let assignment_count = group
            .iter()
            .take_while(|argument| is_parse_time_assignment(argument))
            .count();
        let command_args = std::iter::once(OsString::from("rmux"))
            .chain(group[assignment_count..].iter().map(OsString::from));
        let cli = parse(command_args).ok()?;
        if let Some(command) = cli.command {
            if super::command_has_start_server_flag(&command) {
                return Some(command);
            }
        }
    }
    None
}

fn first_raw_command_group(arguments: &[String]) -> &[String] {
    let end = arguments
        .iter()
        .position(|argument| {
            argument
                .strip_suffix(';')
                .is_some_and(|base| !base.ends_with('\\'))
        })
        .map_or(arguments.len(), |index| index + 1);
    &arguments[..end]
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

fn invocation_is_intrinsically_local(invocation: &RuntimeCommandInvocation) -> bool {
    super::command_inventory::invocation_is_local(&invocation.groups, invocation.control_mode)
}

fn resolve_runtime_command_with_connection(
    connection: &mut Connection,
    socket_path: &Path,
    invocation: RuntimeCommandInvocation,
) -> Result<Option<RuntimeCommandResolution>, ExitFailure> {
    let expansion = expand_runtime_command_segment(connection, &invocation.arguments);
    resolve_expanded_runtime_command(connection, socket_path, invocation, expansion)
}

fn resolve_expanded_runtime_command(
    connection: &mut Connection,
    socket_path: &Path,
    invocation: RuntimeCommandInvocation,
    expansion: Result<Option<String>, RuntimeCommandExpansionError>,
) -> Result<Option<RuntimeCommandResolution>, ExitFailure> {
    let RuntimeCommandInvocation {
        arguments,
        groups,
        control_mode,
    } = invocation;
    let legacy_kill_fallback = queue_starts_with_kill_server(&groups);

    let canonical = match expansion {
        Ok(Some(canonical)) => canonical,
        Ok(None) if control_mode != 0 => return Ok(None),
        Ok(None) => {
            let aliases = server_command_alias_definitions(connection)
                .map_err(|error| error.with_socket_context(socket_path))?;
            if !queue_uses_server_alias(&groups, &aliases) {
                return Ok(Some(RuntimeCommandResolution::LegacyDirect));
            }
            let exit_code = run_raw_command_through_server_with_aliases(
                &arguments,
                socket_path,
                connection,
                aliases,
            )
            .map_err(|error| error.with_socket_context(socket_path))?;
            return Ok(Some(RuntimeCommandResolution::LegacyServerDispatch(
                exit_code,
            )));
        }
        Err(error) if legacy_kill_fallback && error.previous_wire_version().is_some() => {
            // The probe found a legacy daemon. Keep that resolution so a later
            // server-starting command cannot trigger cold startup before the
            // leading kill-server reaches its existing wire fallback.
            return Ok(Some(RuntimeCommandResolution::LegacyDirect));
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

    if control_mode == 0 {
        if let Some(status) =
            source_file_input::discover(connection, &canonical, arguments.clone())?
        {
            return Ok(Some(RuntimeCommandResolution::LegacyServerDispatch(status)));
        }
        if let Some(status) = new_pane::discover(connection, &canonical, arguments.clone())? {
            return Ok(Some(RuntimeCommandResolution::LegacyServerDispatch(status)));
        }
        if let Some(status) =
            resize_pane_position::discover(connection, &canonical, arguments.clone())?
        {
            return Ok(Some(RuntimeCommandResolution::LegacyServerDispatch(status)));
        }
        if let Some(status) =
            set_pane_placement::discover(connection, &canonical, arguments.clone())?
        {
            return Ok(Some(RuntimeCommandResolution::LegacyServerDispatch(status)));
        }
    }
    if queue_has_builtin_after_parse_time_assignment(&groups) {
        let aliases = server_command_alias_definitions(connection)
            .map_err(|error| error.with_socket_context(socket_path))?;
        if queue_requires_direct_parse_for_builtin_assignment(&groups, &aliases) {
            return Ok(Some(RuntimeCommandResolution::LegacyDirect));
        }
    }

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

fn queue_has_builtin_after_parse_time_assignment(groups: &[Vec<String>]) -> bool {
    groups.iter().any(|group| {
        let Some(first) = group.first() else {
            return false;
        };
        if !is_parse_time_assignment(first) {
            return false;
        }
        let Some(command_name) = alias_lookup_command_name(group) else {
            return true;
        };
        has_tmux_command_candidate(command_name)
    })
}

fn queue_requires_direct_parse_for_builtin_assignment(
    groups: &[Vec<String>],
    aliases: &[String],
) -> bool {
    queue_has_builtin_after_parse_time_assignment(groups)
        && !queue_uses_server_alias(groups, aliases)
}

fn alias_lookup_command_name(group: &[String]) -> Option<&str> {
    group
        .iter()
        .find(|argument| !is_parse_time_assignment(argument))
        .map(String::as_str)
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
#[path = "alias_fallback_tests.rs"]
mod tests;
