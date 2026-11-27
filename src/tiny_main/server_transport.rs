use super::*;

#[cfg(all(test, unix))]
mod canonical_inventory_tests {
    crate::session_inventory_tests::canonical_inventory_tests!("tiny");
}

#[cfg(all(test, unix))]
fn canonical_inventory_run_all_panes(path: &std::path::Path) -> i32 {
    let mut connection = rmux_client::connect(path).unwrap();
    run_list_panes_all_sessions(&mut connection).unwrap()
}

pub(super) fn run_start_server(socket_path: &Path) -> Result<i32, String> {
    let mut connection = Connection::start_server(socket_path, false, default_auto_start_config()?)
        .map_err(|error| match error {
            StartServerError::Client(error) => client_error(socket_path, error),
            StartServerError::AutoStart(error) => error.to_string(),
        })?;
    let response = connection
        .list_sessions(ListSessionsRequest {
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
        })
        .map_err(|error| error.to_string())?;
    response
        .command_output()
        .ok_or_else(|| "protocol error: unexpected response for start-server".to_owned())?;
    Ok(0)
}

pub(super) fn run_list_sessions(
    socket_path: &Path,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    let response = connection
        .list_sessions(ListSessionsRequest {
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
        })
        .map_err(|error| error.to_string())?;
    write_response_output_or_error(response, "list-sessions")
}

pub(super) fn run_has_session(
    original_args: &[OsString],
    socket_path: &Path,
    request: TinyHasSession,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    let response = connection
        .has_session(request.target)
        .map_err(|error| error.to_string())?;
    match response {
        Response::HasSession(response) if response.exists => Ok(0),
        Response::HasSession(_) => exec_full_helper(original_args),
        response => write_response_output_or_error(response, "has-session"),
    }
}

pub(super) fn run_list_windows(
    original_args: &[OsString],
    socket_path: &Path,
    request: TinyListWindows,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    let response = connection
        .list_windows(request.target, None)
        .map_err(|error| error.to_string())?;
    if response_needs_session_resolution_retry(&response) {
        return exec_full_helper(original_args);
    }
    write_response_output_or_error(response, "list-windows")
}

pub(super) fn run_list_panes(
    original_args: &[OsString],
    socket_path: &Path,
    request: TinyListPanes,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    match request {
        TinyListPanes::AllSessions => run_list_panes_all_sessions(&mut connection),
        TinyListPanes::Target {
            target,
            target_window_index,
        } => {
            let target_window_index = match target_window_index {
                Some(index) => Some(index),
                None => match resolve_active_window_index(&mut connection, &target) {
                    Ok(index) => Some(index),
                    Err(_) => return exec_full_helper(original_args),
                },
            };
            let response = connection
                .list_panes_in_window(
                    target,
                    target_window_index,
                    Some(DEFAULT_LIST_PANES_WINDOW_FORMAT.to_owned()),
                )
                .map_err(|error| error.to_string())?;
            if response_needs_session_resolution_retry(&response) {
                return exec_full_helper(original_args);
            }
            write_response_output_or_error(response, "list-panes")
        }
    }
}

pub(super) fn run_list_panes_all_sessions(connection: &mut Connection) -> Result<i32, String> {
    let sessions = list_session_names(connection)?;
    let mut stdout = Vec::new();
    for session_name in sessions {
        let response = connection
            .list_panes_in_window(
                session_name,
                None,
                Some(DEFAULT_LIST_PANES_ALL_FORMAT.to_owned()),
            )
            .map_err(|error| error.to_string())?;
        stdout.extend_from_slice(&response_stdout(response, "list-panes")?);
    }
    write_stdout(&stdout)
        .map(|()| 0)
        .map_err(|error| format!("failed to write list-panes command output: {error}"))
}

