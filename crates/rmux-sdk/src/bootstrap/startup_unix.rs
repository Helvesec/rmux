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
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::fs::{flock, FlockOperation};
use tokio::net::UnixStream;
use tokio::time::sleep;

use crate::bootstrap::deadline::StartupDeadline;
use rmux_os::identity::real_user_id;

/// Permission bits enforced for the per-endpoint startup lock file.
pub const STARTUP_LOCK_MODE: u32 = 0o600;
/// Permission bits enforced for the owning `rmux-$uid` socket directory.
pub const SOCKET_DIRECTORY_MODE: u32 = 0o700;
/// World/group bit mask; any of these set on a socket-related path is unsafe.
pub const UNSAFE_PERMISSION_MASK: u32 = 0o077;
/// Default deadline a startup owner waits for the launched daemon to bind.
pub const DEFAULT_STARTUP_DEADLINE: Duration = Duration::from_secs(5);
/// Default poll interval used while waiting for the daemon to become ready.
pub const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(25);

const STALE_PROBE_TIMEOUT: Duration = Duration::from_millis(50);

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

    if let Some(stream) = try_connect_validated(socket_path, owner_uid).await? {
        return Ok(StartupOutcome::JoinedExisting(stream));
    }

    ensure_owner_only_directory(parent, owner_uid)?;

    let lock_path = startup_lock_path(socket_path);
    let lock_guard = StartupLock::acquire(&lock_path, owner_uid, deadline, poll_interval).await?;

    if let Some(stream) = try_connect_validated(socket_path, owner_uid).await? {
        drop(lock_guard);
        return Ok(StartupOutcome::JoinedExisting(stream));
    }

    prepare_socket_path_safe(socket_path, owner_uid)?;

    launcher()
        .await
        .map_err(|error| StartupError::Launcher { source: error })?;

    let stream = wait_for_daemon(socket_path, owner_uid, deadline, poll_interval).await?;
    drop(lock_guard);
    Ok(StartupOutcome::Started(stream))
}

async fn try_connect_validated(
    socket_path: &Path,
    owner_uid: u32,
) -> Result<Option<UnixStream>, StartupError> {
    reject_socket_symlink(socket_path)?;
    match UnixStream::connect(socket_path).await {
        Ok(stream) => {
            reject_socket_symlink(socket_path)?;
            match validate_peer_credentials(&stream, owner_uid, socket_path) {
                Ok(()) => Ok(Some(stream)),
                Err(error) => Err(error),
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
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

fn reject_socket_symlink(socket_path: &Path) -> Result<(), StartupError> {
    match fs::symlink_metadata(socket_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(StartupError::SymlinkRejected {
            path: socket_path.to_path_buf(),
        }),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StartupError::Filesystem {
            operation: "stat daemon socket for symlink check",
            path: socket_path.to_path_buf(),
            source: error,
        }),
    }
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

    let effective_poll = poll_interval.max(MIN_POLL_INTERVAL);
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
        sleep(deadline.sleep_for(effective_poll)).await;
    }
}

fn startup_lock_path(socket_path: &Path) -> PathBuf {
    let mut lock_name = socket_path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    lock_name.push(".startup-lock");
    let parent = socket_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    parent.join(lock_name)
}

fn ensure_owner_only_directory(path: &Path, owner_uid: u32) -> Result<(), StartupError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_directory_metadata(path, &metadata, owner_uid),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_owner_only_directory(path)?;
            let metadata =
                fs::symlink_metadata(path).map_err(|error| StartupError::Filesystem {
                    operation: "stat owner-only directory after create",
                    path: path.to_path_buf(),
                    source: error,
                })?;
            validate_directory_metadata(path, &metadata, owner_uid)
        }
        Err(error) => Err(StartupError::Filesystem {
            operation: "stat owner-only directory",
            path: path.to_path_buf(),
            source: error,
        }),
    }
}

