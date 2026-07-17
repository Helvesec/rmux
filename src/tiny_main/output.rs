use std::io::{self, ErrorKind, Write};
use std::path::Path;

use rmux_client::ClientError;
use rmux_proto::Response;

use crate::tmux_error_surface::{tmux_cli_error_message, tmux_client_connect_error_message};

pub(super) fn write_response_output_or_error(
    response: Response,
    command: &str,
) -> Result<i32, String> {
    match response {
        Response::Error(error) => Err(tmux_cli_error_message(command, &error.error)),
        response => {
            if let Some(output) = response.command_output() {
                write_stdout(output.stdout()).map_err(|error| {
                    format!("failed to write {command} command output: {error}")
                })?;
            }
            Ok(0)
        }
    }
}

pub(super) fn write_stdout(bytes: &[u8]) -> io::Result<()> {
    match io::stdout().lock().write_all(bytes) {
        Ok(()) => Ok(()),
        Err(error) if stdout_write_error_is_tty_compatible_exit(error.kind()) => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) fn write_stderr(bytes: &[u8]) -> io::Result<()> {
    io::stderr().lock().write_all(bytes)
}

fn stdout_write_error_is_tty_compatible_exit(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::BrokenPipe)
}

pub(super) fn client_error(socket_path: &Path, error: ClientError) -> String {
    tmux_client_connect_error_message(socket_path, &error).unwrap_or_else(|| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::stdout_write_error_is_tty_compatible_exit;
    use std::io::ErrorKind;

    #[test]
    fn stdout_write_only_tolerates_broken_pipe() {
        assert!(stdout_write_error_is_tty_compatible_exit(
            ErrorKind::BrokenPipe
        ));
        assert!(!stdout_write_error_is_tty_compatible_exit(
            ErrorKind::StorageFull
        ));
    }
}
