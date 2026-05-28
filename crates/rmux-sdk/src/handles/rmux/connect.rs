use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::process::{Command, Stdio};
use std::time::Duration;

#[cfg(windows)]
use crate::bootstrap::deadline::StartupDeadline;
use crate::bootstrap::discovery;
#[cfg(windows)]
use crate::diagnostics::FEATURE_TRANSPORT_UNIX_SOCKET;
#[cfg(unix)]
use crate::diagnostics::FEATURE_TRANSPORT_WINDOWS_PIPE;
use crate::transport::TransportClient;
use crate::{Result, RmuxEndpoint, RmuxError};
#[cfg(windows)]
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_NOT_CONNECTED,
};

const INTERNAL_DAEMON_FLAG: &str = "--__internal-daemon";
#[cfg(windows)]
const WINDOWS_CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(10);

#[cfg(unix)]
pub(super) async fn connect_transport(
    endpoint: &RmuxEndpoint,
    timeout: Option<Duration>,
) -> Result<TransportClient> {
    match endpoint {
        RmuxEndpoint::UnixSocket(path) => {
            let stream = timeout_io("connect to rmux daemon", timeout, async {
                tokio::net::UnixStream::connect(path).await
            })
            .await?;
            Ok(TransportClient::spawn(stream))
        }
        RmuxEndpoint::WindowsPipe(_) => Err(RmuxError::unsupported(
            FEATURE_TRANSPORT_WINDOWS_PIPE,
            "use a Unix socket endpoint on Unix SDK builds",
        )),
        RmuxEndpoint::Default => Err(RmuxError::transport(
            "resolve rmux SDK endpoint",
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "default endpoint was not resolved before connecting",
            ),
        )),
    }
}

pub(crate) async fn connect_transport_to_endpoint(
    endpoint: &RmuxEndpoint,
    timeout: Option<Duration>,
) -> Result<TransportClient> {
    connect_transport(endpoint, timeout).await
}

pub(crate) async fn connect_or_start_transport(
    endpoint: &RmuxEndpoint,
    default_timeout: Option<Duration>,
) -> Result<TransportClient> {
    connect_or_start_transport_for_platform(endpoint, default_timeout).await
}

#[cfg(unix)]
async fn connect_or_start_transport_for_platform(
    endpoint: &RmuxEndpoint,
    default_timeout: Option<Duration>,
) -> Result<TransportClient> {
    let timeout = startup_operation_timeout(default_timeout);
    let RmuxEndpoint::UnixSocket(socket_path) = endpoint else {
        return connect_transport(endpoint, timeout).await;
    };
    let socket_path = socket_path.clone();
    let outcome = crate::bootstrap::startup_unix::connect_or_start_with_timeout(
        &socket_path,
        || {
            let socket_path = socket_path.clone();
            async move { spawn_hidden_daemon(socket_path.as_os_str()) }
        },
        timeout,
        crate::bootstrap::startup_unix::STARTUP_POLL_INTERVAL,
    )
    .await
    .map_err(startup_error)?;
    Ok(TransportClient::spawn(outcome.into_stream()))
}

#[cfg(windows)]
async fn connect_or_start_transport_for_platform(
    endpoint: &RmuxEndpoint,
    default_timeout: Option<Duration>,
) -> Result<TransportClient> {
    let timeout = startup_operation_timeout(default_timeout);
    let startup_deadline = StartupDeadline::from_timeout(timeout);
    let RmuxEndpoint::WindowsPipe(pipe) = endpoint else {
        return connect_transport(endpoint, timeout).await;
    };
    let pipe_path = std::path::PathBuf::from(pipe);
    let outcome = crate::bootstrap::startup_windows::connect_or_start_with_timeout(
        &pipe_path,
        || {
            let pipe_path = pipe_path.clone();
            async move { spawn_hidden_daemon(pipe_path.as_os_str()) }
        },
        startup_deadline.requested_timeout(),
        crate::bootstrap::startup_windows::STARTUP_POLL_INTERVAL,
    )
    .await
    .map_err(startup_error)?;
    // Windows startup probes use a blocking client stream owned by a private
    // Tokio runtime. The SDK transport actor must own an async pipe client on
    // the caller's runtime, so reconnect here with the same configured retry
    // budget instead of using a raw one-shot open. Reuse only the remaining
    // startup budget so connect_or_start never becomes startup timeout plus
    // another full connect timeout.
    drop_windows_startup_probe_stream(outcome).await?;
    connect_transport(endpoint, startup_deadline.remaining_timeout()).await
}

#[cfg(windows)]
async fn drop_windows_startup_probe_stream(
    outcome: crate::bootstrap::startup_windows::StartupOutcome,
) -> Result<()> {
    tokio::task::spawn_blocking(move || drop(outcome))
        .await
        .map_err(|error| {
            RmuxError::transport(
                "release Windows startup probe stream",
                io::Error::other(error.to_string()),
            )
        })
}