fn validate_directory_metadata(
    path: &Path,
    metadata: &fs::Metadata,
    owner_uid: u32,
) -> Result<(), StartupError> {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(StartupError::SymlinkRejected {
            path: path.to_path_buf(),
        });
    }
    if !file_type.is_dir() {
        return Err(StartupError::Filesystem {
            operation: "ensure owner-only directory",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::AlreadyExists,
                "expected a directory at this path",
            ),
        });
    }
    if metadata.uid() != owner_uid {
        return Err(StartupError::UnsafeOwner {
            path: path.to_path_buf(),
            expected_uid: owner_uid,
            actual_uid: metadata.uid(),
        });
    }
    let mode = metadata.mode() & 0o7777;
    if mode != SOCKET_DIRECTORY_MODE {
        let permissions = fs::Permissions::from_mode(SOCKET_DIRECTORY_MODE);
        fs::set_permissions(path, permissions).map_err(|error| StartupError::Filesystem {
            operation: "tighten directory permissions",
            path: path.to_path_buf(),
            source: error,
        })?;
        let metadata = fs::symlink_metadata(path).map_err(|error| StartupError::Filesystem {
            operation: "stat owner-only directory after chmod",
            path: path.to_path_buf(),
            source: error,
        })?;
        let mode = metadata.mode() & 0o7777;
        if mode & UNSAFE_PERMISSION_MASK != 0 {
            return Err(StartupError::UnsafePermissions {
                path: path.to_path_buf(),
                mode,
            });
        }
    }
    Ok(())
}

fn create_owner_only_directory(path: &Path) -> Result<(), StartupError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    builder.mode(SOCKET_DIRECTORY_MODE);
    builder
        .create(path)
        .map_err(|error| StartupError::Filesystem {
            operation: "create owner-only directory",
            path: path.to_path_buf(),
            source: error,
        })
}

fn prepare_socket_path_safe(socket_path: &Path, owner_uid: u32) -> Result<(), StartupError> {
    match fs::symlink_metadata(socket_path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                return Err(StartupError::SymlinkRejected {
                    path: socket_path.to_path_buf(),
                });
            }
            if !file_type.is_socket() {
                return Err(StartupError::Filesystem {
                    operation: "remove non-socket residue",
                    path: socket_path.to_path_buf(),
                    source: io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "endpoint path exists and is not a Unix socket",
                    ),
                });
            }
            if metadata.uid() != owner_uid {
                return Err(StartupError::UnsafeOwner {
                    path: socket_path.to_path_buf(),
                    expected_uid: owner_uid,
                    actual_uid: metadata.uid(),
                });
            }
            if !stale_socket_unanswered(socket_path)? {
                return Err(StartupError::Filesystem {
                    operation: "treat answering socket as stale",
                    path: socket_path.to_path_buf(),
                    source: io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "another rmux daemon is already answering this endpoint",
                    ),
                });
            }
            fs::remove_file(socket_path).map_err(|error| StartupError::Filesystem {
                operation: "remove stale socket",
                path: socket_path.to_path_buf(),
                source: error,
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StartupError::Filesystem {
            operation: "stat socket path",
            path: socket_path.to_path_buf(),
            source: error,
        }),
    }
}

fn stale_socket_unanswered(socket_path: &Path) -> Result<bool, StartupError> {
    use std::os::unix::net::UnixStream as StdUnixStream;

    match StdUnixStream::connect(socket_path) {
        Ok(stream) => {
            // Drop the probe stream immediately; we only needed the connect
            // result. The timeout on the closing handshake guards against a
            // peer that accepts but never reads a goodbye frame.
            let _ = stream.set_read_timeout(Some(STALE_PROBE_TIMEOUT));
            drop(stream);
            Ok(false)
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            Ok(true)
        }
        Err(error) => Err(StartupError::Filesystem {
            operation: "probe potentially stale socket",
            path: socket_path.to_path_buf(),
            source: error,
        }),
    }
}

#[derive(Debug)]
struct StartupLock {
    file: fs::File,
}

