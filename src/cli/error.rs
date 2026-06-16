use std::ffi::CStr;
use std::io::ErrorKind;
use std::path::Path;

use rmux_client::{default_socket_path, AutoStartError, ClientError, NestedContextError};

#[derive(Debug)]
pub(crate) struct ExitFailure {
    exit_code: i32,
    message: String,
    use_stderr: bool,
}

impl ExitFailure {
    pub(crate) fn exit_code(&self) -> i32 {
        self.exit_code
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn use_stderr(&self) -> bool {
        self.use_stderr
    }

    pub(crate) fn new(exit_code: i32, message: impl Into<String>) -> Self {
        Self {
            exit_code,
            message: message.into(),
            use_stderr: true,
        }
    }

    pub(super) fn new_stdout(exit_code: i32, message: impl Into<String>) -> Self {
        Self {
            exit_code,
            message: message.into(),
            use_stderr: false,
        }
    }

    pub(super) fn from_clap(error: clap::Error) -> Self {
        let exit_code = match error.kind() {
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => 0,
            _ => 1,
        };
        let message = tmux_compat_clap_message(&error);

        Self {
            exit_code,
            message,
            use_stderr: error.use_stderr(),
        }
    }

    pub(super) fn from_client(error: ClientError) -> Self {
        Self::new(1, error.to_string())
    }

    pub(super) fn from_client_connect(socket_path: &Path, error: ClientError) -> Self {
        if server_is_absent(&error) {
            if default_socket_path()
                .ok()
                .as_deref()
                .is_some_and(|default_path| default_path == socket_path)
            {
                return Self::no_server_running(socket_path);
            }
            if let ClientError::Io(io_error) = &error {
                return Self::new(
                    1,
                    format!(
                        "error connecting to {} ({})",
                        socket_path.display(),
                        io_error_message_without_code(io_error)
                    ),
                );
            }
        }

        Self::from_client(error)
    }

    pub(super) fn no_server_running(socket_path: &Path) -> Self {
        Self::new(1, format!("no server running on {}", socket_path.display()))
    }

    pub(super) fn from_auto_start(error: AutoStartError) -> Self {
        Self::new(1, error.to_string())
    }
}

fn tmux_compat_clap_message(error: &clap::Error) -> String {
    let message = error.to_string().trim_end().to_owned();
    let first_line = message.lines().next().unwrap_or(message.as_str());
    if message == "error: size missing" || message == "error: command join-pane: size missing" {
        return "size missing".to_owned();
    }
    if first_line.contains("invalid session name: session names must be non-empty") {
        return "invalid session: ".to_owned();
    }
    if let Some((_, detail)) = first_line.rsplit_once(": ") {
        if let Some(normalized) = normalized_invalid_value_detail(detail) {
            return normalized;
        }
    }
    if let Some(stripped) = message.strip_prefix("error: ") {
        if matches!(
            stripped,
            "width too small"
                | "width invalid"
                | "width too large"
                | "height too small"
                | "height invalid"
                | "height too large"
                | "adjustment invalid"
                | "adjustment too small"
                | "adjustment too large"
        ) {
            return stripped.to_owned();
        }
    }
    if let Some(stripped) = message.strip_prefix("error: command ") {
        return format!("command {stripped}");
    }
    if let Some((_, option)) = message.rsplit_once(": invalid option: ") {
        let option = option.lines().next().unwrap_or(option);
        return format!("invalid option: {option}");
    }
    message
}

fn normalized_invalid_value_detail(detail: &str) -> Option<String> {
    if matches!(
        detail,
        "width too small"
            | "width invalid"
            | "width too large"
            | "height too small"
            | "height invalid"
            | "height too large"
            | "adjustment invalid"
            | "adjustment too small"
            | "adjustment too large"
    ) {
        return Some(detail.to_owned());
    }

    None
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

impl From<NestedContextError> for ExitFailure {
    fn from(error: NestedContextError) -> Self {
        Self::new(1, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::tmux_compat_clap_message;

    #[test]
    fn clap_invalid_option_value_errors_keep_single_tmx_line() {
        let error = clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            "error: invalid value 'no-such-hook' for '<HOOK>': invalid option: no-such-hook\n\nFor more information, try '--help'.",
        );

        assert_eq!(
            tmux_compat_clap_message(&error),
            "invalid option: no-such-hook"
        );
    }

    #[test]
    fn resize_pane_dimension_errors_keep_single_tmux_line() {
        let error = clap::Error::raw(clap::error::ErrorKind::ValueValidation, "width too small");

        assert_eq!(tmux_compat_clap_message(&error), "width too small");
    }

    #[test]
    fn resize_pane_adjustment_errors_keep_single_tmux_line() {
        for message in [
            "adjustment invalid",
            "adjustment too small",
            "adjustment too large",
        ] {
            let error = clap::Error::raw(clap::error::ErrorKind::ValueValidation, message);

            assert_eq!(tmux_compat_clap_message(&error), message);
        }
    }

    #[test]
    fn invalid_value_dimension_errors_keep_single_tmux_line() {
        let error = clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            "error: invalid value '70000' for '-x <COLS>': width too large\n\nFor more information, try '--help'.",
        );

        assert_eq!(tmux_compat_clap_message(&error), "width too large");
    }

    #[test]
    fn empty_session_name_errors_keep_tmux_line() {
        let error = clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            "error: invalid value '' for '<NEW_NAME>': invalid session name: session names must be non-empty\n\nFor more information, try '--help'.",
        );

        assert_eq!(tmux_compat_clap_message(&error), "invalid session: ");
    }
}
