use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use rmux_client::{
    connect, detect_context, drive_control_mode, ClientContext, ClientError, Connection,
    ControlModeStatus, ControlTransition,
};
use rmux_proto::request::{
    AttachSessionExt2Request, DetachClientExtRequest, ListClientsRequest, RefreshClientRequest,
    SuspendClientRequest, SwitchClientExt3Request,
};
use rmux_proto::{ClientTerminalContext, ControlMode, ErrorResponse, Response};

use super::attach_transport::{
    attach_with_connection, begin_queued_attach, QueuedAttachSessionResult,
};
use super::json_output::{list_clients_json_format, write_list_clients_json};
use super::{
    connect_with_startserver_outcome, current_terminal_size, expect_command_success,
    finish_command_success, list_session_names, resolve_session_target_spec, run_command,
    run_payload_command_resolved, unexpected_response, ExitFailure, StartupOptions,
};
use crate::cli_args::{
    AttachSessionArgs, Cli, DetachClientArgs, ListClientsArgs, RefreshClientArgs,
    SuspendClientArgs, SwitchClientArgs,
};
use crate::client_terminal::client_terminal_context_from_parts;
use crate::empty_server_lifecycle::shutdown_started_empty_server_at;

pub(super) fn client_terminal_context_from_cli(cli: &Cli) -> ClientTerminalContext {
    let mut terminal_features = cli
        .terminal_features()
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|feature| !feature.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if cli.assume_256_colors {
        terminal_features.push("256".to_owned());
    }

    client_terminal_context_from_parts(terminal_features, cli.utf8)
}

struct PreparedAttachSession {
    connection: Connection,
    request: AttachSessionExt2Request,
    nested_context: bool,
    nested_target: Option<String>,
    nested_skip_environment_update: bool,
    nested_toggle_read_only: bool,
}

pub(super) fn run_attach_session(
    args: AttachSessionArgs,
    socket_path: &Path,
    startup: StartupOptions,
    client_terminal: ClientTerminalContext,
) -> Result<i32, ExitFailure> {
    let prepared = prepare_attach_session(args, socket_path, startup, client_terminal)?;
    if prepared.nested_context {
        return run_nested_attach_session_as_switch_client(prepared);
    }

    let PreparedAttachSession {
        connection,
        request,
        ..
    } = prepared;
    attach_with_connection(connection, request)
}

pub(super) fn run_attach_session_queued(
    args: AttachSessionArgs,
    socket_path: &Path,
    startup: StartupOptions,
    client_terminal: ClientTerminalContext,
) -> Result<QueuedAttachSessionResult, ExitFailure> {
    let prepared = prepare_attach_session(args, socket_path, startup, client_terminal)?;
    if prepared.nested_context {
        return run_nested_attach_session_as_switch_client(prepared)
            .map(QueuedAttachSessionResult::Completed);
    }
    let PreparedAttachSession {
        connection,
        request,
        ..
    } = prepared;
    begin_queued_attach(connection, request)
}

fn prepare_attach_session(
    args: AttachSessionArgs,
    socket_path: &Path,
    startup: StartupOptions,
    client_terminal: ClientTerminalContext,
) -> Result<PreparedAttachSession, ExitFailure> {
    let nested_context = validate_nested_attach_before_connect(&args, socket_path)?;
    let nested_target = args.target.as_ref().map(ToString::to_string);
    let target_spec = args.target.as_ref().map(ToString::to_string);
    let nested_skip_environment_update = args.skip_environment_update;
    let nested_toggle_read_only = args.read_only;
    let outcome = connect_with_startserver_outcome(socket_path, startup)?;
    let started_by_caller =
        outcome.provenance == rmux_client::ServerConnectionProvenance::StartedByCaller;
    let mut connection = outcome.into_connection();
    if list_session_names(&mut connection)?.is_empty() {
        if started_by_caller {
            let _ = connection.shutdown_if_idle();
        }
        return Err(ExitFailure::new(1, "no sessions"));
    }
    let target = args
        .target
        .as_ref()
        .map(|target| resolve_session_target_spec(&mut connection, target, false))
        .transpose()?;
    let request = AttachSessionExt2Request {
        target,
        target_spec,
        detach_other_clients: args.detach_other_clients || args.kill_other_clients,
        kill_other_clients: args.kill_other_clients,
        read_only: args.read_only,
        skip_environment_update: args.skip_environment_update,
        flags: optional_client_flags(args.flags),
        working_directory: args.working_directory,
        client_terminal,
        client_size: current_terminal_size(),
    };

    Ok(PreparedAttachSession {
        connection,
        request,
        nested_context,
        nested_target,
        nested_skip_environment_update,
        nested_toggle_read_only,
    })
}

