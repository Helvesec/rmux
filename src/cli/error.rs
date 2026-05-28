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
    if let Some(stripped) = message.strip_prefix("error: command ") {
        return format!("command {stripped}");
    }
    message
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
