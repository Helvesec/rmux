//! Public CLI dispatch for the RMUX binary.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

mod alias_fallback;
mod attach_transport;
mod automation;
mod buffer_commands;
mod capabilities;
mod capture_pane;
mod claude_launcher;
mod claude_skill;
mod client_commands;
mod command_inventory;
mod command_runner;
mod config_commands;
mod control_mode_error;
mod diagnose;
mod dispatch;
mod display_panes;
mod error;
mod format_print;
mod json_output;
mod key_commands;
mod message_commands;
mod pane_commands;
mod scripting_contract;
mod server_commands;
mod session_commands;
mod shell_startup;
mod startup;
mod target_resolution;
mod terminal_size;
mod terminal_theme;
mod tmux_dropin;
mod top_level;
mod web_commands;
mod web_share_display;
mod window_commands;
mod window_snapshot;

use crate::cli_args::{parse, parse_with_runtime_command_groups, scan_top_level_command, Cli};
use crate::cli_response::{expect_command_output, expect_command_success};
use attach_transport::{attach_with_connection, require_attach_terminal};
use client_commands::{
    client_terminal_context_from_cli, optional_client_flags, run_control_mode, run_detach_client,
    run_list_clients, run_refresh_client, run_suspend_client, run_switch_client,
};
use client_commands::{run_switch_client_on_connection, validate_nested_attach_before_connect};
#[cfg(test)]
use command_inventory::render_list_commands_line;
pub(crate) use command_runner::{
    capture_target_action_needs_legacy_retry, cli_target_actions_enabled, run_command,
    run_command_resolved, run_payload_command, run_payload_command_resolved,
    target_action_needs_legacy_retry,
};
use command_runner::{
    finish_command_success, unexpected_response, write_command_output, write_lines_output,
};
use control_mode_error::parse_failure as control_mode_parse_failure;
#[cfg(test)]
use dispatch::default_client_command;
use dispatch::{command_has_start_server_flag, dispatch_command_queue};
pub(crate) use error::{ExitFailure, ExitMessageTermination};
use rmux_client::{
    connect, ensure_server_running_with_config_outcome, resolve_socket_path,
    resolve_tmux_compatible_socket_path, Connection,
};
use shell_startup::run_shell_startup;
#[cfg(test)]
use shell_startup::same_file_identity_for_paths;
#[cfg(test)]
use startup::ServerStartupConfig;
use startup::{
    run_foreground_server, server_starting_command, startup_config_from_cli,
    startup_config_from_top_level_scan, StartServerConnection, StartupEndpoint, StartupOptions,
};
use target_resolution::{
    list_session_names, listed_pane_index_matches_target, resolve_current_pane_target,
    resolve_current_session_target, resolve_existing_window_target_or_current,
    resolve_pane_target_or_current, resolve_pane_target_spec, resolve_session_listing_target,
    resolve_session_target_or_current, resolve_session_target_spec,
    resolve_split_window_target_spec, resolve_target_spec,
    resolve_window_index_target_or_current_session, resolve_window_target_or_current,
    resolve_window_target_spec, response_name_for_target,
};
use terminal_size::{build_terminal_size, current_terminal_size};
use top_level::{
    accept_compatibility_options, infer_client_utf8_from_env, scan_claude_top_level_invocation,
    top_level_decision, validate_claude_top_level_invocation, validate_top_level_invocation,
};

const TMUX_COMPAT_OVERRIDE_ENV: &str = "RMUX_INTERNAL_INVOKED_AS_TMUX";

mod invocation;
pub(crate) use invocation::run;

enum ColdAliasParseOutcome {
    NotApplicable(clap::Error),
    Parsed(Box<Cli>, StartupEndpoint),
    Dispatched(i32),
}