pub(super) fn validate_nested_attach_before_connect(
    args: &AttachSessionArgs,
    socket_path: &Path,
) -> Result<bool, ExitFailure> {
    let nested_context =
        detect_context() == ClientContext::Nested && inherited_rmux_socket_matches(socket_path);
    if nested_context {
        validate_nested_attach_session(args)?;
    }
    Ok(nested_context)
}

fn run_nested_attach_session_as_switch_client(
    prepared: PreparedAttachSession,
) -> Result<i32, ExitFailure> {
    let PreparedAttachSession {
        mut connection,
        nested_target,
        nested_skip_environment_update,
        nested_toggle_read_only,
        ..
    } = prepared;
    run_switch_client_on_connection(
        &mut connection,
        SwitchClientExt3Request {
            target_client: None,
            target: nested_target,
            key_table: None,
            last_session: false,
            next_session: false,
            previous_session: false,
            toggle_read_only: nested_toggle_read_only,
            sort_order: None,
            skip_environment_update: nested_skip_environment_update,
            zoom: false,
        },
    )
}

fn inherited_rmux_socket_matches(socket_path: &Path) -> bool {
    inherited_rmux_socket_matches_from_env(std::env::var_os("RMUX").as_deref(), socket_path)
}

fn inherited_rmux_socket_matches_from_env(rmux: Option<&OsStr>, socket_path: &Path) -> bool {
    let Some(inherited_socket) = rmux.and_then(rmux_socket_path_from_env) else {
        return false;
    };
    rmux_os::path::socket_paths_match(&inherited_socket, socket_path)
}

fn rmux_socket_path_from_env(value: &OsStr) -> Option<PathBuf> {
    let value = value.to_string_lossy();
    let path = value
        .split_once(',')
        .map_or(value.as_ref(), |(path, _)| path);
    (!path.is_empty()).then(|| PathBuf::from(path))
}

fn validate_nested_attach_session(args: &AttachSessionArgs) -> Result<(), ExitFailure> {
    let mut unsupported = Vec::new();
    if args.working_directory.is_some() {
        unsupported.push("-c");
    }
    if args.detach_other_clients {
        unsupported.push("-d");
    }
    if !args.flags.is_empty() {
        unsupported.push("-f");
    }
    if args.read_only {
        unsupported.push("-r");
    }
    if args.kill_other_clients {
        unsupported.push("-x");
    }

    if !unsupported.is_empty() {
        return Err(ExitFailure::new(
            1,
            format!(
                "attach-session inside an attached client supports only -E and -t; unsupported: {}",
                unsupported.join(", ")
            ),
        ));
    }

    if args.target.is_none() {
        return Err(ExitFailure::new(
            1,
            "attach-session inside an attached client requires -t",
        ));
    }

    Ok(())
}

