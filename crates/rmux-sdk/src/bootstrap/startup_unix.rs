//! Unix daemon startup race serialization for the SDK bootstrap layer.
//!
//! This module owns the Unix-only contract for `connect_or_start`: a single
//! caller per endpoint becomes the startup owner under a per-endpoint flock,
//! prepares the on-disk artifacts (owner-only `rmux-$uid` directory, stale
//! socket cleanup, symlink rejection), invokes the supplied launcher, and
//! waits for the daemon to come up. Concurrent callers either lose the race
//! and connect to the daemon the winner created, or surface a documented
//! recoverable error.
//!
//! The module deliberately stays in the SDK bootstrap/IPC boundary. Server
//! command dispatch is unaffected, and the detached IPC contract used by
//! existing length-prefixed bincode clients and `attach-session` upgrades
//! remains untouched.
//!
//! All filesystem operations validate that the lock file, socket directory,
//! and socket path itself are not symlinks before trusting them.

#![cfg(unix)]

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::task;
use tokio::time::sleep;

use crate::bootstrap::deadline::StartupDeadline;
use rmux_os::identity::real_user_id;

#[path = "startup_unix/filesystem.rs"]
mod filesystem;
#[path = "startup_unix/lock.rs"]
mod lock;

use filesystem::{prepare_socket_parent, prepare_socket_path_safe, reject_socket_symlink};
use lock::StartupLock;

/// Permission bits enforced for the per-endpoint startup lock file.
pub const STARTUP_LOCK_MODE: u32 = 0o600;
/// Permission bits enforced for the owning `rmux-$uid` socket directory.
pub const SOCKET_DIRECTORY_MODE: u32 = 0o700;
/// World/group bit mask; any of these set on a socket-related path is unsafe.
pub const UNSAFE_PERMISSION_MASK: u32 = 0o077;
/// Default deadline a startup owner waits for the launched daemon to bind.
///
/// Hidden daemon startup can contend with other release-gate suites on macOS
/// runners. A longer deadline keeps the bootstrap fail-closed while avoiding
/// false negatives when the child process is merely slow to bind the socket.
pub const DEFAULT_STARTUP_DEADLINE: Duration = Duration::from_secs(20);
/// Default poll interval used while waiting for the daemon to become ready.
pub const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(25);
const CONNECT_PROBE_TIMEOUT: Duration = Duration::from_millis(50);

/// Outcome of [`connect_or_start`].
#[derive(Debug)]
pub enum StartupOutcome {
    /// The caller acquired the startup lock, ran the launcher, and connected
    /// to the daemon it just started.
    Started(UnixStream),
    /// The caller connected to a daemon that was already serving the endpoint
    /// (either before any lock attempt or after losing the startup race).
    JoinedExisting(UnixStream),
}

impl StartupOutcome {
    /// Borrow the connected stream regardless of who started the daemon.
    #[must_use]
    pub fn stream(&self) -> &UnixStream {
        match self {
            Self::Started(stream) | Self::JoinedExisting(stream) => stream,
        }
    }

    /// Consume the outcome and return only the connected stream.
    #[must_use]
    pub fn into_stream(self) -> UnixStream {
        match self {
            Self::Started(stream) | Self::JoinedExisting(stream) => stream,
        }
    }

    /// Returns whether this caller was the startup owner that actually ran
    /// the launcher closure.
    #[must_use]
    pub const fn is_owner(&self) -> bool {
        matches!(self, Self::Started(_))
    }
}

