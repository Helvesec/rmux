//! Windows daemon startup race serialization for the SDK bootstrap layer.
//!
//! The Windows hidden-daemon launch path needs the same "exactly one
//! launcher per endpoint" guarantee the Unix `flock`-based bootstrap gives.
//! On Windows the documented primitive is a per-user named mutex held over
//! the `CreateNamedPipeW`/`first_pipe_instance(true)` window. This module
//! owns that gate, layered on top of the existing `rmux-ipc` Windows pipe
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
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::future::Future;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rmux_ipc::{
    acquire_named_mutex, connect_blocking, BlockingLocalStream, LocalEndpoint, NamedMutexAcquire,
    NamedMutexError, MAX_NAMED_MUTEX_LEN,
};
use rmux_proto::{
    encode_frame, FrameDecoder, HasSessionRequest, Request, Response, RmuxError, SessionName,
};
use tokio::time::sleep;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_NO_DATA, ERROR_PIPE_BUSY,
    ERROR_PIPE_NOT_CONNECTED,
};

use crate::bootstrap::deadline::StartupDeadline;

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
    /// The startup mutex name derived from the pipe path violates the Win32
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

fn validate_pipe_name(pipe_name: &Path) -> Result<(), StartupError> {
    let value = pipe_name.as_os_str();
    if value.is_empty() {
        return Err(StartupError::InvalidPipeName {
            reason: "pipe name was empty".into(),
            pipe_name: pipe_name.to_path_buf(),
        });
    }
    let display = value.to_string_lossy();
    if !display
        .get(..PIPE_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PIPE_PREFIX))
    {
        return Err(StartupError::InvalidPipeName {
            reason: format!("pipe name must start with {PIPE_PREFIX:?}"),
            pipe_name: pipe_name.to_path_buf(),
        });
    }
    if pipe_name.file_name().is_none() {
        return Err(StartupError::InvalidPipeName {
            reason: "pipe name has no label component".into(),
            pipe_name: pipe_name.to_path_buf(),
        });
    }
    Ok(())
}

fn startup_mutex_name(pipe_name: &Path) -> Result<OsString, StartupError> {
    let display = pipe_name.as_os_str().to_string_lossy();
    if !display
        .get(..PIPE_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PIPE_PREFIX))
    {
        return Err(StartupError::InvalidPipeName {
            reason: format!("pipe name must start with {PIPE_PREFIX:?}"),
            pipe_name: pipe_name.to_path_buf(),
        });
    }

    // Win32 pipe names are case-insensitive but kernel mutex names are case-
    // sensitive in the default object namespace. Lowercase the entire derived
    // label so two callers using differently-cased pipe paths always derive
    // the same mutex name and actually serialize against each other. The SID
    // and integrity prefix remain unique once lowercased because no two
    // distinct identities collapse together under ASCII lowercasing.
    let label_lower = display[PIPE_PREFIX.len()..].to_ascii_lowercase();
    if label_lower.is_empty() {
        return Err(StartupError::InvalidPipeName {
            reason: "pipe name has no label component".into(),
            pipe_name: pipe_name.to_path_buf(),
        });
    }

    let candidate = format!("{STARTUP_MUTEX_PREFIX}{label_lower}");
    if candidate.len() > MAX_NAMED_MUTEX_LEN {
        return Err(StartupError::InvalidMutexName {
            reason: format!(
                "derived mutex name length {} exceeds {MAX_NAMED_MUTEX_LEN}",
                candidate.len()
            ),
            pipe_name: pipe_name.to_path_buf(),
        });
    }

    Ok(OsString::from(candidate))
}

