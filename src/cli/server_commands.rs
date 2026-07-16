use std::path::Path;
#[cfg(unix)]
use std::time::{Duration, Instant};

use rmux_client::{connect, ClientError, Connection, StartServerError};
use rmux_client::{connect_or_absent, ConnectResult};
use rmux_proto::RmuxError;
#[cfg(windows)]
use rmux_proto::CAPABILITY_DAEMON_STATUS;
use rmux_proto::{ListSessionsRequest, RMUX_WIRE_VERSION};

use super::{
    expect_command_output, resolve_session_target_or_current, run_command, run_command_resolved,
    run_payload_command, ExitFailure, StartupOptions,
};
#[cfg(not(windows))]
use super::{expect_command_success, write_command_output};
use crate::cli_args::{ClientTargetArgs, ServerAccessArgs, SessionTargetArgs};

#[cfg(unix)]
// The daemon may spend up to five seconds draining accepted lifecycle hooks
// before it releases the endpoint. Keep enough margin for connection cleanup
// and scheduler contention so a successful kill-server is a restart barrier.
const KILL_SERVER_SOCKET_CLEANUP_TIMEOUT: Duration = Duration::from_secs(7);
#[cfg(unix)]
const KILL_SERVER_SOCKET_CLEANUP_MIN_POLL: Duration = Duration::from_millis(1);
#[cfg(unix)]
const KILL_SERVER_SOCKET_CLEANUP_MAX_POLL: Duration = Duration::from_millis(10);

pub(super) fn run_start_server(
    socket_path: &Path,
    startup: StartupOptions,
) -> Result<i32, ExitFailure> {
    let mut connection = Connection::start_server(
        socket_path,
        startup.no_start_server,
        startup.config,
    )
    .map_err(|error| match error {
        StartServerError::Client(error) => ExitFailure::from_client_connect(socket_path, error),
        StartServerError::AutoStart(error) => ExitFailure::from_auto_start(error),
    })?;
    let response = connection
        .list_sessions(ListSessionsRequest {
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
        })
        .map_err(ExitFailure::from_client)?;
    let _ = expect_command_output(&response, "list-sessions")?;
    Ok(0)
}

#[cfg(windows)]
pub(super) fn run_kill_server(socket_path: &Path) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    match probe_kill_server_compatible(&mut connection) {
        Ok(()) => {}
        Err(error) if kill_server_connection_closed(&error) => return Ok(0),
        Err(error) => {
            if let Some(wire_version) = legacy_shutdown_fallback_wire_version(&error) {
                return run_legacy_wire_kill_server(socket_path, wire_version);
            }
            return Err(ExitFailure::from_client(error));
        }
    }
    match connection.kill_server_after_write() {
        Ok(()) => Ok(0),
        Err(error) if kill_server_connection_closed(&error) => Ok(0),
        Err(error) => Err(ExitFailure::from_client(error)),
    }
}

#[cfg(not(windows))]
pub(super) fn run_kill_server(socket_path: &Path) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    match connection.kill_server() {
        Ok(response) => {
            let output = response.command_output().cloned();
            expect_command_success(response, "kill-server")?;
            if let Some(output) = output {
                write_command_output(&output)?;
            }
            wait_for_killed_server_socket_cleanup(socket_path);
            Ok(0)
        }
        Err(error) if kill_server_connection_closed(&error) => {
            wait_for_killed_server_socket_cleanup(socket_path);
            Ok(0)
        }
        Err(error) => {
            if let Some(wire_version) = legacy_shutdown_fallback_wire_version(&error) {
                run_legacy_wire_kill_server(socket_path, wire_version)
            } else {
                Err(ExitFailure::from_client(error))
            }
        }
    }
}

#[cfg(windows)]
fn probe_kill_server_compatible(connection: &mut Connection) -> Result<(), ClientError> {
    connection
        .supports_capability(CAPABILITY_DAEMON_STATUS)
        .map(|_| ())
}

fn run_legacy_wire_kill_server(socket_path: &Path, wire_version: u32) -> Result<i32, ExitFailure> {
    let mut connection = match connect_or_absent(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?
    {
        ConnectResult::Connected(connection) => connection,
        ConnectResult::Absent => {
            wait_for_killed_server_socket_cleanup(socket_path);
            return Ok(0);
        }
    };

    match connection.kill_server_legacy_wire(wire_version) {
        Ok(()) => {
            wait_for_killed_server_socket_cleanup(socket_path);
            Ok(0)
        }
        Err(error) if kill_server_connection_closed(&error) => {
            wait_for_killed_server_socket_cleanup(socket_path);
            Ok(0)
        }
        Err(error) => Err(ExitFailure::from_client(error)),
    }
}

#[cfg(unix)]
fn wait_for_killed_server_socket_cleanup(socket_path: &Path) {
    let deadline = Instant::now() + KILL_SERVER_SOCKET_CLEANUP_TIMEOUT;
    let mut next_poll = KILL_SERVER_SOCKET_CLEANUP_MIN_POLL;
    while socket_path.exists() && Instant::now() < deadline {
        std::thread::sleep(next_poll);
        next_poll = (next_poll + next_poll).min(KILL_SERVER_SOCKET_CLEANUP_MAX_POLL);
    }
}

#[cfg(not(unix))]
fn wait_for_killed_server_socket_cleanup(_socket_path: &Path) {}

pub(super) fn run_server_access(
    args: ServerAccessArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_payload_command(socket_path, "server-access", move |connection| {
        connection.server_access(rmux_proto::ServerAccessRequest {
            add: args.add,
            deny: args.deny,
            list: args.list,
            read_only: args.read_only,
            write: args.write,
            target: None,
            user: args.user,
        })
    })
}

pub(super) fn run_lock_server(socket_path: &Path) -> Result<i32, ExitFailure> {
    run_command(socket_path, "lock-server", |connection| {
        connection.lock_server()
    })
}

pub(super) fn run_lock_session(
    args: SessionTargetArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "lock-session", move |connection| {
        let target =
            resolve_session_target_or_current(connection, args.target.as_ref(), "lock-session")?;
        connection
            .lock_session(target)
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_lock_client(
    args: ClientTargetArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command(socket_path, "lock-client", move |connection| {
        connection.lock_client(args.target.unwrap_or_else(|| "=".to_owned()))
    })
}

fn kill_server_connection_closed(error: &ClientError) -> bool {
    matches!(error, ClientError::UnexpectedEof)
        || matches!(
            error,
            ClientError::Io(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::UnexpectedEof
                )
        )
}

fn legacy_shutdown_fallback_wire_version(error: &ClientError) -> Option<u32> {
    match error {
        ClientError::Protocol(RmuxError::UnsupportedWireVersion { got, .. })
            if (1..RMUX_WIRE_VERSION).contains(got) =>
        {
            Some(*got)
        }
        _ => None,
    }
}
