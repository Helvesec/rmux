use std::cell::RefCell;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use rmux_client::{connect, ClientError, Connection};
use rmux_proto::{
    CommandOutput, PaneTarget, ResolveTargetType, Response, RmuxError, Target,
    CAPABILITY_CLI_RUNTIME_COMMAND_EXPANSION, INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH,
};

use crate::cli_response::{expect_command_output, expect_command_success, response_name};

use super::ExitFailure;

thread_local! {
    static COMMAND_CONNECTION_CACHE: RefCell<Option<CommandConnectionCache>> =
        const { RefCell::new(None) };
}

struct CommandConnectionCache {
    socket_path: PathBuf,
    connection: Option<Connection>,
}

struct CommandConnectionCacheReset {
    previous: Option<CommandConnectionCache>,
}

impl Drop for CommandConnectionCacheReset {
    fn drop(&mut self) {
        let previous = self.previous.take();
        COMMAND_CONNECTION_CACHE.with(|cache| {
            let _ = cache.replace(previous);
        });
    }
}

pub(super) fn with_command_connection_cache<R>(socket_path: &Path, run: impl FnOnce() -> R) -> R {
    let previous = COMMAND_CONNECTION_CACHE.with(|cache| {
        cache.replace(Some(CommandConnectionCache {
            socket_path: socket_path.to_path_buf(),
            connection: None,
        }))
    });
    let _reset = CommandConnectionCacheReset { previous };
    run()
}

pub(crate) fn run_command<F>(
    socket_path: &Path,
    command_name: &'static str,
    send: F,
) -> Result<i32, ExitFailure>
where
    F: FnOnce(&mut Connection) -> Result<Response, ClientError>,
{
    let response = with_command_connection(socket_path, |connection| {
        send(connection).map_err(ExitFailure::from_client)
    })?;
    finish_command_success(response, command_name)
}

pub(crate) fn cli_target_actions_enabled() -> bool {
    std::env::var_os("RMUX_DISABLE_CLI_TARGET_ACTIONS").is_none()
}

pub(crate) fn target_action_needs_legacy_retry(response: &Result<Response, ClientError>) -> bool {
    matches!(
        response,
        Ok(Response::Error(error)) if matches!(error.error, RmuxError::Decode(_))
    )
}

pub(crate) fn capture_target_action_needs_legacy_retry(
    response: &Result<Response, ClientError>,
) -> bool {
    target_action_needs_legacy_retry(response)
        || matches!(response, Err(ClientError::UnexpectedEof))
}

pub(crate) fn run_payload_command<F>(
    socket_path: &Path,
    command_name: &'static str,
    send: F,
) -> Result<i32, ExitFailure>
where
    F: FnOnce(&mut Connection) -> Result<Response, ClientError>,
{
    let response = with_command_connection(socket_path, |connection| {
        send(connection).map_err(ExitFailure::from_client)
    })?;
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
    let response = with_command_connection(socket_path, send)?;
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
    let response = with_command_connection(socket_path, send)?;
    let output = expect_command_output(&response, command_name)?;
    write_command_output(output)?;
    Ok(0)
}

pub(super) fn run_queued_server_command(
    socket_path: &Path,
    command_name: &'static str,
    queue_command: String,
) -> Result<i32, ExitFailure> {
    let response = with_command_connection(socket_path, |connection| {
        queued_server_command_response(connection, queue_command)
    })?;
    finish_queued_server_command(command_name, response)
}

pub(super) fn run_queued_server_command_with_connection(
    connection: &mut Connection,
    _socket_path: &Path,
    command_name: &'static str,
    queue_command: String,
) -> Result<i32, ExitFailure> {
    let response = queued_server_command_response(connection, queue_command)?;
    finish_queued_server_command(command_name, response)
}