/// Owns a named-mutex acquisition on a dedicated OS thread for the entire
/// lifetime of the startup race.
///
/// Win32 mutexes are owned per-thread. Crossing an `await` between acquire
/// and release would land the release on whichever runtime thread happens to
/// be polling, where `ReleaseMutex` silently no-ops with `ERROR_NOT_OWNER`.
/// We dedicate a single OS thread to acquire, hold, and release, then
/// terminate. Releasing the mutex is what lets the next loser-process wake
/// up and discover the daemon the winner just started.
struct StartupMutexHolder {
    release: Option<mpsc::SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl StartupMutexHolder {
    fn release(&mut self) {
        if let Some(tx) = self.release.take() {
            // Send may fail if the holder thread already exited (e.g. it
            // panicked while holding the guard); the join below surfaces
            // that, but there is no useful recovery here so we discard.
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            // Joining is bounded: the holder thread only runs the guard
            // drop after receiving the signal, which is microseconds of
            // syscall work. We discard panics for the same reason as above.
            let _ = thread.join();
        }
    }
}

impl Drop for StartupMutexHolder {
    fn drop(&mut self) {
        self.release();
    }
}

async fn acquire_startup_mutex(
    pipe_name: &Path,
    mutex_name: &OsStr,
    deadline: StartupDeadline,
) -> Result<StartupMutexHolder, StartupError> {
    let pipe_owned = pipe_name.to_path_buf();
    let mutex_owned = mutex_name.to_owned();
    let (acquire_tx, acquire_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);

    let thread = thread::Builder::new()
        .name("rmux-startup-mutex".to_owned())
        .spawn(move || {
            let mutex_wait = deadline.requested_timeout().unwrap_or(Duration::MAX);
            let outcome = acquire_named_mutex(&mutex_owned, mutex_wait);
            match outcome {
                Ok(NamedMutexAcquire::Created(guard))
                | Ok(NamedMutexAcquire::Opened(guard))
                | Ok(NamedMutexAcquire::Abandoned(guard)) => {
                    if acquire_tx.send(Ok(())).is_err() {
                        // The async caller dropped the receiver before we
                        // reported success; release immediately so we never
                        // strand the kernel mutex.
                        drop(guard);
                        return;
                    }
                    // Block until the holder is dropped (signal sent) or the
                    // sender is dropped (channel closed). Either way, drop
                    // here releases the mutex on the same thread that won
                    // initial ownership.
                    let _ = release_rx.recv();
                    drop(guard);
                }
                Err(error) => {
                    let _ = acquire_tx.send(Err(error));
                }
            }
        })
        .map_err(|source| StartupError::Mutex {
            pipe_name: pipe_owned.clone(),
            source,
        })?;

    let acquired = acquire_rx.await.map_err(|_canceled| StartupError::Mutex {
        pipe_name: pipe_owned.clone(),
        source: io::Error::other("startup mutex thread exited before reporting an outcome"),
    })?;

    match acquired {
        Ok(()) => Ok(StartupMutexHolder {
            release: Some(release_tx),
            thread: Some(thread),
        }),
        Err(error) => {
            // Holder thread already exited via the failure branch; join is
            // immediate but worth doing to surface any join error.
            let _ = thread.join();
            Err(map_named_mutex_error(error, pipe_name, deadline))
        }
    }
}

fn map_named_mutex_error(
    error: NamedMutexError,
    pipe_name: &Path,
    deadline: StartupDeadline,
) -> StartupError {
    match error {
        NamedMutexError::TimedOut => StartupError::MutexTimeout {
            pipe_name: pipe_name.to_path_buf(),
            waited: deadline.requested_timeout().unwrap_or(Duration::MAX),
        },
        NamedMutexError::AccessDenied(source) => StartupError::MutexAccessDenied {
            pipe_name: pipe_name.to_path_buf(),
            source,
        },
        NamedMutexError::InvalidName { reason } => StartupError::InvalidMutexName {
            reason,
            pipe_name: pipe_name.to_path_buf(),
        },
        NamedMutexError::SecurityDescriptor(source)
        | NamedMutexError::Create(source)
        | NamedMutexError::Wait(source) => StartupError::Mutex {
            pipe_name: pipe_name.to_path_buf(),
            source,
        },
    }
}

async fn probe_responsive(
    endpoint: &LocalEndpoint,
    pipe_name: &Path,
) -> Result<Option<BlockingLocalStream>, StartupError> {
    let endpoint_owned = endpoint.clone();
    let pipe_owned = pipe_name.to_path_buf();
    tokio::task::spawn_blocking(move || probe_blocking(&endpoint_owned, &pipe_owned))
        .await
        .map_err(|error| StartupError::PipeIo {
            operation: "join probe task",
            pipe_name: pipe_name.to_path_buf(),
            source: io::Error::other(format!("startup probe join failed: {error}")),
        })?
}

fn probe_blocking(
    endpoint: &LocalEndpoint,
    pipe_name: &Path,
) -> Result<Option<BlockingLocalStream>, StartupError> {
    let mut stream = match connect_blocking(endpoint, PROBE_CONNECT_TIMEOUT) {
        Ok(stream) => stream,
        Err(error) => return classify_connect_error(error, pipe_name).map(|()| None),
    };

    stream
        .set_write_timeout(Some(PROBE_IO_TIMEOUT))
        .map_err(|source| StartupError::PipeIo {
            operation: "set probe write timeout",
            pipe_name: pipe_name.to_path_buf(),
            source,
        })?;
    stream
        .set_read_timeout(Some(PROBE_IO_TIMEOUT))
        .map_err(|source| StartupError::PipeIo {
            operation: "set probe read timeout",
            pipe_name: pipe_name.to_path_buf(),
            source,
        })?;

    let target = SessionName::new(PROBE_SESSION_NAME).map_err(|error| StartupError::PipeIo {
        operation: "build probe session name",
        pipe_name: pipe_name.to_path_buf(),
        source: io::Error::other(error),
    })?;
    let request = Request::HasSession(HasSessionRequest { target });
    let frame = encode_frame(&request).map_err(|error| StartupError::PipeIo {
        operation: "encode probe frame",
        pipe_name: pipe_name.to_path_buf(),
        source: io::Error::other(error),
    })?;

    if let Err(error) = stream.write_all(&frame).and_then(|()| stream.flush()) {
        return classify_io_error(error, "send probe frame", pipe_name).map(|()| None);
    }

    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(None),
            Ok(bytes_read) => decoder.push_bytes(&buffer[..bytes_read]),
            Err(error) => {
                return classify_io_error(error, "read probe response", pipe_name).map(|()| None)
            }
        }
        match decoder.next_frame::<Response>() {
            Ok(Some(Response::HasSession(_))) => return Ok(Some(stream)),
            Ok(Some(response)) => {
                return Err(StartupError::PipeIo {
                    operation: "validate probe response",
                    pipe_name: pipe_name.to_path_buf(),
                    source: io::Error::other(format!(
                        "unexpected startup probe response: {response:?}"
                    )),
                });
            }
            Ok(None) => continue,
            Err(RmuxError::IncompleteFrame { .. }) => continue,
            Err(_) => return Ok(None),
        }
    }
}

