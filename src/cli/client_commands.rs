use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use rmux_client::attach_terminal_with_initial_bytes;
#[cfg(unix)]
use rmux_client::attach_terminal_with_initial_bytes_and_resize_geometry;
#[cfg(windows)]
use rmux_client::attach_terminal_with_initial_bytes_and_windows_console_key;
#[cfg(unix)]
use rmux_client::AttachError;
use rmux_client::{
    connect, detect_context, drive_control_mode, AttachTransition, ClientContext, ClientError,
    Connection, ControlTransition,
};
use rmux_proto::request::{
    AttachSessionExt2Request, AttachSessionExt3Request, DetachClientExtRequest, ListClientsRequest,
    RefreshClientRequest, SuspendClientRequest, SwitchClientExt3Request,
};
#[cfg(windows)]
use rmux_proto::CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY;
use rmux_proto::{
    ClientTerminalContext, ControlMode, ErrorResponse, Response, CAPABILITY_ATTACH_RENDER,
    CAPABILITY_ATTACH_RESIZE_GEOMETRY,
};

use super::json_output::{list_clients_json_format, write_list_clients_json};
use super::{
    connect_with_startserver, current_terminal_size, expect_command_success,
    finish_command_success, list_session_names, resolve_session_target_spec, run_command,
    run_payload_command_resolved, unexpected_response, ExitFailure, StartupOptions,
};
use crate::cli_args::{
    AttachSessionArgs, Cli, DetachClientArgs, ListClientsArgs, RefreshClientArgs,
    SuspendClientArgs, SwitchClientArgs,
};
use crate::client_terminal::client_terminal_context_from_parts;

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