pub(super) fn run_control_mode(
    cli: &Cli,
    socket_path: &Path,
    startup: StartupOptions,
) -> Result<i32, ExitFailure> {
    let endpoint = startup.endpoint.clone();
    let outcome = connect_with_startserver_outcome(socket_path, startup)?;
    let provenance = outcome.provenance;
    let connection = outcome.into_connection();
    match connection
        .begin_control_mode_with_initial_commands(
            ControlMode::from_count(cli.control_mode),
            client_terminal_context_from_cli(cli),
            cli.control_command_lines(),
        )
        .map_err(ExitFailure::from_client)?
    {
        ControlTransition::Upgraded(upgrade) => {
            let control_result = control_mode_exit_code(drive_control_mode(upgrade, &[]));
            let cleanup_result =
                shutdown_started_empty_server_at(&endpoint.socket_path(), provenance)
                    .map_err(|error| ExitFailure::new(1, error.to_string()));
            let exit_code = control_result?;
            cleanup_result?;
            Ok(exit_code)
        }
        ControlTransition::Rejected(Response::Error(ErrorResponse { error })) => {
            Err(ExitFailure::new(1, error.to_string()))
        }
        ControlTransition::Rejected(response) => {
            Err(unexpected_response("control-mode", &response))
        }
    }
}

/// Maps the end of a control session to tmux's exit status. A failed command
/// of a client that never joined a session exits 1, and so does a server that
/// vanished without `%exit`: the client has already written
/// `%exit server exited unexpectedly` to the control stream, as tmux does.
fn control_mode_exit_code(
    result: Result<ControlModeStatus, ClientError>,
) -> Result<i32, ExitFailure> {
    match result {
        Ok(ControlModeStatus::Success) => Ok(0),
        Ok(ControlModeStatus::CommandFailed) => Ok(1),
        Err(ClientError::UnexpectedEof) => Ok(1),
        Err(error) => Err(ExitFailure::from_client(error)),
    }
}

pub(super) fn run_switch_client(
    args: SwitchClientArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    run_switch_client_on_connection(
        &mut connection,
        SwitchClientExt3Request {
            target_client: args.target_client,
            target: args.target,
            key_table: args.key_table,
            last_session: args.last_session,
            next_session: args.next_session,
            previous_session: args.previous_session,
            toggle_read_only: args.toggle_read_only,
            sort_order: args.sort_order,
            skip_environment_update: args.skip_environment_update,
            zoom: args.zoom,
        },
    )
}

pub(super) fn run_switch_client_on_connection(
    connection: &mut Connection,
    request: SwitchClientExt3Request,
) -> Result<i32, ExitFailure> {
    let response = connection
        .switch_client_with_target_selector(request)
        .map_err(ExitFailure::from_client)?;
    expect_command_success(response, "switch-client")?;
    Ok(0)
}

pub(super) fn run_refresh_client(
    args: RefreshClientArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let adjustment =
        if args.clear_pan || args.pan_left || args.pan_right || args.pan_up || args.pan_down {
            args.adjustment
                .as_deref()
                .map(rmux_core::command_parser::parse_pan_adjustment)
                .transpose()
                .map_err(|error| {
                    ExitFailure::from_client(rmux_client::ClientError::Protocol(error))
                })?
        } else {
            None
        };
    run_command(socket_path, "refresh-client", move |connection| {
        connection.refresh_client(RefreshClientRequest {
            target_client: args.target_client,
            adjustment,
            clear_pan: args.clear_pan,
            pan_left: args.pan_left,
            pan_right: args.pan_right,
            pan_up: args.pan_up,
            pan_down: args.pan_down,
            status_only: args.status_only,
            clipboard_query: args.clipboard_query,
            flags: args.flags,
            flags_alias: args.flags_alias,
            subscriptions: args.subscriptions,
            subscriptions_format: args.subscriptions_format,
            control_size: args.control_size,
            colour_report: args.colour_report,
        })
    })
}

