use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
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
#[cfg(windows)]
const WINDOWS_STARTUP_READY_EVENT_TIMEOUT: Duration = Duration::from_secs(2);

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
    let candidates = hidden_daemon_binary_candidates();
    let mut last_error = None;
    for (index, binary) in candidates.iter().enumerate() {
        let result = match spawn_hidden_daemon_with_binary(endpoint, binary, true) {
            Ok(()) => return Ok(()),
            Err(error) if rmux_os::daemon::should_retry_hidden_daemon_without_breakaway(&error) => {
                spawn_hidden_daemon_with_binary(endpoint, binary, false)
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error)
                if error.kind() == io::ErrorKind::NotFound && index + 1 < candidates.len() =>
            {
                last_error = Some(error);
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                last_error = Some(error);
                break;
            }
            Err(error) => return Err(error),
        }
    }

    Err(hidden_daemon_not_found_error(&candidates, last_error))
}

fn spawn_hidden_daemon_with_binary(
    endpoint: &OsStr,
    binary: &OsStr,
    allow_job_breakaway: bool,
) -> io::Result<()> {
    #[cfg(windows)]
    let ready = rmux_os::daemon::StartupReadyEvent::new()?;
    let mut command = Command::new(binary);
    command
        .arg(INTERNAL_DAEMON_FLAG)
        .arg(endpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.arg("--startup-ready-event").arg(ready.name());
    rmux_os::daemon::configure_hidden_daemon_command(&mut command, allow_job_breakaway);
    let child = rmux_os::daemon::spawn_hidden_daemon_command(&mut command)?;
    drop(child);
    #[cfg(windows)]
    {
        let _ = ready.wait(WINDOWS_STARTUP_READY_EVENT_TIMEOUT);
    }
    Ok(())
}

pub(super) fn daemon_binary() -> std::ffi::OsString {
    std::env::var_os(discovery::SDK_DAEMON_BINARY_ENV).unwrap_or_else(|| "rmux".into())
}

fn hidden_daemon_binary_candidates() -> Vec<OsString> {
    hidden_daemon_binary_candidates_from_sources(
        std::env::var_os(discovery::SDK_DAEMON_BINARY_ENV),
        resolve_executable_on_path("rmux"),
    )
}

#[cfg(test)]
fn hidden_daemon_binary_candidates_from_env(override_binary: Option<OsString>) -> Vec<OsString> {
    hidden_daemon_binary_candidates_from_sources(override_binary, None)
}

fn hidden_daemon_binary_candidates_from_sources(
    override_binary: Option<OsString>,
    public_binary: Option<PathBuf>,
) -> Vec<OsString> {
    if let Some(binary) = override_binary {
        return vec![binary];
    }

    let mut candidates = Vec::new();
    push_unique_candidate(&mut candidates, OsString::from("rmux-daemon"));
    if let Some(public_binary) = public_binary.as_deref() {
        let resolved = std::fs::canonicalize(public_binary).ok();
        push_daemon_sibling_candidates(&mut candidates, public_binary, resolved.as_deref());
    }
    push_unique_candidate(&mut candidates, OsString::from("rmux"));
    candidates
}

fn push_daemon_sibling_candidates(
    candidates: &mut Vec<OsString>,
    public_binary: &Path,
    resolved_binary: Option<&Path>,
) {
    if let Some(path) = daemon_sibling_path(public_binary) {
        push_unique_candidate(candidates, path.into_os_string());
    }
    if let Some(path) = resolved_binary.and_then(daemon_sibling_path) {
        push_unique_candidate(candidates, path.into_os_string());
    }
}

fn daemon_sibling_path(binary: &Path) -> Option<PathBuf> {
    let mut candidate = binary.to_path_buf();
    let daemon_file_name = match binary.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if !extension.is_empty() => format!("rmux-daemon.{extension}"),
        _ => "rmux-daemon".to_owned(),
    };
    candidate.set_file_name(daemon_file_name);
    candidate.is_file().then_some(candidate)
}

fn push_unique_candidate(candidates: &mut Vec<OsString>, candidate: OsString) {
    if !candidates.iter().any(|existing| existing == &candidate) {
        candidates.push(candidate);
    }
}

fn resolve_executable_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        for file_name in executable_file_names(name) {
            let candidate = directory.join(file_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn executable_file_names(name: &str) -> Vec<OsString> {
    #[cfg(windows)]
    {
        let mut names = vec![OsString::from(name)];
        if Path::new(name).extension().is_none() {
            let pathext =
                std::env::var_os("PATHEXT").unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
            for extension in pathext.to_string_lossy().split(';') {
                if extension.is_empty() {
                    continue;
                }
                let normalized = if extension.starts_with('.') {
                    extension.to_owned()
                } else {
                    format!(".{extension}")
                };
                names.push(OsString::from(format!("{name}{normalized}")));
            }
        }
        names
    }
    #[cfg(not(windows))]
    {
        vec![OsString::from(name)]
    }
}

fn hidden_daemon_not_found_error(
    candidates: &[OsString],
    last_error: Option<io::Error>,
) -> io::Error {
    let candidates = candidates
        .iter()
        .map(|candidate| candidate.to_string_lossy())
        .collect::<Vec<_>>()
        .join(", ");
    let mut message = format!(
        "no rmux hidden daemon binary candidate was available; tried [{candidates}]. \
         Install rmux, ensure rmux-daemon or rmux is on PATH, or set {} to an absolute rmux binary path",
        discovery::SDK_DAEMON_BINARY_ENV
    );
    if let Some(error) = last_error {
        message.push_str(&format!("; last error: {error}"));
    }
    io::Error::new(io::ErrorKind::NotFound, message)
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        hidden_daemon_binary_candidates_from_env, hidden_daemon_binary_candidates_from_sources,
        hidden_daemon_not_found_error,
    };

    fn temp_root(name: &str) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rmux-sdk-daemon-candidate-{name}-{}-{timestamp}",
            std::process::id()
        ))
    }

    #[test]
    fn hidden_daemon_spawn_prefers_minimal_sibling_binary() {
        assert_eq!(
            hidden_daemon_binary_candidates_from_env(None),
            vec![OsString::from("rmux-daemon"), OsString::from("rmux")]
        );
    }

    #[test]
    fn hidden_daemon_spawn_honors_explicit_sdk_binary_override() {
        assert_eq!(
            hidden_daemon_binary_candidates_from_env(Some(OsString::from("/tmp/custom-rmux"))),
            vec![OsString::from("/tmp/custom-rmux")]
        );
    }

    #[test]
    fn hidden_daemon_spawn_uses_public_binary_daemon_sibling_before_rmux_fallback() {
        let root = temp_root("public-sibling");
        std::fs::create_dir_all(&root).expect("create root");
        let public = root.join("rmux");
        let daemon = root.join("rmux-daemon");
        std::fs::write(&public, b"rmux").expect("write public");
        std::fs::write(&daemon, b"daemon").expect("write daemon");

        assert_eq!(
            hidden_daemon_binary_candidates_from_sources(None, Some(public)),
            vec![
                OsString::from("rmux-daemon"),
                daemon.into_os_string(),
                OsString::from("rmux"),
            ]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hidden_daemon_not_found_error_mentions_env_override() {
        let error = hidden_daemon_not_found_error(
            &[OsString::from("rmux-daemon"), OsString::from("rmux")],
            None,
        );
        let message = error.to_string();

        assert!(message.contains("rmux-daemon"));
        assert!(message.contains("RMUX_SDK_DAEMON_BINARY"));
    }
}