impl StartupLock {
    async fn acquire(
        path: &Path,
        owner_uid: u32,
        deadline: StartupDeadline,
        poll_interval: Duration,
    ) -> Result<Self, StartupError> {
        if let Ok(metadata) = fs::symlink_metadata(path) {
            validate_lock_metadata(path, &metadata, owner_uid)?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .mode(STARTUP_LOCK_MODE)
            .open(path)
            .map_err(|error| StartupError::Lock {
                path: path.to_path_buf(),
                source: error,
            })?;

        let metadata = file.metadata().map_err(|error| StartupError::Lock {
            path: path.to_path_buf(),
            source: error,
        })?;
        validate_lock_metadata(path, &metadata, owner_uid)?;

        acquire_lock_with_deadline(path, &file, deadline, poll_interval).await?;

        let metadata = file.metadata().map_err(|error| StartupError::Lock {
            path: path.to_path_buf(),
            source: error,
        })?;
        validate_lock_metadata(path, &metadata, owner_uid)?;
        validate_locked_file_is_still_named(path, &metadata)?;

        Ok(Self { file })
    }
}

fn validate_lock_metadata(
    path: &Path,
    metadata: &fs::Metadata,
    owner_uid: u32,
) -> Result<(), StartupError> {
    if metadata.file_type().is_symlink() {
        return Err(StartupError::SymlinkRejected {
            path: path.to_path_buf(),
        });
    }
    if !metadata.file_type().is_file() {
        return Err(StartupError::Filesystem {
            operation: "validate lock file is a regular file",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "startup lock path is not a regular file",
            ),
        });
    }
    if metadata.uid() != owner_uid {
        return Err(StartupError::UnsafeOwner {
            path: path.to_path_buf(),
            expected_uid: owner_uid,
            actual_uid: metadata.uid(),
        });
    }
    let mode = metadata.mode() & 0o7777;
    if mode & UNSAFE_PERMISSION_MASK != 0 {
        return Err(StartupError::UnsafePermissions {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

async fn acquire_lock_with_deadline(
    path: &Path,
    file: &fs::File,
    deadline: StartupDeadline,
    poll_interval: Duration,
) -> Result<(), StartupError> {
    const MIN_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(1);

    let effective_poll = poll_interval.max(MIN_LOCK_POLL_INTERVAL);

    loop {
        match flock(file.as_fd(), FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let source = io::Error::from(error);
                if source.kind() != io::ErrorKind::WouldBlock {
                    return Err(StartupError::Lock {
                        path: path.to_path_buf(),
                        source,
                    });
                }

                if deadline.is_elapsed() {
                    return Err(StartupError::Lock {
                        path: path.to_path_buf(),
                        source: io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!(
                                "timed out after {}ms waiting for startup lock",
                                deadline.elapsed().as_millis()
                            ),
                        ),
                    });
                }

                sleep(deadline.sleep_for(effective_poll)).await;
            }
        }
    }
}

fn validate_locked_file_is_still_named(
    path: &Path,
    locked_metadata: &fs::Metadata,
) -> Result<(), StartupError> {
    let path_metadata = fs::symlink_metadata(path).map_err(|error| StartupError::Lock {
        path: path.to_path_buf(),
        source: error,
    })?;
    if path_metadata.file_type().is_symlink() {
        return Err(StartupError::SymlinkRejected {
            path: path.to_path_buf(),
        });
    }
    let lock_file_changed = path_metadata.dev() != locked_metadata.dev()
        || path_metadata.ino() != locked_metadata.ino();
    if lock_file_changed {
        return Err(StartupError::Lock {
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::WouldBlock,
                "startup lock file changed while acquiring lock",
            ),
        });
    }
    Ok(())
}

impl Drop for StartupLock {
    fn drop(&mut self) {
        // Closing the descriptor releases the flock; the explicit unlock
        // makes the release point obvious to anyone tracing the lock.
        let _ = flock(self.file.as_fd(), FlockOperation::Unlock);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn unique_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "rmux-startup-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn startup_lock_path_uses_sibling_filename() {
        let socket = PathBuf::from("/tmp/rmux-1000/default");
        assert_eq!(
            startup_lock_path(&socket),
            PathBuf::from("/tmp/rmux-1000/default.startup-lock")
        );
    }