pub(super) fn list_session_names(
    connection: &mut Connection,
) -> Result<Vec<rmux_proto::SessionName>, String> {
    let response = connection
        .list_sessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })
        .map_err(|error| error.to_string())?;
    let stdout = response_stdout(response, "list-sessions")?;
    String::from_utf8_lossy(&stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            rmux_proto::SessionName::from_canonical(line)
                .map_err(|error| format!("invalid session name from list-sessions: {error}"))
        })
        .collect()
}

pub(super) fn response_stdout(response: Response, command: &str) -> Result<Vec<u8>, String> {
    match response {
        Response::Error(error) => Err(tmux_cli_error_message(command, &error.error)),
        response => response
            .command_output()
            .map(|output| output.stdout().to_vec())
            .ok_or_else(|| format!("{command} returned no command output")),
    }
}

pub(super) fn resolve_active_window_index(
    connection: &mut Connection,
    session_name: &rmux_proto::SessionName,
) -> Result<u32, String> {
    let response = connection
        .list_windows(
            session_name.clone(),
            Some("#{window_index}:#{window_active}".to_owned()),
        )
        .map_err(|error| error.to_string())?;
    let output = response
        .command_output()
        .ok_or_else(|| "protocol error: list-windows did not return command output".to_owned())?;
    let stdout = String::from_utf8_lossy(output.stdout());
    let active_line = stdout
        .lines()
        .find(|line| line.rsplit(':').next() == Some("1"))
        .ok_or_else(|| "list-panes could not resolve the active window".to_owned())?;
    active_line
        .split(':')
        .next()
        .ok_or_else(|| "active window output is malformed".to_owned())?
        .parse::<u32>()
        .map_err(|error| format!("invalid active window index from server: {error}"))
}

#[cfg(windows)]
pub(super) fn run_kill_server(
    socket_path: &Path,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let (mut connection, selected_socket_path) = match command_connection.take_reusable(socket_path)
    {
        Some(connection) => (connection, socket_path.to_path_buf()),
        None => connect_for_server_shutdown(socket_path)
            .map_err(|error| client_error(socket_path, error))?,
    };
    match probe_kill_server_compatible(&mut connection) {
        Ok(()) => {}
        Err(error) if kill_server_connection_closed(&error) => {
            drop(connection);
            wait_for_killed_server_socket_cleanup(&selected_socket_path)?;
            return Ok(0);
        }
        Err(error) => {
            if let Some(wire_version) = legacy_shutdown_fallback_wire_version(&error) {
                return run_legacy_wire_kill_server(&selected_socket_path, wire_version);
            }
            return Err(error.to_string());
        }
    }
    let shutdown = connection.kill_server_after_write();
    drop(connection);
    match shutdown {
        Ok(()) => {
            wait_for_killed_server_socket_cleanup(&selected_socket_path)?;
            Ok(0)
        }
        Err(error) if kill_server_connection_closed(&error) => {
            wait_for_killed_server_socket_cleanup(&selected_socket_path)?;
            Ok(0)
        }
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(windows)]
pub(super) fn probe_kill_server_compatible(connection: &mut Connection) -> Result<(), ClientError> {
    connection
        .supports_capability(CAPABILITY_DAEMON_STATUS)
        .map(|_| ())
}

#[cfg(not(windows))]
pub(super) fn run_kill_server(
    socket_path: &Path,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    match connection.kill_server() {
        Ok(response) => {
            let code = write_response_output_or_error(response, "kill-server")?;
            drop(connection);
            wait_for_killed_server_socket_cleanup(socket_path)?;
            Ok(code)
        }
        Err(error) if kill_server_connection_closed(&error) => {
            drop(connection);
            wait_for_killed_server_socket_cleanup(socket_path)?;
            Ok(0)
        }
        Err(error) => {
            if let Some(wire_version) = legacy_shutdown_fallback_wire_version(&error) {
                run_legacy_wire_kill_server(socket_path, wire_version)
            } else {
                Err(error.to_string())
            }
        }
    }
}