#[cfg(not(any(unix, windows)))]
async fn connect_or_start_transport_for_platform(
    endpoint: &RmuxEndpoint,
    default_timeout: Option<Duration>,
) -> Result<TransportClient> {
    connect_transport(endpoint, startup_operation_timeout(default_timeout)).await
}

pub(super) fn startup_operation_timeout(default_timeout: Option<Duration>) -> Option<Duration> {
    discovery::resolve_timeout(None, default_timeout)
}

fn spawn_hidden_daemon(endpoint: &OsStr) -> io::Result<()> {
    match spawn_hidden_daemon_with_breakaway(endpoint, true) {
        Ok(()) => Ok(()),
        Err(error) if rmux_os::daemon::should_retry_hidden_daemon_without_breakaway(&error) => {
            spawn_hidden_daemon_with_breakaway(endpoint, false)
        }
        Err(error) => Err(error),
    }
}

fn spawn_hidden_daemon_with_breakaway(
    endpoint: &OsStr,
    allow_job_breakaway: bool,
) -> io::Result<()> {
    let mut command = Command::new(daemon_binary());
    command
        .arg(INTERNAL_DAEMON_FLAG)
        .arg(endpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    rmux_os::daemon::configure_hidden_daemon_command(&mut command, allow_job_breakaway);
    let child = rmux_os::daemon::spawn_hidden_daemon_command(&mut command)?;
    drop(child);
    Ok(())
}

fn daemon_binary() -> std::ffi::OsString {
    std::env::var_os(discovery::SDK_DAEMON_BINARY_ENV).unwrap_or_else(|| "rmux".into())
}

fn startup_error(error: impl fmt::Display) -> RmuxError {
    RmuxError::transport(
        "connect or start rmux daemon",
        io::Error::other(error.to_string()),
    )
}

#[cfg(windows)]
pub(super) async fn connect_transport(
    endpoint: &RmuxEndpoint,
    timeout: Option<Duration>,
) -> Result<TransportClient> {
    match endpoint {
        RmuxEndpoint::WindowsPipe(pipe) => {
            let stream = connect_windows_pipe(pipe, timeout).await?;
            Ok(TransportClient::spawn(stream))
        }
        RmuxEndpoint::UnixSocket(_) => Err(RmuxError::unsupported(
            FEATURE_TRANSPORT_UNIX_SOCKET,
            "use a Windows named-pipe endpoint on Windows SDK builds",
        )),
        RmuxEndpoint::Default => Err(RmuxError::transport(
            "resolve rmux SDK endpoint",
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "default endpoint was not resolved before connecting",
            ),
        )),
    }
}

#[cfg(windows)]
async fn connect_windows_pipe(pipe: &str, timeout: Option<Duration>) -> Result<NamedPipeClient> {
    let deadline = StartupDeadline::from_timeout(timeout);
    loop {
        match ClientOptions::new().open(std::path::Path::new(pipe)) {
            Ok(stream) => return Ok(stream),
            Err(error) if windows_pipe_connect_retryable(&error) => {
                if deadline.is_elapsed() {
                    return Err(RmuxError::transport(
                        "connect to rmux daemon",
                        timeout_error(
                            "connect to rmux daemon",
                            deadline.requested_timeout().unwrap_or(Duration::MAX),
                        ),
                    ));
                }
                tokio::time::sleep(deadline.sleep_for(WINDOWS_CONNECT_RETRY_INTERVAL)).await;
            }
            Err(error) => return Err(RmuxError::transport("connect to rmux daemon", error)),
        }
    }
}

#[cfg(windows)]
pub(super) fn windows_pipe_connect_retryable(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code)
            if code == ERROR_PIPE_BUSY as i32
                || code == ERROR_PIPE_NOT_CONNECTED as i32
                || code == ERROR_NO_DATA as i32
                || code == ERROR_FILE_NOT_FOUND as i32
    )
}

#[cfg(not(any(unix, windows)))]
pub(super) async fn connect_transport(
    _endpoint: &RmuxEndpoint,
    _timeout: Option<Duration>,
) -> Result<TransportClient> {
    Err(RmuxError::unsupported(
        "transport.local_ipc",
        "this target does not support rmux local IPC transports",
    ))
}

#[cfg(unix)]
async fn timeout_io<F, T>(
    operation: &'static str,
    timeout: Option<Duration>,
    future: F,
) -> Result<T>
where
    F: std::future::Future<Output = io::Result<T>>,
{
    match timeout {
        Some(timeout) => tokio::time::timeout(timeout, future)
            .await
            .map_err(|_| RmuxError::transport(operation, timeout_error(operation, timeout)))?
            .map_err(|error| RmuxError::transport(operation, error)),
        None => future
            .await
            .map_err(|error| RmuxError::transport(operation, error)),
    }
}

#[cfg(any(unix, windows))]
fn timeout_error(operation: &str, timeout: Duration) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "timed out after {}s while {operation}",
            timeout.as_secs_f32()
        ),
    )
}