    #[tokio::test]
    async fn launcher_runs_once_when_only_one_caller() {
        let dir = unique_dir("solo");
        fs::create_dir_all(&dir).expect("temp dir");
        let socket = dir.join("default");
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = Arc::clone(&calls);

        let result = connect_or_start_with(
            &socket,
            move || async move {
                calls_clone.fetch_add(1, Ordering::SeqCst);
                Err(io::Error::other("no daemon for solo"))
            },
            Duration::from_millis(50),
            Duration::from_millis(10),
        )
        .await;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        match result {
            Err(StartupError::Launcher { .. }) => {}
            other => panic!("expected Launcher error, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn invalid_path_when_socket_path_has_no_parent() {
        let socket = PathBuf::from("/");
        let result = connect_or_start_with(
            &socket,
            || async { Err::<(), io::Error>(io::Error::other("never")) },
            Duration::from_millis(10),
            Duration::from_millis(5),
        )
        .await;

        assert!(matches!(result, Err(StartupError::InvalidPath { .. })));
    }

    #[tokio::test]
    async fn lock_acquisition_times_out_when_lock_is_held() {
        let dir = unique_dir("held-lock");
        fs::create_dir_all(&dir).expect("temp dir");
        let lock_path = dir.join("default.startup-lock");
        let holder = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_CLOEXEC)
            .mode(STARTUP_LOCK_MODE)
            .open(&lock_path)
            .expect("open held lock");
        flock(holder.as_fd(), FlockOperation::LockExclusive).expect("hold startup lock");

        let result = StartupLock::acquire(
            &lock_path,
            real_user_id(),
            StartupDeadline::from_timeout(Some(Duration::from_millis(20))),
            Duration::from_millis(5),
        )
        .await;

        match result {
            Err(StartupError::Lock { path, source }) => {
                assert_eq!(path, lock_path);
                assert_eq!(source.kind(), io::ErrorKind::TimedOut);
            }
            other => panic!("expected timed-out Lock error, got {other:?}"),
        }

        drop(holder);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn recoverable_matrix_matches_documented_contract() {
        let recoverable = [
            StartupError::Lock {
                path: PathBuf::from("/tmp/lock"),
                source: io::Error::other("lock"),
            },
            StartupError::Launcher {
                source: io::Error::other("launcher"),
            },
            StartupError::StartupTimeout {
                socket_path: PathBuf::from("/tmp/sock"),
                waited: Duration::from_millis(1),
            },
            StartupError::PeerCredentialMismatch {
                expected_uid: 1000,
                actual_uid: 1001,
                socket_path: PathBuf::from("/tmp/sock"),
            },
        ];
        for error in recoverable {
            assert!(
                error.is_recoverable(),
                "expected recoverable, got {error:?}"
            );
        }

        let not_recoverable = [
            StartupError::InvalidPath {
                reason: "no parent".to_owned(),
                path: PathBuf::from("/"),
            },
            StartupError::SymlinkRejected {
                path: PathBuf::from("/tmp/sym"),
            },
            StartupError::Filesystem {
                operation: "stat",
                path: PathBuf::from("/tmp/x"),
                source: io::Error::other("fs"),
            },
            StartupError::UnsafeOwner {
                path: PathBuf::from("/tmp/x"),
                expected_uid: 1000,
                actual_uid: 0,
            },
            StartupError::UnsafePermissions {
                path: PathBuf::from("/tmp/x"),
                mode: 0o644,
            },
        ];
        for error in not_recoverable {
            assert!(
                !error.is_recoverable(),
                "expected non-recoverable, got {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn startup_outcome_is_owner_only_for_started() {
        let dir = unique_dir("outcome-isowner");
        fs::create_dir_all(&dir).expect("temp dir");
        let socket = dir.join("default");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind helper listener");
        let accept = tokio::spawn(async move { listener.accept().await });

        let stream = UnixStream::connect(&socket).await.expect("connect helper");
        let started = StartupOutcome::Started(stream);
        assert!(started.is_owner());
        let joined = StartupOutcome::JoinedExisting(started.into_stream());
        assert!(!joined.is_owner());
        drop(joined);

        let _ = accept.await;
        let _ = fs::remove_dir_all(&dir);
    }
}