/// Typed errors produced by [`connect_or_start`].
#[derive(Debug)]
pub enum StartupError {
    /// The supplied socket path could not be used at all (no parent, empty,
    /// or otherwise structurally invalid).
    InvalidPath {
        /// Visible reason describing why the path was rejected.
        reason: String,
        /// Path that was rejected.
        path: PathBuf,
    },
    /// A path on the startup critical path (lock file, socket directory, or
    /// socket itself) was a symlink and so was rejected before any unlink or
    /// bind.
    SymlinkRejected {
        /// Symlink path that was refused.
        path: PathBuf,
    },
    /// A filesystem-level operation failed.
    Filesystem {
        /// Short stable identifier for the failing step (e.g. `"create lock"`).
        operation: &'static str,
        /// Path the operation targeted.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// Acquiring or holding the per-endpoint flock failed.
    Lock {
        /// Lock file path that produced the error.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// A directory or socket-related path was owned by a different user.
    UnsafeOwner {
        /// Path with unsafe ownership.
        path: PathBuf,
        /// Real user id of the running process.
        expected_uid: u32,
        /// Owner uid actually observed on disk.
        actual_uid: u32,
    },
    /// A directory or file granted access bits to anyone other than the owner.
    UnsafePermissions {
        /// Path with unsafe permissions.
        path: PathBuf,
        /// Mode bits observed on disk.
        mode: u32,
    },
    /// The launcher closure failed to start the daemon.
    Launcher {
        /// Underlying I/O error reported by the launcher closure.
        source: io::Error,
    },
    /// The startup deadline elapsed before the daemon answered.
    StartupTimeout {
        /// Endpoint that never came up in time.
        socket_path: PathBuf,
        /// Total time the caller waited.
        waited: Duration,
    },
    /// A connected daemon answered but its peer credentials did not match the
    /// running user's real uid.
    PeerCredentialMismatch {
        /// Real user id of the running process.
        expected_uid: u32,
        /// uid reported by the daemon's peer credentials.
        actual_uid: u32,
        /// Endpoint that produced the mismatched credentials.
        socket_path: PathBuf,
    },
}

impl StartupError {
    /// Returns `true` when the error is one of the documented recoverable
    /// loser outcomes. A caller that hits a recoverable error may retry the
    /// same endpoint, fall through to a slower bootstrap path, or surface the
    /// error to its own user as a transient bootstrap failure.
    ///
    /// `Filesystem`, `InvalidPath`, `SymlinkRejected`, `UnsafeOwner`, and
    /// `UnsafePermissions` are intentionally not recoverable: they reflect a
    /// hostile or misconfigured filesystem rather than a transient race
    /// between two callers.
    #[must_use]
    pub const fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Self::Lock { .. }
                | Self::Launcher { .. }
                | Self::StartupTimeout { .. }
                | Self::PeerCredentialMismatch { .. }
        )
    }
}

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { reason, path } => write!(
                formatter,
                "rmux startup rejected '{}': {reason}",
                path.display()
            ),
            Self::SymlinkRejected { path } => write!(
                formatter,
                "rmux startup refused to follow symlink at '{}'",
                path.display()
            ),
            Self::Filesystem {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "rmux startup failed to {operation} '{}': {source}",
                path.display()
            ),
            Self::Lock { path, source } => write!(
                formatter,
                "rmux startup lock '{}' failed: {source}",
                path.display()
            ),
            Self::UnsafeOwner {
                path,
                expected_uid,
                actual_uid,
            } => write!(
                formatter,
                "rmux startup refused '{}': owned by uid {actual_uid} but expected uid {expected_uid}",
                path.display()
            ),
            Self::UnsafePermissions { path, mode } => write!(
                formatter,
                "rmux startup refused '{}': permissions 0o{mode:04o} grant access beyond the owner",
                path.display()
            ),
            Self::Launcher { source } => {
                write!(formatter, "rmux startup launcher failed: {source}")
            }
            Self::StartupTimeout {
                socket_path,
                waited,
            } => write!(
                formatter,
                "rmux startup timed out after {}ms waiting for '{}' to answer",
                waited.as_millis(),
                socket_path.display()
            ),
            Self::PeerCredentialMismatch {
                expected_uid,
                actual_uid,
                socket_path,
            } => write!(
                formatter,
                "rmux daemon at '{}' reported peer uid {actual_uid} but expected {expected_uid}",
                socket_path.display()
            ),
        }
    }
}

impl Error for StartupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Filesystem { source, .. }
            | Self::Lock { source, .. }
            | Self::Launcher { source } => Some(source),
            _ => None,
        }
    }
}