fn parse_cold_alias_queue_after_startup(
    args: &[OsString],
    original_error: clap::Error,
) -> Result<ColdAliasParseOutcome, ExitFailure> {
    let Ok(scan) = scan_top_level_command(args.get(1..).unwrap_or(&[])) else {
        return Ok(ColdAliasParseOutcome::NotApplicable(original_error));
    };
    if scan.control_mode != 0
        || scan.no_fork
        || scan.no_start_server
        || scan.shell_command.is_some()
    {
        return Ok(ColdAliasParseOutcome::NotApplicable(original_error));
    }
    let Some(first_command) = alias_fallback::first_cold_start_command(args) else {
        return Ok(ColdAliasParseOutcome::NotApplicable(original_error));
    };
    if !command_has_start_server_flag(&first_command) {
        return Ok(ColdAliasParseOutcome::NotApplicable(original_error));
    }

    let socket_path = if invoked_as_tmux(args) {
        resolve_tmux_compatible_socket_path(
            scan.socket_name.as_deref(),
            scan.socket_path.as_deref().map(Path::new),
        )
    } else {
        resolve_socket_path(
            scan.socket_name.as_deref(),
            scan.socket_path.as_deref().map(Path::new),
        )
    }
    .map_err(ExitFailure::from_client)?;
    let startup_config = startup_config_from_top_level_scan(&scan, &first_command);
    let outcome =
        ensure_server_running_with_config_outcome(&socket_path, startup_config.auto_start)
            .map_err(ExitFailure::from_auto_start)
            .map_err(|error| error.with_socket_context(&socket_path))?;
    let endpoint = StartupEndpoint::prestarted(outcome);
    let selected_socket_path = endpoint.socket_path();
    let resolution = endpoint.with_connection_mut(|connection| {
        alias_fallback::runtime_command_resolution_after_startup(
            args,
            &selected_socket_path,
            connection,
        )
    })?;
    let Some(resolution) = resolution else {
        return Err(ExitFailure::from_clap(original_error));
    };
    if let alias_fallback::RuntimeCommandResolution::LegacyServerDispatch(exit_code) = resolution {
        return Ok(ColdAliasParseOutcome::Dispatched(exit_code));
    }
    let cli =
        parse_with_runtime_resolution(args, Some(&resolution)).map_err(ExitFailure::from_clap)?;
    Ok(ColdAliasParseOutcome::Parsed(Box::new(cli), endpoint))
}

fn parse_with_runtime_resolution(
    args: &[OsString],
    resolution: Option<&alias_fallback::RuntimeCommandResolution>,
) -> Result<Cli, clap::Error> {
    match resolution {
        Some(alias_fallback::RuntimeCommandResolution::Canonical(groups)) => {
            parse_with_runtime_command_groups(args.to_vec(), groups)
        }
        Some(alias_fallback::RuntimeCommandResolution::LegacyDirect) | None => parse(args.to_vec()),
        Some(alias_fallback::RuntimeCommandResolution::LegacyServerDispatch(_)) => unreachable!(),
    }
}

fn invoked_as_tmux(args: &[OsString]) -> bool {
    invoked_as_tmux_argv0(args) || internal_tmux_compat_override()
}

fn invoked_as_tmux_argv0(args: &[OsString]) -> bool {
    args.first()
        .and_then(|arg| std::path::Path::new(arg).file_stem())
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("tmux"))
}

fn internal_tmux_compat_override() -> bool {
    env::var_os(TMUX_COMPAT_OVERRIDE_ENV)
        .as_deref()
        .is_some_and(|value| value == "1")
}

fn parse_failure_or_absent_server(
    args: &[OsString],
    error: clap::Error,
) -> Result<i32, ExitFailure> {
    if !parse_failure_should_probe_server(args, &error) {
        return Err(ExitFailure::from_clap(error));
    }

    let Some((socket_name, socket_path)) = recover_socket_selection(args.get(1..).unwrap_or(&[]))
    else {
        return Err(ExitFailure::from_clap(error));
    };
    let resolved = if invoked_as_tmux(args) {
        resolve_tmux_compatible_socket_path(socket_name.as_deref(), socket_path.as_deref())
    } else {
        resolve_socket_path(socket_name.as_deref(), socket_path.as_deref())
    }
    .map_err(ExitFailure::from_client)?;

    match connect(&resolved) {
        Ok(mut connection) if error.kind() == clap::error::ErrorKind::InvalidSubcommand => {
            alias_fallback::run_unknown_command_through_server_aliases(
                args,
                &resolved,
                &mut connection,
            )
            .map_err(|error| error.with_socket_context(&resolved))
        }
        Ok(_) => Err(ExitFailure::from_clap(error)),
        Err(connect_error) => Err(ExitFailure::from_client_connect(&resolved, connect_error)),
    }
}

