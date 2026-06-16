use std::path::Path;
#[cfg(unix)]
use std::time::{Duration, Instant};

use rmux_client::{connect, ClientError, Connection, StartServerError};
use rmux_proto::ListSessionsRequest;

use super::{
    expect_command_output, expect_command_success, resolve_session_target_or_current, run_command,
    run_command_resolved, run_payload_command, write_command_output, ExitFailure, StartupOptions,
};
use crate::cli_args::{ClientTargetArgs, ServerAccessArgs, SessionTargetArgs};

#[cfg(unix)]
const KILL_SERVER_SOCKET_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
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
    matches!(
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