/// Connects to the daemon serving `socket_path`, starting it under a
/// per-endpoint startup lock if no live daemon is reachable.
///
/// Concurrency contract:
///
/// - Only the caller that wins the per-endpoint flock invokes `launcher`.
/// - All other callers either join the daemon the winner started, or surface
///   a documented [`StartupError::is_recoverable`] error.
/// - Filesystem races are guarded: the `rmux-$uid` directory is owner-only,
///   the lock file is opened with `O_NOFOLLOW` at mode `0o600`, the socket
///   path is `lstat`-checked before any unlink, and a stale socket is only
///   removed after a connect probe proves no daemon is answering.
/// - The connected stream's peer credentials must match the running user's
///   real uid. A mismatch closes the stream and returns the typed
///   [`StartupError::PeerCredentialMismatch`].
pub async fn connect_or_start<L, F>(
    socket_path: &Path,
    launcher: L,
) -> Result<StartupOutcome, StartupError>
where
    L: FnOnce() -> F,
    F: Future<Output = io::Result<()>>,
{
    connect_or_start_with(
        socket_path,
        launcher,
        DEFAULT_STARTUP_DEADLINE,
        STARTUP_POLL_INTERVAL,
    )
    .await
}

/// Variant of [`connect_or_start`] with an explicit deadline and poll
/// interval. Reserved for tests and for callers that need a tighter budget
/// than the default.
pub async fn connect_or_start_with<L, F>(
    socket_path: &Path,
    launcher: L,
    deadline: Duration,
    poll_interval: Duration,
) -> Result<StartupOutcome, StartupError>
where
    L: FnOnce() -> F,
    F: Future<Output = io::Result<()>>,
{
    connect_or_start_with_timeout(socket_path, launcher, Some(deadline), poll_interval).await
}

/// Variant of [`connect_or_start`] with an optional startup deadline.
///
/// `None` means no deadline. This keeps the public `Duration`-based helper
/// compatible while letting the SDK facade map `Duration::MAX` to an
/// unbounded connect-or-start operation.
pub async fn connect_or_start_with_timeout<L, F>(
    socket_path: &Path,
    launcher: L,
    deadline: Option<Duration>,
    poll_interval: Duration,
) -> Result<StartupOutcome, StartupError>
where
    L: FnOnce() -> F,
    F: Future<Output = io::Result<()>>,
{
    let deadline = StartupDeadline::from_timeout(deadline);
    let owner_uid = real_user_id();

    let empty_socket_path = socket_path.as_os_str().is_empty();
    if can_probe_existing_socket_before_startup_validation(socket_path) {
        if let Some(stream) = try_connect_validated(socket_path, owner_uid).await? {
            return Ok(StartupOutcome::JoinedExisting(stream));
        }
    }

    let lock_path = if empty_socket_path {
        None
    } else {
        Some(startup_lock_path_for_filesystem_socket(
            socket_path,
            owner_uid,
        )?)
    };

    let lock_guard = match lock_path.as_deref() {
        Some(lock_path) => {
            Some(StartupLock::acquire(lock_path, owner_uid, deadline, poll_interval).await?)
        }
        None => None,
    };

    if let Some(stream) = try_connect_validated(socket_path, owner_uid).await? {
        drop(lock_guard);
        return Ok(StartupOutcome::JoinedExisting(stream));
    }

    if !empty_socket_path {
        prepare_socket_path_safe(socket_path, owner_uid)?;
    }

    launcher()
        .await
        .map_err(|error| StartupError::Launcher { source: error })?;

    let stream = wait_for_daemon(socket_path, owner_uid, deadline, poll_interval).await?;
    drop(lock_guard);
    Ok(StartupOutcome::Started(stream))
}