fn parse_failure_should_probe_server(args: &[OsString], error: &clap::Error) -> bool {
    if matches!(
        error.kind(),
        clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
    ) {
        return false;
    }

    if error.kind() == clap::error::ErrorKind::InvalidSubcommand {
        return first_command_token(args.get(1..).unwrap_or(&[])).is_some();
    }

    if error.kind() == clap::error::ErrorKind::ValueValidation {
        let message = error.to_string();
        if message.contains("command too long") {
            return first_command_token(args.get(1..).unwrap_or(&[])).is_some();
        }
        if message.contains("expects an argument") {
            return first_command_token(args.get(1..).unwrap_or(&[]))
                .is_some_and(|command| matches!(command.as_str(), "new-session" | "new"));
        }
        return first_command_token(args.get(1..).unwrap_or(&[]))
            .is_some_and(|command| matches!(command.as_str(), "resize-pane" | "resizep"));
    }

    first_command_token(args.get(1..).unwrap_or(&[])).is_some_and(|command| {
        matches!(
            command.as_str(),
            "new-session" | "new" | "resize-pane" | "resizep"
        )
    })
}

fn recover_socket_selection(arguments: &[OsString]) -> Option<(Option<OsString>, Option<PathBuf>)> {
    let mut socket_name = None;
    let mut socket_path = None;
    let mut index = 0;

    while index < arguments.len() {
        let argument = arguments[index].to_str()?;
        if argument == "--" {
            break;
        }
        if !argument.starts_with('-') || argument == "-" {
            break;
        }

        match argument {
            "-L" => {
                index += 1;
                socket_name = arguments.get(index).cloned();
            }
            "-S" => {
                index += 1;
                socket_path = arguments.get(index).cloned().map(PathBuf::from);
            }
            "-c" | "-f" | "-T" => {
                index += 1;
            }
            value if value.starts_with("-L") && value.len() > 2 => {
                socket_name = Some(OsString::from(&value[2..]));
            }
            value if value.starts_with("-S") && value.len() > 2 => {
                socket_path = Some(PathBuf::from(&value[2..]));
            }
            _ => {}
        }
        index += 1;
    }

    Some((socket_name, socket_path))
}

fn first_command_token(arguments: &[OsString]) -> Option<String> {
    let mut index = 0;

    while index < arguments.len() {
        let argument = arguments[index].to_str()?;
        if argument == "--" {
            return arguments.get(index + 1)?.to_str().map(str::to_owned);
        }
        if !argument.starts_with('-') || argument == "-" {
            return Some(argument.to_owned());
        }

        if matches!(argument, "-c" | "-f" | "-L" | "-S" | "-T") {
            index += 1;
        }
        index += 1;
    }

    None
}

fn connect_with_startserver(
    socket_path: &Path,
    startup: StartupOptions,
) -> Result<Connection, ExitFailure> {
    connect_with_startserver_outcome(socket_path, startup)
        .map(StartServerConnection::into_connection)
}

impl StartServerConnection {
    fn into_connection(self) -> Connection {
        self.connection
    }
}

/// Connects the next queued command, auto-starting the daemon when allowed.
///
/// The startup connection opened before dispatch is handed to the first
/// command that needs one; leaving it open in parallel would register a second
/// idle client and permanently cancel the daemon's exit-empty shutdown. Its
/// provenance is kept on the shared startup endpoint, so a later attach still
/// knows that this invocation started the daemon.
fn connect_with_startserver_outcome(
    socket_path: &Path,
    startup: StartupOptions,
) -> Result<StartServerConnection, ExitFailure> {
    let StartupOptions {
        no_start_server,
        config,
        endpoint,
        automation_deadline: _,
    } = startup;
    if no_start_server {
        let connection = connect(socket_path)
            .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
        return Ok(StartServerConnection {
            connection,
            provenance: endpoint.provenance(),
        });
    }
    if let Some(connection) = endpoint.take_connection() {
        return Ok(StartServerConnection {
            connection,
            provenance: endpoint.provenance(),
        });
    }
    let outcome = ensure_server_running_with_config_outcome(socket_path, config)
        .map_err(ExitFailure::from_auto_start)?;
    endpoint.record_ensured(outcome.socket_path(), outcome.provenance());
    Ok(StartServerConnection {
        connection: outcome.into_connection(),
        provenance: endpoint.provenance(),
    })
}

fn shell_command_text(command: Vec<String>) -> String {
    if command.len() == 1 {
        return command.into_iter().next().expect("single shell token");
    }

    command
        .into_iter()
        .map(shell_command_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_command_token(token: String) -> String {
    format!("'{}'", token.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod diagnostic_prefix_tests;
