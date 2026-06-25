//! Windows daemon startup race serialization for the SDK bootstrap layer.
//!
//! The Windows hidden-daemon launch path needs the same "exactly one
//! launcher per endpoint" guarantee the Unix `flock`-based bootstrap gives.
//! On Windows the documented primitive is a per-user named mutex held over
//! the `CreateNamedPipeW`/`first_pipe_instance(true)` window. This module
//! owns that check, layered on top of the existing `rmux-ipc` Windows pipe
//! contract:
//!
//! * Endpoint names stay `\\.\pipe\rmux-{SID}-il-{integrity}-{label}`. This
//!   module never invents new pipe names.
//! * The same `IdentityResolver`/SID values that scope the pipe ACL also
//!   scope the mutex's discretionary ACL, so a peer running under a
//!   different identity cannot acquire the mutex or open the pipe.
//! * `ServerOptions::first_pipe_instance(true)` remains the authoritative
//!   first-instance enforcement inside `rmux-ipc`. The mutex prevents two
//!   `rmux` callers from racing to spawn that listener; it does not
//!   substitute for it.
//!
//! Race guard:
//!
//! 1. Probe the pipe with the existing framed bincode `HasSession` request.
//!    If the daemon answers, [`StartupOutcome::JoinedExisting`] is returned
//!    without ever touching the mutex.
//! 2. Otherwise acquire the per-endpoint named mutex.
//! 3. Re-probe under the mutex. If a peer started the daemon while we were
//!    waiting, return [`StartupOutcome::JoinedExisting`] without spawning.
//! 4. Run the launcher closure exactly once and wait for the new daemon to
//!    respond to the same probe.
//!
//! Busy/not-found/no-data/access-denied/timeout errors raised by the pipe or
//! the mutex surface as typed [`StartupError`] variants.

#![cfg(windows)]

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rmux_ipc::{BlockingLocalStream, LocalEndpoint};

use crate::bootstrap::deadline::StartupDeadline;

#[path = "startup_windows/mutex.rs"]
mod mutex;
#[path = "startup_windows/name.rs"]
mod name;
#[path = "startup_windows/probe.rs"]
mod probe;

use mutex::{acquire_startup_mutex, acquire_startup_mutex_blocking};
use name::{startup_mutex_name, validate_pipe_name};
use probe::{probe_blocking, probe_responsive, wait_for_daemon, wait_for_daemon_blocking};

const PIPE_PREFIX: &str = r"\\.\pipe\";
const STARTUP_MUTEX_PREFIX: &str = r"Local\rmux-startup-";
const PROBE_CONNECT_TIMEOUT: Duration = Duration::from_millis(200);
const PROBE_IO_TIMEOUT: Duration = Duration::from_millis(250);
const PROBE_SESSION_NAME: &str = "__rmux_startup_probe__";

/// Default deadline a startup owner waits for the launched daemon to bind.
pub const DEFAULT_STARTUP_DEADLINE: Duration = Duration::from_secs(5);
/// Default poll interval used while waiting for the daemon to become ready.
pub const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Outcome of [`connect_or_start`].
#[derive(Debug)]
pub enum StartupOutcome {
    /// The caller acquired the startup mutex, ran the launcher, and
    /// connected to the daemon it just started.
    Started(BlockingLocalStream),
    /// The caller connected to a daemon that was already serving the
    /// endpoint (either before any mutex attempt or after losing the race).
    JoinedExisting(BlockingLocalStream),
}