fn startup_lock_path_for_filesystem_socket(
    socket_path: &Path,
    owner_uid: u32,
) -> Result<PathBuf, StartupError> {
    let parent = socket_path
        .parent()
        .ok_or_else(|| StartupError::InvalidPath {
            reason: "socket path has no parent directory".to_owned(),
            path: socket_path.to_path_buf(),
        })?;
    if parent.as_os_str().is_empty() {
        return Err(StartupError::InvalidPath {
            reason: "socket path has an empty parent directory".to_owned(),
            path: socket_path.to_path_buf(),
        });
    }
    if socket_path.file_name().is_none() {
        return Err(StartupError::InvalidPath {
            reason: "socket path has no file name component".to_owned(),
            path: socket_path.to_path_buf(),
        });
    }

    prepare_socket_parent(socket_path, parent, owner_uid).map(|prepared| prepared.lock_path)
}

fn can_probe_existing_socket_before_startup_validation(socket_path: &Path) -> bool {
    if socket_path.as_os_str().is_empty() {
        return true;
    }
    let Some(parent) = socket_path.parent() else {
        return false;
    };
    !parent.as_os_str().is_empty() && socket_path.file_name().is_some()
}

async fn try_connect_validated(
    socket_path: &Path,
    owner_uid: u32,
) -> Result<Option<UnixStream>, StartupError> {
    if !socket_path.as_os_str().is_empty() {
        reject_socket_symlink(socket_path)?;
    }
    match connect_socket_path(socket_path).await {
        Ok(stream) => {
            if !socket_path.as_os_str().is_empty() {
                reject_socket_symlink(socket_path)?;
            }
            match validate_peer_credentials(&stream, owner_uid, socket_path) {
                Ok(()) => Ok(Some(stream)),
                Err(error) => Err(error),
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::TimedOut
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(StartupError::Filesystem {
            operation: "connect to daemon socket",
            path: socket_path.to_path_buf(),
            source: error,
        }),
    }
}

async fn connect_socket_path(socket_path: &Path) -> io::Result<UnixStream> {
    let socket_path = socket_path.to_path_buf();
    let stream = task::spawn_blocking(move || {
        let endpoint = rmux_ipc::resolve_endpoint(None, Some(socket_path.as_path()))?;
        rmux_ipc::connect_blocking(&endpoint, CONNECT_PROBE_TIMEOUT)
    })
    .await
    .map_err(io::Error::other)??;
    stream.set_nonblocking(true)?;
    UnixStream::from_std(stream)
}

fn validate_peer_credentials(
    stream: &UnixStream,
    expected_uid: u32,
    socket_path: &Path,
) -> Result<(), StartupError> {
    let credentials = stream
        .peer_cred()
        .map_err(|error| StartupError::Filesystem {
            operation: "read daemon peer credentials",
            path: socket_path.to_path_buf(),
            source: error,
        })?;
    let actual_uid = credentials.uid();
    if actual_uid == expected_uid {
        Ok(())
    } else {
        Err(StartupError::PeerCredentialMismatch {
            expected_uid,
            actual_uid,
            socket_path: socket_path.to_path_buf(),
        })
    }
}

async fn wait_for_daemon(
    socket_path: &Path,
    owner_uid: u32,
    deadline: StartupDeadline,
    poll_interval: Duration,
) -> Result<UnixStream, StartupError> {
    // The minimum poll interval keeps a misconfigured zero-interval caller
    // from spinning on the connect probe; anything below this is rounded up.
    const MIN_POLL_INTERVAL: Duration = Duration::from_millis(1);

    let max_poll = poll_interval.max(MIN_POLL_INTERVAL);
    let mut next_poll = MIN_POLL_INTERVAL.min(max_poll);
    loop {
        match try_connect_validated(socket_path, owner_uid).await {
            Ok(Some(stream)) => return Ok(stream),
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        if deadline.is_elapsed() {
            return Err(StartupError::StartupTimeout {
                socket_path: socket_path.to_path_buf(),
                waited: deadline.elapsed(),
            });
        }
        sleep(deadline.sleep_for(next_poll)).await;
        next_poll = (next_poll + next_poll).min(max_poll);
    }
}

#[cfg(test)]
#[path = "startup_unix/tests.rs"]
mod tests;
