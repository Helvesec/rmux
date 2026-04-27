#![deny(missing_docs)]

//! Blocking Unix-socket client for the RMUX detached RPC protocol.
//!
//! This crate provides the transport layer for sending [`rmux_proto::Request`]
//! frames and receiving [`rmux_proto::Response`] frames over a blocking
//! `std::os::unix::net::UnixStream`. It also exposes nested-session detection
//! through the `$RMUX` environment variable and raw-terminal lifecycle
//! management for attach-mode clients.

#[cfg(unix)]
pub mod attach;
#[cfg(windows)]
#[path = "attach_windows.rs"]
pub mod attach;
pub mod auto_start;
pub(crate) mod commands;
pub mod connection;
#[cfg(unix)]
pub mod control;
#[cfg(windows)]
#[path = "control_unsupported.rs"]
pub mod control;
pub mod nested;

pub use attach::{
    attach_terminal, attach_terminal_with_initial_bytes, attach_with_terminal, drive_attach_stream,
    AttachError, RawTerminal,
};
pub use auto_start::{
    ensure_server_running, ensure_server_running_with_config, AutoStartConfig,
    AutoStartConfigSelection, AutoStartError, INTERNAL_DAEMON_FLAG,
};
pub use commands::server::StartServerError;
pub use connection::{
    connect, connect_or_absent, default_socket_path, resolve_socket_path, socket_path_for_label,
    AttachSessionUpgrade, AttachTransition, ConnectResult, Connection, ControlModeUpgrade,
    ControlTransition,
};
pub use control::{drive_control_mode, drive_control_mode_with_stdio};
pub use nested::{
    detect_context, ensure_nested_context, require_nested_context, ClientContext,
    NestedContextError,
};

use rmux_proto::RmuxError;
use std::fmt;

/// Client-side errors for transport and protocol failures.
#[derive(Debug)]
pub enum ClientError {
    /// An I/O error occurred on the Unix socket.
    Io(std::io::Error),
    /// A protocol framing or encoding error occurred.
    Protocol(RmuxError),
    /// Entering or restoring raw terminal mode failed.
    Attach(AttachError),
    /// The server closed the connection before sending a complete response frame.
    UnexpectedEof,
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "i/o error: {error}"),
            Self::Protocol(error) => write!(formatter, "protocol error: {error}"),
            Self::Attach(error) => write!(formatter, "attach error: {error}"),
            Self::UnexpectedEof => formatter
                .write_str("server closed connection before a complete response frame arrived"),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::Attach(error) => Some(error),
            Self::UnexpectedEof => None,
        }
    }
}

impl From<std::io::Error> for ClientError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<RmuxError> for ClientError {
    fn from(error: RmuxError) -> Self {
        Self::Protocol(error)
    }
}

impl From<AttachError> for ClientError {
    fn from(error: AttachError) -> Self {
        Self::Attach(error)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::io;

    use super::{AttachError, ClientError};

    #[test]
    fn client_error_wraps_attach_errors() {
        let error = ClientError::from(AttachError::Io(io::Error::other("dup failed")));

        assert!(
            matches!(error, ClientError::Attach(AttachError::Io(_))),
            "attach errors should preserve their variant information"
        );
        assert_eq!(
            error.to_string(),
            expected_attach_error_display("dup failed")
        );
        assert!(
            error.source().is_some(),
            "wrapped attach error should chain"
        );
    }

    #[cfg(unix)]
    fn expected_attach_error_display(message: &str) -> String {
        format!("attach error: terminal descriptor operation failed: {message}")
    }

    #[cfg(windows)]
    fn expected_attach_error_display(message: &str) -> String {
        format!("attach error: terminal console operation failed: {message}")
    }

    #[cfg(not(any(unix, windows)))]
    fn expected_attach_error_display(message: &str) -> String {
        format!("attach error: terminal descriptor operation failed: {message}")
    }
}
