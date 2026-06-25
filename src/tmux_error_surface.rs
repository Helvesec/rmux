use std::ffi::CStr;
use std::io::ErrorKind;
use std::path::Path;

use rmux_client::{default_socket_path, ClientError};
use rmux_proto::RmuxError;

pub(crate) fn tmux_client_connect_error_message(
    socket_path: &Path,
    error: &ClientError,
) -> Option<String> {
    if !server_is_absent(error) {
        return None;
    }

    if default_socket_path()
        .ok()
        .as_deref()
        .is_some_and(|default_path| default_path == socket_path)
    {
        return Some(format!("no server running on {}", socket_path.display()));
    }

    if let ClientError::Io(io_error) = error {
        return Some(format!(
            "error connecting to {} ({})",
            socket_path.display(),
            io_error_message_without_code(io_error)
        ));
    }

    None
}

pub(crate) fn tmux_cli_error_message(command_name: &str, error: &RmuxError) -> String {
    match error {
        RmuxError::InvalidTarget { value, reason }
            if matches!(command_name, "link-window" | "move-window")
                && reason == "window index already exists in session" =>
        {
            window_index_from_target(value)
                .map(|index| format!("index in use: {index}"))
                .unwrap_or_else(|| error.to_string())
        }
        RmuxError::InvalidTarget { reason, .. } if reason.starts_with("can't find ") => {
            reason.clone()
        }
        RmuxError::SessionNotFound(session_name) if command_name == "kill-session" => {
            format!("can't find session: {session_name}")
        }
        RmuxError::InvalidSetOption(message)
            if message.starts_with("unknown value: ") || message.starts_with("bad value: ") =>
        {
            message.clone()
        }
        RmuxError::InvalidSetOption(message)
            if message.starts_with("value is ") || message.starts_with("invalid style: ") =>
        {
            message.clone()
        }
        RmuxError::InvalidSetOption(message)
            if message
                .strip_prefix("invalid set-option request: ")
                .is_some_and(|message| {
                    message.starts_with("value is ") || message.starts_with("invalid style: ")
                }) =>
        {
            message
                .strip_prefix("invalid set-option request: ")
                .unwrap_or(message)
                .to_owned()
        }
        RmuxError::InvalidSetOption(message) if message.ends_with(" is already set") => message
            .strip_suffix(" is already set")
            .map(|name| format!("already set: {name}"))
            .unwrap_or_else(|| message.clone()),
        RmuxError::Server(message)
            if command_name == "detach-client"
                && message == "detach-client requires an attached client" =>
        {
            "no current client".to_owned()
        }
        RmuxError::Server(message) | RmuxError::Message(message)
            if message
                .strip_prefix("invalid set-option request: ")
                .is_some_and(|message| {
                    message.starts_with("value is ") || message.starts_with("invalid style: ")
                }) =>
        {
            message
                .strip_prefix("invalid set-option request: ")
                .unwrap_or(message)
                .to_owned()
        }
        RmuxError::Server(message) if command_name == "delete-buffer" => {
            if let Some(name) = message.strip_prefix("no buffer ") {
                format!("unknown buffer: {name}")
            } else {
                message.clone()
            }
        }
        RmuxError::Server(message) => message.clone(),
        _ => error.to_string(),
    }
}

pub(crate) fn source_file_error_uses_stdout(error: &RmuxError) -> bool {
    match error {
        RmuxError::Server(message) => has_source_file_line_prefix(message),
        _ => false,
    }
}

fn window_index_from_target(target: &str) -> Option<&str> {
    target.rsplit_once(':').map(|(_, index)| index)
}

fn has_source_file_line_prefix(message: &str) -> bool {
    let mut parts = message.split(':');
    let Some(mut previous) = parts.next() else {
        return false;
    };
    for current in parts {
        if !previous.is_empty() && previous.bytes().all(|byte| byte.is_ascii_digit()) {
            return true;
        }
        previous = current;
    }
    false
}

fn server_is_absent(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Io(io_error)
            if matches!(
                io_error.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            )
    )
}

fn io_error_message_without_code(error: &std::io::Error) -> String {
    if let Some(errno) = error.raw_os_error() {
        // tmux reports the strerror text inside "error connecting to ... (...)"
        // without Rust's additional "(os error N)" suffix.
        let message = unsafe {
            // SAFETY: `strerror` returns either null or a pointer to a
            // NUL-terminated process-owned message for the supplied errno.
            let ptr = libc::strerror(errno);
            (!ptr.is_null()).then(|| CStr::from_ptr(ptr).to_string_lossy().into_owned())
        };
        if let Some(message) = message {
            return message;
        }
    }

    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_file_line_prefix_accepts_windows_drive_paths() {
        assert!(has_source_file_line_prefix(
            r"C:\Users\RMUXUser\.tmux.conf:12: unknown command"
        ));
    }

    #[test]
    fn source_file_line_prefix_rejects_plain_messages() {
        assert!(!has_source_file_line_prefix(
            "source-file failed: missing file"
        ));
        assert!(!has_source_file_line_prefix(
            "C:\\Users\\RMUXUser\\.tmux.conf"
        ));
    }
}
