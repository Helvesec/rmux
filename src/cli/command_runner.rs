use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use rmux_client::{connect, ClientError, Connection};
use rmux_proto::{CommandOutput, PaneTarget, ResolveTargetType, Response, Target};

use crate::cli_response::{expect_command_output, expect_command_success, response_name};

use super::ExitFailure;

pub(crate) fn run_command<F>(
    socket_path: &Path,
    command_name: &'static str,
    send: F,
) -> Result<i32, ExitFailure>
where
    F: FnOnce(&mut Connection) -> Result<Response, ClientError>,
{
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let response = send(&mut connection).map_err(ExitFailure::from_client)?;
    finish_command_success(response, command_name)
}

pub(crate) fn run_payload_command<F>(
    socket_path: &Path,
    command_name: &'static str,
    send: F,
) -> Result<i32, ExitFailure>
where
    F: FnOnce(&mut Connection) -> Result<Response, ClientError>,
{
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let response = send(&mut connection).map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, command_name)?;
    write_command_output(output)?;
    Ok(0)
}

pub(crate) fn run_command_resolved<F>(
    socket_path: &Path,
    command_name: &'static str,
    send: F,
) -> Result<i32, ExitFailure>
where
    F: FnOnce(&mut Connection) -> Result<Response, ExitFailure>,
{
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let response = send(&mut connection)?;
    finish_command_success(response, command_name)
}

pub(crate) fn run_payload_command_resolved<F>(
    socket_path: &Path,
    command_name: &'static str,
    send: F,
) -> Result<i32, ExitFailure>
where
    F: FnOnce(&mut Connection) -> Result<Response, ExitFailure>,
{
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let response = send(&mut connection)?;
    let output = expect_command_output(&response, command_name)?;
    write_command_output(output)?;
    Ok(0)
}

pub(super) fn run_queued_server_command(
    socket_path: &Path,
    command_name: &'static str,
    queue_command: String,
) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let target = inherited_pane_target(&mut connection, socket_path)?;
    let response = connection
        .source_file(
            vec!["-".to_owned()],
            false,
            false,
            false,
            false,
            target,
            Some(queue_command),
        )
        .map_err(ExitFailure::from_client)?;
    if let Some(output) = response
        .command_output()
        .filter(|output| !output.stdout().is_empty())
    {
        let rendered = String::from_utf8_lossy(output.stdout());
        if let Some(message) = strip_source_file_stdin_line_prefix(&rendered) {
            let mut message = message.to_owned();
            while message.ends_with("\n\n") {
                message.pop();
            }
            while message.ends_with('\n') {
                message.pop();
            }
            return Err(ExitFailure::new(1, message));
        }
    }
    finish_command_success(response, command_name)
        .map_err(|error| normalize_queued_direct_error(command_name, error))
}

pub(crate) fn inherited_pane_target(
    connection: &mut Connection,
    socket_path: &Path,
) -> Result<Option<PaneTarget>, ExitFailure> {
    let Some(pane_id) = inherited_pane_id(socket_path) else {
        return Ok(None);
    };
    let response = connection
        .resolve_target(Some(pane_id), ResolveTargetType::Pane, false, false)
        .map_err(ExitFailure::from_client)?;
    match response {
        Response::ResolveTarget(response) => match response.target {
            Target::Pane(target) => Ok(Some(target)),
            _ => Ok(None),
        },
        Response::Error(_) => Ok(None),
        _ => Ok(None),
    }
}

fn inherited_pane_id(socket_path: &Path) -> Option<String> {
    if !rmux_env_socket_matches(socket_path) {
        return None;
    }
    std::env::var("RMUX_PANE")
        .ok()
        .or_else(|| std::env::var("TMUX_PANE").ok())
        .filter(|value| value.starts_with('%'))
}

fn rmux_env_socket_matches(socket_path: &Path) -> bool {
    let Some(inherited_socket) = std::env::var("RMUX")
        .ok()
        .and_then(|value| rmux_socket_path_from_env(&value))
    else {
        return false;
    };
    socket_paths_match(&inherited_socket, socket_path)
}

fn rmux_socket_path_from_env(value: &str) -> Option<PathBuf> {
    let path = value.split_once(',').map_or(value, |(path, _)| path);
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

fn normalize_queued_direct_error(command_name: &str, error: ExitFailure) -> ExitFailure {
    if command_name == "source-file" {
        return error;
    }
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

pub(super) fn unexpected_response(command_name: &str, response: &Response) -> ExitFailure {
    ExitFailure::new(
        1,
        format!(
            "protocol error: unexpected '{}' response for {command_name}",
            response_name(response)
        ),
    )
}

pub(super) fn finish_command_success(
    response: Response,
    command_name: &'static str,
) -> Result<i32, ExitFailure> {
    let output = response.command_output().cloned();
    expect_command_success(response, command_name)?;
    if let Some(output) = output {
        write_command_output(&output)?;
    }
    Ok(0)
}

pub(super) fn write_command_output(output: &CommandOutput) -> Result<(), ExitFailure> {
    match std::io::stdout().write_all(output.stdout()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(ExitFailure::new(
            1,
            format!("failed to write command output: {error}"),
        )),
    }
}

pub(super) fn write_lines_output(lines: &[String]) -> Result<i32, ExitFailure> {
    if lines.is_empty() {
        write_command_output(&CommandOutput::from_stdout(Vec::new()))?;
    } else {
        write_command_output(&CommandOutput::from_stdout(
            format!("{}\n", lines.join("\n")).into_bytes(),
        ))?;
    }
    Ok(0)
}