fn queued_server_command_response(
    connection: &mut Connection,
    queue_command: String,
) -> Result<Response, ExitFailure> {
    let source_path = if connection
        .supports_capability(CAPABILITY_CLI_RUNTIME_COMMAND_EXPANSION)
        .map_err(ExitFailure::from_client)?
    {
        INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH
    } else {
        "-"
    };
    connection
        .source_file(
            vec![source_path.to_owned()],
            false,
            false,
            false,
            false,
            None,
            Some(queue_command),
        )
        .map_err(ExitFailure::from_client)
}

fn finish_queued_server_command(
    command_name: &'static str,
    response: Response,
) -> Result<i32, ExitFailure> {
    if let Response::SourceFile(source) = &response {
        if source.exit_status().unwrap_or(0) != 0 && !source.stderr().is_empty() {
            if let Some(output) = source.command_output() {
                write_command_output(output)?;
            }
            let message = String::from_utf8_lossy(source.stderr())
                .trim_end_matches('\n')
                .to_owned();
            return Err(ExitFailure::new(source.exit_status().unwrap_or(1), message));
        }
    }
    let source_stdout_may_be_diagnostic = match &response {
        Response::SourceFile(source) if command_name == "if-shell" => {
            source.exit_status().is_some_and(|status| status != 0)
        }
        _ => true,
    };
    if let Some(output) = response
        .command_output()
        .filter(|output| source_stdout_may_be_diagnostic && !output.stdout().is_empty())
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
    if let Response::SourceFile(source) = &response {
        if let Some(exit_status) = source.exit_status().filter(|status| *status != 0) {
            if let Some(output) = source.command_output() {
                write_command_output(output)?;
            }
            return Ok(exit_status);
        }
    }
    finish_command_success(response, command_name)
        .map_err(|error| normalize_queued_direct_error(command_name, error))
}

fn with_command_connection<F, R>(socket_path: &Path, run: F) -> Result<R, ExitFailure>
where
    F: FnOnce(&mut Connection) -> Result<R, ExitFailure>,
{
    if command_connection_cache_matches(socket_path) {
        return COMMAND_CONNECTION_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            let cache = cache
                .as_mut()
                .expect("cache must exist after positive match");
            if cache.connection.is_none() {
                cache.connection = Some(
                    connect(socket_path)
                        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?,
                );
            }
            let connection = cache
                .connection
                .as_mut()
                .expect("connection must exist after initialization");
            let result = run(connection);
            if result.is_err() {
                cache.connection = None;
            }
            result
        });
    }

    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    run(&mut connection)
}

fn command_connection_cache_matches(socket_path: &Path) -> bool {
    COMMAND_CONNECTION_CACHE.with(|cache| {
        cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| cache.socket_path == socket_path)
    })
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
    rmux_os::path::socket_paths_match(&inherited_socket, socket_path)
}

fn rmux_socket_path_from_env(value: &str) -> Option<PathBuf> {
    let path = value.split_once(',').map_or(value, |(path, _)| path);
    (!path.is_empty()).then(|| PathBuf::from(path))
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

#[cfg(test)]
mod tests {
    use rmux_client::ClientError;
    use rmux_proto::{ErrorResponse, Response, RmuxError};

    use super::{capture_target_action_needs_legacy_retry, target_action_needs_legacy_retry};

    #[test]
    fn target_action_retry_is_limited_to_protocol_decode_failures() {
        assert!(target_action_needs_legacy_retry(&Ok(Response::Error(
            ErrorResponse {
                error: RmuxError::Decode("unknown variant index".to_owned()),
            },
        ))));
        assert!(!target_action_needs_legacy_retry(&Err(
            ClientError::UnexpectedEof,
        )));
        assert!(capture_target_action_needs_legacy_retry(&Err(
            ClientError::UnexpectedEof,
        )));
        assert!(!target_action_needs_legacy_retry(&Ok(Response::Error(
            ErrorResponse {
                error: RmuxError::InvalidTarget {
                    value: "alpha:0.99".to_owned(),
                    reason: "can't find pane: 99".to_owned(),
                },
            },
        ))));
    }
}