pub(super) fn run_attach_session(
    args: AttachSessionArgs,
    socket_path: &Path,
    startup: StartupOptions,
    client_terminal: ClientTerminalContext,
) -> Result<i32, ExitFailure> {
    let nested_context =
        detect_context() == ClientContext::Nested && inherited_rmux_socket_matches(socket_path);
    if nested_context {
        validate_nested_attach_session(&args)?;
    }
    let nested_target = args.target.as_ref().map(ToString::to_string);
    let target_spec = args.target.as_ref().map(ToString::to_string);
    let nested_skip_environment_update = args.skip_environment_update;
    let nested_toggle_read_only = args.read_only;
    let mut connection = connect_with_startserver(socket_path, startup)?;
    if list_session_names(&mut connection)?.is_empty() {
        let _ = connection.kill_server();
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

    if nested_context {
        return run_switch_client_on_connection(
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
        );
    }

    attach_with_connection(connection, request)
}

fn inherited_rmux_socket_matches(socket_path: &Path) -> bool {
    inherited_rmux_socket_matches_from_env(std::env::var_os("RMUX").as_deref(), socket_path)
}

fn inherited_rmux_socket_matches_from_env(rmux: Option<&OsStr>, socket_path: &Path) -> bool {
    let Some(inherited_socket) = rmux.and_then(rmux_socket_path_from_env) else {
        return false;
    };
    socket_paths_match(&inherited_socket, socket_path)
}

fn rmux_socket_path_from_env(value: &OsStr) -> Option<PathBuf> {
    let value = value.to_string_lossy();
    let path = value
        .split_once(',')
        .map_or(value.as_ref(), |(path, _)| path);
    (!path.is_empty()).then(|| PathBuf::from(path))
}

fn socket_paths_match(left: &Path, right: &Path) -> bool {
    let left = canonical_socket_path(left);
    let right = canonical_socket_path(right);
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn canonical_socket_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(file_name)) => std::fs::canonicalize(parent)
            .map(|canonical_parent| canonical_parent.join(file_name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
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
    let connection = connect_with_startserver(socket_path, startup)?;
    match connection
        .begin_control_mode(
            ControlMode::from_count(cli.control_mode),
            client_terminal_context_from_cli(cli),
        )
        .map_err(ExitFailure::from_client)?
    {
        ControlTransition::Upgraded(upgrade) => {
            drive_control_mode(upgrade, cli.control_command_lines())
                .map_err(ExitFailure::from_client)?;
            Ok(0)
        }
        ControlTransition::Rejected(Response::Error(ErrorResponse { error })) => {
            Err(ExitFailure::new(1, error.to_string()))
        }
        ControlTransition::Rejected(response) => {
            Err(unexpected_response("control-mode", &response))
        }
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
    run_command(socket_path, "refresh-client", move |connection| {
        connection.refresh_client(RefreshClientRequest {
            target_client: args.target_client,
            adjustment: args.adjustment,
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
            colour_report: None,
        })
    })
}

pub(super) fn run_list_clients(
    args: ListClientsArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    if let Some(flag) = args.unsupported_flag() {
        return Err(ExitFailure::new(
            1,
            format!("command list-clients: unknown flag {flag}"),
        ));
    }
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
                sort_order: None,
                reversed: false,
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
                sort_order: None,
                reversed: false,
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

pub(super) fn attach_with_connection(
    mut connection: Connection,
    request: AttachSessionExt2Request,
) -> Result<i32, ExitFailure> {
    let attach_resize_geometry = connection
        .supports_capability(CAPABILITY_ATTACH_RESIZE_GEOMETRY)
        .map_err(ExitFailure::from_client)?;
    let attach_render = connection
        .supports_capability(CAPABILITY_ATTACH_RENDER)
        .map_err(ExitFailure::from_client)?;
    #[cfg(windows)]
    let attach_windows_console_key = connection
        .supports_capability(CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY)
        .map_err(ExitFailure::from_client)?;
    let transition = if attach_render {
        connection
            .begin_attach_with_capabilities(AttachSessionExt3Request::from_ext2(
                request,
                vec![CAPABILITY_ATTACH_RENDER.to_owned()],
            ))
            .map_err(ExitFailure::from_client)?
    } else {
        connection
            .begin_attach_with_target_spec(request)
            .map_err(ExitFailure::from_client)?
    };
    match transition {
        AttachTransition::Upgraded(upgrade) => {
            let (stream, initial_bytes) = upgrade.into_parts();
            #[cfg(unix)]
            {
                if attach_resize_geometry {
                    attach_terminal_with_initial_bytes_and_resize_geometry(stream, initial_bytes)
                        .map_err(attach_terminal_exit_failure)?;
                } else {
                    attach_terminal_with_initial_bytes(stream, initial_bytes)
                        .map_err(attach_terminal_exit_failure)?;
                }
            }
            #[cfg(windows)]
            {
                let _ = attach_resize_geometry;
                attach_terminal_with_initial_bytes_and_windows_console_key(
                    stream,
                    initial_bytes,
                    attach_windows_console_key,
                )
                .map_err(attach_terminal_exit_failure)?;
            }
            Ok(0)
        }
        AttachTransition::Rejected(response) => {
            expect_command_success(response, "attach-session")?;
            Ok(0)
        }
    }
}

fn attach_terminal_exit_failure(error: ClientError) -> ExitFailure {
    if attach_terminal_failed_because_stdio_is_not_terminal(&error) {
        ExitFailure::new(1, "open terminal failed: not a terminal")
    } else {
        ExitFailure::from_client(error)
    }
}

#[cfg(unix)]
fn attach_terminal_failed_because_stdio_is_not_terminal(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Attach(AttachError::Termios(errno))
            if matches!(errno.raw_os_error(), libc::ENOTTY | libc::ENODEV)
    )
}

#[cfg(windows)]
fn attach_terminal_failed_because_stdio_is_not_terminal(_error: &ClientError) -> bool {
    false
}

pub(super) fn optional_client_flags(flags: Vec<String>) -> Option<Vec<String>> {
    (!flags.is_empty()).then_some(flags)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use super::inherited_rmux_socket_matches_from_env;

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