fn classify_connect_error(error: io::Error, pipe_name: &Path) -> Result<(), StartupError> {
    if let Some(code) = error.raw_os_error() {
        if code == ERROR_FILE_NOT_FOUND as i32 {
            return Ok(());
        }
        if code == ERROR_PIPE_BUSY as i32 {
            return Err(StartupError::PipeBusy {
                pipe_name: pipe_name.to_path_buf(),
            });
        }
        if code == ERROR_NO_DATA as i32 || code == ERROR_PIPE_NOT_CONNECTED as i32 {
            return Ok(());
        }
        if code == ERROR_ACCESS_DENIED as i32 {
            return Err(StartupError::PipeAccessDenied {
                pipe_name: pipe_name.to_path_buf(),
            });
        }
    }

    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => Ok(()),
        io::ErrorKind::TimedOut => Err(StartupError::PipeBusy {
            pipe_name: pipe_name.to_path_buf(),
        }),
        io::ErrorKind::PermissionDenied => Err(StartupError::PipeAccessDenied {
            pipe_name: pipe_name.to_path_buf(),
        }),
        _ => Err(StartupError::PipeIo {
            operation: "open named pipe",
            pipe_name: pipe_name.to_path_buf(),
            source: error,
        }),
    }
}

fn classify_io_error(
    error: io::Error,
    operation: &'static str,
    pipe_name: &Path,
) -> Result<(), StartupError> {
    if let Some(code) = error.raw_os_error() {
        if code == ERROR_BROKEN_PIPE as i32
            || code == ERROR_PIPE_NOT_CONNECTED as i32
            || code == ERROR_NO_DATA as i32
            || code == ERROR_FILE_NOT_FOUND as i32
        {
            return Ok(());
        }
        if code == ERROR_ACCESS_DENIED as i32 {
            return Err(StartupError::PipeAccessDenied {
                pipe_name: pipe_name.to_path_buf(),
            });
        }
    }
    if matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotFound
            | io::ErrorKind::TimedOut
            | io::ErrorKind::UnexpectedEof
    ) {
        return Ok(());
    }

    Err(StartupError::PipeIo {
        operation,
        pipe_name: pipe_name.to_path_buf(),
        source: error,
    })
}

