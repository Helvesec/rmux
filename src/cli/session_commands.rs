use std::path::{Path, PathBuf};

use rmux_client::connect;
use rmux_client::{detect_context, detect_parent, ClientContext, ClientContextParent};
use rmux_proto::request::{AttachSessionExt2Request, SwitchClientExt3Request};
use rmux_proto::request::{KillSessionRequest, ListSessionsRequest, NewSessionExtRequest};
use rmux_proto::{ClientTerminalContext, ErrorResponse, ProcessCommand, Response};

use super::json_output::{list_sessions_json_format, write_list_sessions_json};
use super::{
    attach_with_connection, current_terminal_size, require_attach_terminal,
    run_switch_client_on_connection,
};
use super::{
    build_terminal_size, connect_with_startserver, expect_command_success, list_session_names,
    optional_client_flags, resolve_current_session_target, resolve_session_target_or_current,
    resolve_session_target_spec, run_command_resolved, run_payload_command, unexpected_response,
    write_command_output, ExitFailure, StartupOptions,
};
use crate::cli_args::{
    parse_target_spec, HasSessionArgs, KillSessionArgs, ListSessionsArgs, NewSessionArgs,
    RenameSessionArgs,
};

pub(super) fn run_new_session(
    args: NewSessionArgs,
    socket_path: &Path,
    startup: StartupOptions,
    client_terminal: ClientTerminalContext,
) -> Result<i32, ExitFailure> {
    validate_new_session_size(args.cols, args.rows)?;

    if !args.detached && detect_parent() == ClientContextParent::Tmux {
        return Err(ExitFailure::new(
            1,
            "sessions should be nested with care, unset $TMUX to force",
        ));
    }

    let client_context = detect_context();
    let mut connection = connect_with_startserver(socket_path, startup)?;
    if !args.detached && client_context == ClientContext::Outside {
        reject_existing_session_before_attach_preflight(&args, &mut connection)?;
        require_attach_terminal()?;
    }

    let client_size = if !args.detached && client_context == ClientContext::Outside {
        current_terminal_size()
    } else {
        None
    };
    let status = if client_size.is_some() {
        let response = connection
            .show_options(
                rmux_proto::OptionScopeSelector::SessionGlobal,
                Some("status".to_owned()),
                true,
                false,
                false,
            )
            .map_err(ExitFailure::from_client)?;
        let output = super::expect_command_output(&response, "show-options")?;
        String::from_utf8_lossy(output.stdout()).trim().to_owned()
    } else {
        String::new()
    };
    let size = new_session_initial_size(args.cols, args.rows, client_size, &status);
    let client_flags = optional_client_flags(args.flags.clone());
    let working_directory = args
        .working_directory
        .or_else(current_working_directory_string);
    let response = connection
        .new_session_extended(NewSessionExtRequest {
            session_name: args.session_name.clone(),
            detached: args.detached,
            size,
            environment: (!args.environment.is_empty()).then_some(args.environment),
            group_target: args.group_target,
            working_directory,
            attach_if_exists: args.attach_if_exists,
            detach_other_clients: args.detach_other_clients || args.kill_other_clients,
            kill_other_clients: args.kill_other_clients,
            flags: client_flags.clone(),
            window_name: args.window_name,
            print_session_info: args.print_session_info,
            print_format: args.print_format,
            process_command: ProcessCommand::from_legacy_command(Some(&args.command)),
            command: (!args.command.is_empty()).then_some(args.command),
            client_environment: invoking_client_environment(),
            skip_environment_update: args.skip_environment_update,
        })
        .map_err(ExitFailure::from_client)?;
    let output = response.command_output().cloned();
    let (target, detached) = match response {
        Response::NewSession(response) => (response.session_name, response.detached),
        other => {
            expect_command_success(other, "new-session")?;
            unreachable!("new-session success must return a new-session response")
        }
    };

    if let Some(output) = output {
        write_command_output(&output)?;
    }

    if detached {
        return Ok(0);
    }

    match client_context {
        ClientContext::Nested => run_switch_client_on_connection(
            &mut connection,
            SwitchClientExt3Request {
                target_client: None,
                target: Some(target.to_string()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: false,
            },
        ),
        ClientContext::Outside => attach_with_connection(
            connection,
            AttachSessionExt2Request {
                target: Some(target.clone()),
                target_spec: Some(target.to_string()),
                detach_other_clients: false,
                kill_other_clients: false,
                read_only: false,
                skip_environment_update: false,
                flags: client_flags,
                working_directory: None,
                client_terminal,
                client_size: current_terminal_size(),
            },
        ),
    }
}

fn reject_existing_session_before_attach_preflight(
    args: &NewSessionArgs,
    connection: &mut rmux_client::Connection,
) -> Result<(), ExitFailure> {
    let Some(session_name) = args
        .session_name
        .as_ref()
        .filter(|_| !args.attach_if_exists)
    else {
        return Ok(());
    };
    let response = connection
        .has_session(session_name.clone())
        .map_err(ExitFailure::from_client)?;
    match response {
        Response::HasSession(response) if response.exists => Err(ExitFailure::new(
            1,
            format!("duplicate session: {session_name}"),
        )),
        Response::HasSession(_) => Ok(()),
        Response::Error(ErrorResponse { error }) => Err(ExitFailure::new(1, error.to_string())),
        other => Err(unexpected_response("has-session", &other)),
    }
}

fn new_session_initial_size(
    cols: Option<u16>,
    rows: Option<u16>,
    client_size: Option<rmux_proto::TerminalSize>,
    status: &str,
) -> Option<rmux_proto::TerminalSize> {
    let Some(mut size) = client_size else {
        return build_terminal_size(cols, rows);
    };
    // NewSessionExt.size is pane geometry. Reserve the new session's status
    // before the first PTY/shell is created, not during the later attach.
    // Like tmux, an outside attached client takes precedence over -x/-y.
    let status_rows = if status == "off" {
        0
    } else {
        status.parse::<u16>().unwrap_or(1).max(1)
    };
    size.rows = size.rows.saturating_sub(status_rows).max(1);
    Some(size)
}

fn validate_new_session_size(cols: Option<u16>, rows: Option<u16>) -> Result<(), ExitFailure> {
    if cols == Some(0) {
        return Err(ExitFailure::new(1, "width too small"));
    }
    if rows == Some(0) {
        return Err(ExitFailure::new(1, "height too small"));
    }
    Ok(())
}

fn current_working_directory_string() -> Option<String> {
    current_working_directory().map(|path| path.to_string_lossy().into_owned())
}

#[cfg(windows)]
fn invoking_client_environment() -> Option<Vec<String>> {
    let shell = crate::windows_shell::invoking_client_shell().or_else(|| {
        crate::helper_handoff::launcher_path()?;
        std::env::var("RMUX_INTERNAL_CLIENT_SHELL")
            .ok()
            .filter(|shell| !shell.is_empty())
    });
    Some(crate::windows_shell::invoking_client_environment(
        std::env::vars_os(),
        shell,
    ))
}

#[cfg(not(windows))]
fn invoking_client_environment() -> Option<Vec<String>> {
    None
}

fn current_working_directory() -> Option<PathBuf> {
    std::env::current_dir().ok().filter(|path| path.is_dir())
}

pub(super) fn run_has_session(
    args: HasSessionArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    // tmux requires a current target before resolving even an explicit
    // session target. Command options have already been validated by the CLI.
    if list_session_names(&mut connection)?.is_empty() {
        return Err(ExitFailure::new(1, "no current target"));
    }
    let missing_message = args
        .target
        .as_ref()
        .map(|target| format!("can't find session: {target}"))
        .unwrap_or_else(|| "can't find session".to_owned());
    let target = match args.target.as_deref() {
        None | Some("") => resolve_current_session_target(&mut connection),
        Some(raw_target) => {
            let target = parse_target_spec(raw_target).map_err(|message| {
                ExitFailure::from_clap(clap::Error::raw(
                    clap::error::ErrorKind::ValueValidation,
                    message,
                ))
            })?;
            resolve_session_target_spec(&mut connection, &target, false)
        }
    }
    .map_err(|error| map_has_session_lookup_error(error, args.target.as_deref().unwrap_or("")))?;
    let response = connection
        .has_session(target)
        .map_err(ExitFailure::from_client)?;

    match response {
        Response::HasSession(response) => {
            if response.exists {
                Ok(0)
            } else {
                Err(ExitFailure::new(1, missing_message))
            }
        }
        Response::Error(ErrorResponse { error }) => Err(ExitFailure::new(1, error.to_string())),
        other => Err(unexpected_response("has-session", &other)),
    }
}

fn map_has_session_lookup_error(error: ExitFailure, raw_target: &str) -> ExitFailure {
    if error.message() == "server error: no current target" {
        return ExitFailure::new(1, "no current target");
    }
    if error.message().contains("ambiguous session match") {
        return ExitFailure::new(1, format!("can't find session: {raw_target}"));
    }
    normalize_session_lookup_error(error, "can't find session: {}")
}

pub(super) fn run_kill_session(
    args: KillSessionArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let target =
        resolve_session_target_or_current(&mut connection, args.target.as_ref(), "kill-session")
            .map_err(map_kill_session_lookup_error)?;
    let response = connection
        .kill_session(KillSessionRequest {
            target,
            kill_all_except_target: args.kill_all_except_target,
            clear_alerts: args.clear_alerts,
            kill_group: args.kill_group,
        })
        .map_err(ExitFailure::from_client)?;
    expect_command_success(response, "kill-session")?;
    Ok(0)
}

fn map_kill_session_lookup_error(error: ExitFailure) -> ExitFailure {
    normalize_session_lookup_error(error, "can't find session: {}")
}

fn normalize_session_lookup_error(error: ExitFailure, format: &str) -> ExitFailure {
    const PREFIX: &str = "can't find session: ";

    if let Some((_, session_name)) = error.message().split_once(PREFIX) {
        return ExitFailure::new(1, format.replace("{}", session_name));
    }

    error
}

pub(super) fn run_rename_session(
    args: RenameSessionArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "rename-session", move |connection| {
        let target =
            resolve_session_target_or_current(connection, args.target.as_ref(), "rename-session")?;
        connection
            .rename_session(target, args.new_name)
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_list_sessions(
    args: ListSessionsArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    if args.json {
        let mut connection = connect(socket_path)
            .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
        let response = connection
            .list_sessions(ListSessionsRequest {
                format: Some(list_sessions_json_format()),
                filter: args.filter,
                sort_order: args.sort_order,
                reversed: args.reversed,
            })
            .map_err(ExitFailure::from_client)?;
        let output = super::expect_command_output(&response, "list-sessions")?;
        return write_list_sessions_json(output);
    }

    run_payload_command(socket_path, "list-sessions", move |connection| {
        connection.list_sessions(ListSessionsRequest {
            format: args.format,
            filter: args.filter,
            sort_order: args.sort_order,
            reversed: args.reversed,
        })
    })
}

pub(super) fn configured_default_client_commands(
    connection: &mut rmux_client::Connection,
) -> Result<Vec<crate::cli_args::Command>, ExitFailure> {
    let response = connection
        .show_options(
            rmux_proto::OptionScopeSelector::ServerGlobal,
            Some("default-client-command".into()),
            true,
            false,
            false,
        )
        .map_err(ExitFailure::from_client)?;
    let output = super::expect_command_output(&response, "show-options")?;
    let text = std::str::from_utf8(output.stdout())
        .map_err(|error| ExitFailure::new(1, error.to_string()))?;
    let aliases = super::alias_fallback::server_command_alias_definitions(connection)?;
    default_client_commands_from_value(text.trim_end_matches('\n'), aliases)
}

fn default_client_commands_from_value(
    value: &str,
    aliases: Vec<String>,
) -> Result<Vec<crate::cli_args::Command>, ExitFailure> {
    let parsed = rmux_core::command_parser::CommandParser::new()
        .with_command_aliases(aliases)
        .parse(value)
        .map_err(|error| ExitFailure::new(1, error.to_string()))?;
    crate::cli_args::parse_with_runtime_command_groups(
        ["rmux"],
        &[crate::cli_args::RuntimeCommandGroup::Canonical(
            parsed.to_tmux_reparse_string(),
        )],
    )
    .map(crate::cli_args::Cli::into_command_queue)
    .map_err(ExitFailure::from_clap)
}

#[cfg(test)]
pub(super) fn default_client_command() -> crate::cli_args::Command {
    default_client_commands_from_value("new-session", Vec::new())
        .unwrap()
        .remove(0)
}

#[cfg(test)]
#[path = "session_commands/tests.rs"]
mod tests;