impl StartupOutcome {
    /// Consume the outcome and return only the connected stream.
    #[must_use]
    pub fn into_stream(self) -> BlockingLocalStream {
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
    /// The supplied pipe path was empty or otherwise structurally invalid.
    InvalidPipeName {
        /// Visible reason describing why the pipe name was rejected.
        reason: String,
        /// Pipe path that was rejected.
        pipe_name: PathBuf,
    },
    /// The startup mutex name computed from the pipe path violates the Win32
    /// kernel-object name length limit.
    InvalidMutexName {
        /// Visible reason describing why the mutex name was rejected.
        reason: String,
        /// Pipe path the mutex would have guarded.
        pipe_name: PathBuf,
    },
    /// Building or acquiring the per-endpoint named mutex failed.
    Mutex {
        /// Pipe path the mutex was protecting.
        pipe_name: PathBuf,
        /// Underlying error from the mutex primitive.
        source: io::Error,
    },
    /// The mutex was held by another process and the wait elapsed.
    MutexTimeout {
        /// Pipe path the mutex was protecting.
        pipe_name: PathBuf,
        /// Total wait duration.
        waited: Duration,
    },
    /// `CreateMutexExW` returned `ERROR_ACCESS_DENIED`, meaning a peer
    /// running under a different identity holds the same name.
    MutexAccessDenied {
        /// Pipe path the mutex would have protected.
        pipe_name: PathBuf,
        /// Underlying OS error.
        source: io::Error,
    },
    /// All instances of the named pipe were busy when probing.
    PipeBusy {
        /// Pipe path that returned busy.
        pipe_name: PathBuf,
    },
    /// `CreateFile` reported `ERROR_FILE_NOT_FOUND`; no daemon was listening.
    PipeNotFound {
        /// Pipe path that returned not-found.
        pipe_name: PathBuf,
    },
    /// The pipe instance was closed mid-handshake.
    PipeNoData {
        /// Pipe path that returned no-data.
        pipe_name: PathBuf,
    },
    /// `CreateFile` reported `ERROR_ACCESS_DENIED` when probing the pipe.
    PipeAccessDenied {
        /// Pipe path that rejected the probe.
        pipe_name: PathBuf,
    },
    /// Any other I/O error during pipe probing.
    PipeIo {
        /// Short stable identifier for the failing step.
        operation: &'static str,
        /// Pipe path the operation targeted.
        pipe_name: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The launcher closure failed to spawn the daemon.
    Launcher {
        /// Underlying I/O error reported by the launcher closure.
        source: io::Error,
    },
    /// The startup deadline elapsed before the daemon answered the probe.
    StartupTimeout {
        /// Pipe path that never came up in time.
        pipe_name: PathBuf,
        /// Total time the caller waited.
        waited: Duration,
    },
}

impl StartupError {
    /// Returns whether the error is one of the documented recoverable loser
    /// outcomes. A caller that hits a recoverable error may retry the same
    /// endpoint or surface it as a transient bootstrap failure.
    #[must_use]
    pub const fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Self::Mutex { .. }
                | Self::MutexTimeout { .. }
                | Self::PipeBusy { .. }
                | Self::PipeNotFound { .. }
                | Self::PipeNoData { .. }
                | Self::Launcher { .. }
                | Self::StartupTimeout { .. }
        )
    }
}

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPipeName { reason, pipe_name } => write!(
                formatter,
                "rmux startup rejected pipe '{}': {reason}",
                pipe_name.display()
            ),
            Self::InvalidMutexName { reason, pipe_name } => write!(
                formatter,
                "rmux startup rejected mutex name for '{}': {reason}",
                pipe_name.display()
            ),
            Self::Mutex { pipe_name, source } => write!(
                formatter,
                "rmux startup mutex for '{}' failed: {source}",
                pipe_name.display()
            ),
            Self::MutexTimeout { pipe_name, waited } => write!(
                formatter,
                "rmux startup mutex for '{}' timed out after {}ms",
                pipe_name.display(),
                waited.as_millis()
            ),
            Self::MutexAccessDenied { pipe_name, source } => write!(
                formatter,
                "rmux startup mutex for '{}' denied for current user: {source}",
                pipe_name.display()
            ),
            Self::PipeBusy { pipe_name } => write!(
                formatter,
                "rmux pipe '{}' is busy on every instance",
                pipe_name.display()
            ),
            Self::PipeNotFound { pipe_name } => write!(
                formatter,
                "rmux pipe '{}' is not currently served",
                pipe_name.display()
            ),
            Self::PipeNoData { pipe_name } => write!(
                formatter,
                "rmux pipe '{}' closed mid-handshake",
                pipe_name.display()
            ),
            Self::PipeAccessDenied { pipe_name } => write!(
                formatter,
                "rmux pipe '{}' denied current user access",
                pipe_name.display()
            ),
            Self::PipeIo {
                operation,
                pipe_name,
                source,
            } => write!(
                formatter,
                "rmux pipe '{}' failed to {operation}: {source}",
                pipe_name.display()
            ),
            Self::Launcher { source } => {
                write!(formatter, "rmux startup launcher failed: {source}")
            }
            Self::StartupTimeout { pipe_name, waited } => write!(
                formatter,
                "rmux startup timed out after {}ms waiting for '{}' to answer",
                waited.as_millis(),
                pipe_name.display()
            ),
        }
    }
}

impl Error for StartupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Mutex { source, .. }
            | Self::MutexAccessDenied { source, .. }
            | Self::PipeIo { source, .. }
            | Self::Launcher { source } => Some(source),
            _ => None,
        }
    }
}