async fn wait_for_daemon(
    endpoint: &LocalEndpoint,
    pipe_name: &Path,
    deadline: StartupDeadline,
    poll_interval: Duration,
) -> Result<BlockingLocalStream, StartupError> {
    const MIN_POLL_INTERVAL: Duration = Duration::from_millis(1);

    let effective_poll = poll_interval.max(MIN_POLL_INTERVAL);

    loop {
        match probe_responsive(endpoint, pipe_name).await {
            Ok(Some(stream)) => return Ok(stream),
            Ok(None) => {}
            Err(error) if matches!(error, StartupError::PipeBusy { .. }) => {
                // Pipe instances momentarily exhausted while the daemon comes
                // up; treat as transient and keep polling within budget.
            }
            Err(error) => return Err(error),
        }

        if deadline.is_elapsed() {
            return Err(StartupError::StartupTimeout {
                pipe_name: pipe_name.to_path_buf(),
                waited: deadline.elapsed(),
            });
        }
        sleep(deadline.sleep_for(effective_poll)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe(label: &str) -> PathBuf {
        PathBuf::from(format!(r"\\.\pipe\rmux-S-1-5-21-1000-il-medium-{label}"))
    }

    #[test]
    fn validate_pipe_name_accepts_real_endpoint() {
        let pipe = pipe("default");
        validate_pipe_name(&pipe).expect("real pipe name accepted");
    }

    #[test]
    fn validate_pipe_name_rejects_unix_path() {
        let error = validate_pipe_name(Path::new("/tmp/not-a-pipe"))
            .expect_err("unix paths are not Windows pipe names");
        assert!(matches!(error, StartupError::InvalidPipeName { .. }));
    }

    #[test]
    fn validate_pipe_name_rejects_empty_path() {
        let error = validate_pipe_name(Path::new("")).expect_err("empty pipe path is invalid");
        assert!(matches!(error, StartupError::InvalidPipeName { .. }));
    }

    #[test]
    fn startup_mutex_name_strips_pipe_prefix() {
        let pipe = pipe("default");
        let name = startup_mutex_name(&pipe).expect("derive mutex name");
        let value = name.to_string_lossy();
        assert!(value.starts_with(STARTUP_MUTEX_PREFIX));
        assert!(value.ends_with("-default"));
    }

    #[test]
    fn startup_mutex_name_is_case_insensitive() {
        // The Win32 named-pipe namespace is case-insensitive, but the kernel
        // mutex namespace is case-sensitive. Two callers using the same
        // logical pipe with different case must derive the SAME mutex name
        // or they will fail to serialize against each other.
        let lower = PathBuf::from(r"\\.\pipe\rmux-s-1-5-21-1000-il-medium-default");
        let upper = PathBuf::from(r"\\.\PIPE\RMUX-S-1-5-21-1000-IL-MEDIUM-DEFAULT");
        let lower_name = startup_mutex_name(&lower).expect("lower pipe name accepted");
        let upper_name = startup_mutex_name(&upper).expect("upper pipe name accepted");
        assert_eq!(lower_name, upper_name);
    }

    #[test]
    fn startup_mutex_name_rejects_pipe_without_label() {
        let prefix_only = PathBuf::from(PIPE_PREFIX);
        let error =
            startup_mutex_name(&prefix_only).expect_err("prefix-only path must be rejected");
        assert!(matches!(error, StartupError::InvalidPipeName { .. }));
    }

    #[test]
    fn startup_mutex_name_rejects_oversized_label() {
        let label = "x".repeat(MAX_NAMED_MUTEX_LEN);
        let pipe = PathBuf::from(format!("{PIPE_PREFIX}rmux-{label}"));
        let error = startup_mutex_name(&pipe).expect_err("oversize mutex name must be rejected");
        assert!(matches!(error, StartupError::InvalidMutexName { .. }));
    }

    #[test]
    fn startup_mutex_holder_release_is_idempotent() {
        // The holder must tolerate `release()` running before `Drop` (and vice
        // versa) without panicking on the second call; the bootstrap fast-path
        // returns the holder by value but loser-paths drop it implicitly.
        let mut holder = StartupMutexHolder {
            release: None,
            thread: None,
        };
        holder.release();
        drop(holder);
    }

    #[tokio::test]
    async fn startup_mutex_holder_releases_on_acquiring_thread() {
        // Build a holder backed by a real dedicated OS thread but driven by a
        // local channel pair so the test can observe that:
        //
        // 1. The release signal is sent from the (potentially) async-runtime
        //    drop site.
        // 2. The holder thread itself observes the signal and performs the
        //    drop work on its own thread.
        //
        // This matches the production code path without needing an actual
        // Win32 mutex (which only exists on Windows).
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let (observed_tx, observed_rx) = mpsc::channel::<thread::ThreadId>();
        let thread = thread::Builder::new()
            .name("rmux-startup-mutex-test".to_owned())
            .spawn(move || {
                let _ = release_rx.recv();
                let _ = observed_tx.send(thread::current().id());
            })
            .expect("spawn holder test thread");
        let holder_thread_id = thread.thread().id();

        let mut holder = StartupMutexHolder {
            release: Some(release_tx),
            thread: Some(thread),
        };

        // First release: signals the worker.
        holder.release();
        let observed = observed_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("holder thread observes release");
        assert_eq!(
            observed, holder_thread_id,
            "release work must run on the holder thread, not the caller's thread"
        );

        // Second release / drop: must not block or panic even though the
        // worker already exited and the channel is closed.
        holder.release();
        drop(holder);
    }

    #[test]
    fn recoverable_matrix_matches_documented_contract() {
        let recoverable = [
            StartupError::Mutex {
                pipe_name: pipe("default"),
                source: io::Error::other("mutex"),
            },
            StartupError::MutexTimeout {
                pipe_name: pipe("default"),
                waited: Duration::from_millis(1),
            },
            StartupError::PipeBusy {
                pipe_name: pipe("default"),
            },
            StartupError::PipeNotFound {
                pipe_name: pipe("default"),
            },
            StartupError::PipeNoData {
                pipe_name: pipe("default"),
            },
            StartupError::Launcher {
                source: io::Error::other("launcher"),
            },
            StartupError::StartupTimeout {
                pipe_name: pipe("default"),
                waited: Duration::from_millis(1),
            },
        ];
        for error in recoverable {
            assert!(
                error.is_recoverable(),
                "expected recoverable, got {error:?}"
            );
        }

        let not_recoverable = [
            StartupError::InvalidPipeName {
                reason: "no prefix".into(),
                pipe_name: PathBuf::from("/tmp/x"),
            },
            StartupError::InvalidMutexName {
                reason: "too long".into(),
                pipe_name: pipe("default"),
            },
            StartupError::MutexAccessDenied {
                pipe_name: pipe("default"),
                source: io::Error::other("denied"),
            },
            StartupError::PipeAccessDenied {
                pipe_name: pipe("default"),
            },
            StartupError::PipeIo {
                operation: "stat",
                pipe_name: pipe("default"),
                source: io::Error::other("io"),
            },
        ];
        for error in not_recoverable {
            assert!(
                !error.is_recoverable(),
                "expected non-recoverable, got {error:?}"
            );
        }
    }
}