pub(super) fn run_list_clients(
    args: ListClientsArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    if args.json {
        let mut connection = connect(socket_path)
            .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
        let target_session = args
            .target_session
            .as_ref()
            .map(|target| resolve_session_target_spec(&mut connection, target, false))
            .transpose()?;
        let response = connection
            .list_clients(ListClientsRequest {
                format: Some(list_clients_json_format()),
                filter: args.filter,
                sort_order: args.sort_order,
                reversed: args.reversed,
                target_session,
            })
            .map_err(ExitFailure::from_client)?;
        return match response {
            Response::ListClients(response) => write_list_clients_json(&response),
            Response::Error(ErrorResponse { error }) => Err(ExitFailure::new(1, error.to_string())),
            other => Err(unexpected_response("list-clients", &other)),
        };
    }

    run_payload_command_resolved(socket_path, "list-clients", move |connection| {
        let target_session = args
            .target_session
            .as_ref()
            .map(|target| resolve_session_target_spec(connection, target, false))
            .transpose()?;
        connection
            .list_clients(ListClientsRequest {
                format: args.format,
                filter: args.filter,
                sort_order: args.sort_order,
                reversed: args.reversed,
                target_session,
            })
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_detach_client(
    args: DetachClientArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let target_session = args
        .target_session
        .as_ref()
        .map(|target| resolve_session_target_spec(&mut connection, target, false))
        .transpose()?;
    let response = connection
        .detach_client_extended(DetachClientExtRequest {
            target_client: args.target_client,
            all_other_clients: args.all_other_clients,
            target_session,
            kill_on_detach: args.kill_on_detach,
            exec_command: args.exec_command,
        })
        .map_err(ExitFailure::from_client)?;
    finish_command_success(response, "detach-client")
}

pub(super) fn run_suspend_client(
    args: SuspendClientArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command(socket_path, "suspend-client", move |connection| {
        connection.suspend_client(SuspendClientRequest {
            target_client: args.target_client,
        })
    })
}

pub(super) fn optional_client_flags(flags: Vec<String>) -> Option<Vec<String>> {
    (!flags.is_empty()).then_some(flags)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use super::inherited_rmux_socket_matches_from_env;

    #[cfg(unix)]
    #[test]
    fn nested_attach_matches_missing_bare_socket_spellings() {
        let name = format!("review-nested-{}.sock", std::process::id());
        let absolute = std::env::current_dir().unwrap().join(&name);
        assert!(!absolute.exists());
        for inherited in [&name, &format!("./{name}"), absolute.to_str().unwrap()] {
            let rmux = format!("{inherited},123,0");
            for requested in [Path::new(&name), &Path::new(".").join(&name), &absolute] {
                assert!(inherited_rmux_socket_matches_from_env(
                    Some(OsStr::new(&rmux)),
                    requested,
                ));
            }
        }
        assert!(!inherited_rmux_socket_matches_from_env(
            Some(OsStr::new(&format!("{name},123,0"))),
            &absolute.with_file_name("other.sock"),
        ));
    }

    #[test]
    fn lost_control_server_exits_one_without_a_second_diagnostic() {
        assert_eq!(
            super::control_mode_exit_code(Ok(rmux_client::ControlModeStatus::Success))
                .expect("clean end"),
            0
        );
        assert_eq!(
            super::control_mode_exit_code(Ok(rmux_client::ControlModeStatus::CommandFailed))
                .expect("a failed command is an exit status, not a stderr diagnostic"),
            1
        );
        assert_eq!(
            super::control_mode_exit_code(Err(rmux_client::ClientError::UnexpectedEof))
                .expect("lost server is an exit status, not a stderr diagnostic"),
            1
        );
        let failure = super::control_mode_exit_code(Err(rmux_client::ClientError::Io(
            std::io::Error::from(std::io::ErrorKind::InvalidData),
        )))
        .expect_err("other transport failures keep their diagnostic");
        assert_eq!(failure.exit_code(), 1);
        assert!(failure.use_stderr());
        assert!(!failure.message().is_empty());
    }

    #[test]
    fn nested_attach_only_rewrites_when_inherited_rmux_socket_matches() {
        assert!(inherited_rmux_socket_matches_from_env(
            Some(OsStr::new("/tmp/rmux-1000/default,123,0")),
            Path::new("/tmp/rmux-1000/default"),
        ));
        assert!(!inherited_rmux_socket_matches_from_env(
            Some(OsStr::new("/tmp/rmux-1000/default,123,0")),
            Path::new("/tmp/rmux-1000/other"),
        ));
        assert!(!inherited_rmux_socket_matches_from_env(
            None,
            Path::new("/tmp/rmux-1000/default"),
        ));
    }
}
