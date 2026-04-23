use std::path::Path;

use rmux_client::{connect, ClientError, Connection, StartServerError};

use super::{
    expect_command_success, resolve_session_target_or_current, run_command, run_command_resolved,
    write_command_output, ExitFailure, StartupOptions,
};
use crate::cli_args::{ClientTargetArgs, ServerAccessArgs, SessionTargetArgs};

pub(super) fn run_start_server(
    socket_path: &Path,
    startup: StartupOptions,
) -> Result<i32, ExitFailure> {
    let _connection = Connection::start_server(
        socket_path,
        startup.no_start_server,
        startup.config,
    )
    .map_err(|error| match error {
        StartServerError::Client(error) => ExitFailure::from_client_connect(socket_path, error),
        StartServerError::AutoStart(error) => ExitFailure::from_auto_start(error),
    })?;
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
            Ok(0)
        }
        Err(error) if kill_server_connection_closed(&error) => Ok(0),
        Err(error) => Err(ExitFailure::from_client(error)),
    }
}

pub(super) fn run_server_access(
    args: ServerAccessArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command(socket_path, "server-access", move |connection| {
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
