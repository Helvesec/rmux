//! Auto-start error type, startup-error mapping and user-facing recovery hints.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use rmux_bootstrap::startup_unix::StartupError;
#[cfg(windows)]
use rmux_bootstrap::startup_windows::StartupError;

use crate::shell_quote::shell_quote_path;
use crate::{default_socket_path, ClientError};

/// Errors raised while auto-starting or connecting to the RMUX server.
#[derive(Debug)]
pub enum AutoStartError {
    /// The client transport failed before or during readiness polling.
    Client(ClientError),
    /// Resolving the `rmux` binary path failed.
    BinaryPath(io::Error),
    /// Re-executing the hidden daemon process failed.
    Launch {
        /// The binary path that failed to spawn.
        path: PathBuf,
        /// The underlying process-spawn error.
        error: io::Error,
    },
    /// A running daemon speaks an incompatible protocol version.
    IncompatibleDaemon {
        /// The socket path hosting the incompatible daemon.
        socket_path: PathBuf,
        /// Human-readable protocol mismatch detail.
        message: String,
    },
    /// The socket never became reachable before the readiness deadline.
    TimedOut {
        /// The socket path that never became reachable.
        socket_path: PathBuf,
        /// The amount of time spent polling.
        waited: Duration,
    },
}

impl fmt::Display for AutoStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client(error) => write!(formatter, "{error}"),
            Self::BinaryPath(error) => {
                write!(formatter, "failed to resolve rmux binary path: {error}")
            }
            Self::Launch { path, error } => {
                write!(
                    formatter,
                    "failed to launch hidden rmux daemon '{}': {error}",
                    path.display()
                )
            }
            Self::IncompatibleDaemon {
                socket_path,
                message,
            } => write!(
                formatter,
                "rmux: {message} on '{}'.\nrmux: stopping this daemon ends its sessions and running processes. Save your work with the matching client first.\nrmux: run `{}` to stop it, then retry.",
                socket_path.display(),
                incompatible_daemon_kill_server_command(socket_path)
            ),
            Self::TimedOut {
                socket_path,
                waited,
            } => write!(
                formatter,
                "timed out after {}s waiting for rmux server socket '{}'. \
                 The hidden daemon may have exited before creating the socket; run `{}` to surface startup errors.",
                waited.as_secs(),
                socket_path.display(),
                diagnostic_start_server_command(socket_path)
            ),
        }
    }
}

impl std::error::Error for AutoStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Client(error) => Some(error),
            Self::BinaryPath(error) => Some(error),
            Self::Launch { error, .. } => Some(error),
            Self::IncompatibleDaemon { .. } => None,
            Self::TimedOut { .. } => None,
        }
    }
}

impl From<ClientError> for AutoStartError {
    fn from(error: ClientError) -> Self {
        Self::Client(error)
    }
}

#[cfg(unix)]
pub(super) fn auto_start_error_from_startup(
    error: StartupError,
    binary_path: &Path,
    socket_path: &Path,
) -> AutoStartError {
    match error {
        StartupError::Launcher { source } => AutoStartError::Launch {
            path: binary_path.to_path_buf(),
            error: source,
        },
        StartupError::StartupTimeout { waited, .. } => AutoStartError::TimedOut {
            socket_path: socket_path.to_path_buf(),
            waited,
        },
        error => AutoStartError::Client(ClientError::Io(io::Error::new(
            startup_error_kind(&error),
            error.to_string(),
        ))),
    }
}

#[cfg(windows)]
pub(super) fn auto_start_error_from_startup(
    error: StartupError,
    binary_path: &Path,
    socket_path: &Path,
) -> AutoStartError {
    match error {
        StartupError::Launcher { source } => AutoStartError::Launch {
            path: binary_path.to_path_buf(),
            error: source,
        },
        StartupError::StartupTimeout { waited, .. } => AutoStartError::TimedOut {
            socket_path: socket_path.to_path_buf(),
            waited,
        },
        error => AutoStartError::Client(ClientError::Io(io::Error::new(
            startup_error_kind(&error),
            error.to_string(),
        ))),
    }
}

#[cfg(unix)]
fn startup_error_kind(error: &StartupError) -> io::ErrorKind {
    match error {
        StartupError::InvalidPath { .. } | StartupError::SymlinkRejected { .. } => {
            io::ErrorKind::InvalidInput
        }
        StartupError::UnsafeOwner { .. }
        | StartupError::UnsafePermissions { .. }
        | StartupError::PeerCredentialMismatch { .. } => io::ErrorKind::PermissionDenied,
        StartupError::Lock { source, .. } | StartupError::Filesystem { source, .. } => {
            source.kind()
        }
        StartupError::Launcher { source } => source.kind(),
        StartupError::StartupTimeout { .. } => io::ErrorKind::TimedOut,
    }
}

#[cfg(windows)]
fn startup_error_kind(error: &StartupError) -> io::ErrorKind {
    match error {
        StartupError::InvalidPipeName { .. } | StartupError::InvalidMutexName { .. } => {
            io::ErrorKind::InvalidInput
        }
        StartupError::MutexAccessDenied { .. } | StartupError::PipeAccessDenied { .. } => {
            io::ErrorKind::PermissionDenied
        }
        StartupError::MutexTimeout { .. }
        | StartupError::PipeBusy { .. }
        | StartupError::StartupTimeout { .. } => io::ErrorKind::TimedOut,
        StartupError::PipeNotFound { .. } | StartupError::PipeNoData { .. } => {
            io::ErrorKind::NotFound
        }
        StartupError::Mutex { source, .. } | StartupError::PipeIo { source, .. } => source.kind(),
        StartupError::Launcher { source } => source.kind(),
    }
}

pub(super) fn incompatible_daemon_kill_server_command(socket_path: &Path) -> String {
    if default_socket_path()
        .ok()
        .as_deref()
        .is_some_and(|default_path| default_path == socket_path)
    {
        return "rmux kill-server".to_owned();
    }

    format!("rmux -S {} kill-server", shell_quote_path(socket_path))
}

fn diagnostic_start_server_command(socket_path: &Path) -> String {
    if default_socket_path()
        .ok()
        .as_deref()
        .is_some_and(|default_path| default_path == socket_path)
    {
        return "rmux start-server".to_owned();
    }

    format!("rmux -S {} start-server", shell_quote_path(socket_path))
}