/// Connects to the daemon serving `pipe_name`, starting it under a
/// per-endpoint named mutex if no live daemon is reachable.
pub async fn connect_or_start<L, F>(
    pipe_name: &Path,
    launcher: L,
) -> Result<StartupOutcome, StartupError>
where
    L: FnOnce() -> F,
    F: Future<Output = io::Result<()>>,
{
    connect_or_start_with(
        pipe_name,
        launcher,
        DEFAULT_STARTUP_DEADLINE,
        STARTUP_POLL_INTERVAL,
    )
    .await
}

/// Variant of [`connect_or_start`] with an explicit deadline and poll
/// interval.
pub async fn connect_or_start_with<L, F>(
    pipe_name: &Path,
    launcher: L,
    deadline: Duration,
    poll_interval: Duration,
) -> Result<StartupOutcome, StartupError>
where
    L: FnOnce() -> F,
    F: Future<Output = io::Result<()>>,
{
    connect_or_start_with_timeout(pipe_name, launcher, Some(deadline), poll_interval).await
}

/// Variant of [`connect_or_start`] with an optional startup deadline.
///
/// `None` means no deadline for daemon readiness. The Win32 mutex primitive
/// still receives a clamped `Duration::MAX` wait because its public API is
/// duration-based.
pub async fn connect_or_start_with_timeout<L, F>(
    pipe_name: &Path,
    launcher: L,
    deadline: Option<Duration>,
    poll_interval: Duration,
) -> Result<StartupOutcome, StartupError>
where
    L: FnOnce() -> F,
    F: Future<Output = io::Result<()>>,
{
    let deadline = StartupDeadline::from_timeout(deadline);
    validate_pipe_name(pipe_name)?;
    let endpoint = LocalEndpoint::from_path(pipe_name.to_path_buf());

    if let Some(stream) = probe_responsive(&endpoint, pipe_name).await? {
        return Ok(StartupOutcome::JoinedExisting(stream));
    }

    let mutex_name = startup_mutex_name(pipe_name)?;
    let _guard = acquire_startup_mutex(pipe_name, &mutex_name, deadline).await?;

    if let Some(stream) = probe_responsive(&endpoint, pipe_name).await? {
        // Drop the guard implicitly at end of scope; the daemon another
        // caller started is already responsive.
        return Ok(StartupOutcome::JoinedExisting(stream));
    }

    launcher()
        .await
        .map_err(|source| StartupError::Launcher { source })?;

    let stream = wait_for_daemon(&endpoint, pipe_name, deadline, poll_interval).await?;
    drop(_guard);
    Ok(StartupOutcome::Started(stream))
}

/// Blocking variant of [`connect_or_start_with`] for synchronous Windows
/// clients that would otherwise pay for a Tokio runtime only to spawn a
/// hidden daemon and poll a named pipe.
pub fn connect_or_start_blocking_with<L>(
    pipe_name: &Path,
    launcher: L,
    deadline: Duration,
    poll_interval: Duration,
) -> Result<StartupOutcome, StartupError>
where
    L: FnOnce() -> io::Result<()>,
{
    connect_or_start_blocking_with_timeout(pipe_name, launcher, Some(deadline), poll_interval)
}

/// Blocking variant of [`connect_or_start_with_timeout`].
pub fn connect_or_start_blocking_with_timeout<L>(
    pipe_name: &Path,
    launcher: L,
    deadline: Option<Duration>,
    poll_interval: Duration,
) -> Result<StartupOutcome, StartupError>
where
    L: FnOnce() -> io::Result<()>,
{
    let deadline = StartupDeadline::from_timeout(deadline);
    validate_pipe_name(pipe_name)?;
    let endpoint = LocalEndpoint::from_path(pipe_name.to_path_buf());

    if let Some(stream) = probe_blocking(&endpoint, pipe_name)? {
        return Ok(StartupOutcome::JoinedExisting(stream));
    }

    let mutex_name = startup_mutex_name(pipe_name)?;
    let guard = acquire_startup_mutex_blocking(pipe_name, &mutex_name, deadline)?;

    if let Some(stream) = probe_blocking(&endpoint, pipe_name)? {
        drop(guard);
        return Ok(StartupOutcome::JoinedExisting(stream));
    }

    launcher().map_err(|source| StartupError::Launcher { source })?;

    let stream = wait_for_daemon_blocking(&endpoint, pipe_name, deadline, poll_interval)?;
    drop(guard);
    Ok(StartupOutcome::Started(stream))
}

#[cfg(test)]
#[path = "startup_windows/tests.rs"]
mod tests;
