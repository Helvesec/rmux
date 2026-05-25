//! Hidden daemon auto-start support for tmux `CMD_STARTSERVER` commands.

use std::env;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
#[cfg(any(all(test, unix), not(any(unix, windows))))]
use std::time::Instant;

#[cfg(not(windows))]
use rmux_proto::{ListSessionsRequest, Response};
#[cfg(unix)]
use rmux_sdk::bootstrap::startup_unix::{
    connect_or_start_with, StartupError, StartupOutcome, DEFAULT_STARTUP_DEADLINE,
    STARTUP_POLL_INTERVAL,
};
#[cfg(windows)]
use rmux_sdk::bootstrap::startup_windows::{
    connect_or_start_with, StartupError, StartupOutcome, DEFAULT_STARTUP_DEADLINE,
    STARTUP_POLL_INTERVAL,
};

use crate::shell_quote::shell_quote_path;
#[cfg(any(all(test, unix), not(any(unix, windows))))]
use crate::ConnectResult;
use crate::{ClientError, Connection};

mod upgrade_restart;

#[cfg(not(any(unix, windows)))]
const AUTO_START_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(any(unix, windows)))]
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The undocumented CLI flag that switches `rmux` into hidden daemon mode.
///
/// This constant is shared with `src/main.rs` so both sides of the re-exec
/// protocol stay in sync.
pub const INTERNAL_DAEMON_FLAG: &str = "--__internal-daemon";

const BINARY_OVERRIDE_ENV: &str = "RMUX_INTERNAL_BINARY_PATH";
const BINARY_OVERRIDE_TEST_OPT_IN_ENV: &str = "RMUX_ALLOW_INTERNAL_BINARY_OVERRIDE";

/// Config loading policy to pass to a newly auto-started hidden daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoStartConfig {
    selection: AutoStartConfigSelection,
    quiet: bool,
    cwd: Option<PathBuf>,
    web_frontend: Option<String>,
    web_port: Option<u16>,
}

impl AutoStartConfig {
    /// Builds a policy that leaves startup config loading disabled.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            selection: AutoStartConfigSelection::Disabled,
            quiet: true,
            cwd: None,
            web_frontend: None,
            web_port: None,
        }
    }

    /// Builds a policy that loads RMUX's default startup config search path.
    #[must_use]
    pub fn default_files(quiet: bool, cwd: Option<PathBuf>) -> Self {
        Self {
            selection: AutoStartConfigSelection::Default,
            quiet,
            cwd,
            web_frontend: None,
            web_port: None,
        }
    }

    /// Builds a policy that loads the explicit top-level `-f` files.
    #[must_use]
    pub fn custom_files(files: Vec<PathBuf>, quiet: bool, cwd: Option<PathBuf>) -> Self {
        Self {
            selection: AutoStartConfigSelection::Files(files),
            quiet,
            cwd,
            web_frontend: None,
            web_port: None,
        }
    }

    /// Overrides the web-share listener port for a newly auto-started daemon.
    #[must_use]
    pub const fn with_web_port(mut self, port: u16) -> Self {
        self.web_port = Some(port);
        self
    }

    /// Overrides the frontend origin used by newly auto-started web shares.
    #[must_use]
    pub fn with_web_frontend(mut self, frontend: String) -> Self {
        self.web_frontend = Some(frontend);
        self
    }

    #[cfg(not(windows))]
    fn loads_startup_config(&self) -> bool {
        !matches!(self.selection, AutoStartConfigSelection::Disabled)
    }

    fn append_hidden_daemon_args(&self, command: &mut Command) {
        match &self.selection {
            AutoStartConfigSelection::Disabled => {}
            AutoStartConfigSelection::Default => {
                command.arg("--config-default");
            }
            AutoStartConfigSelection::Files(files) => {
                for file in files {
                    command.arg("--config-file").arg(file);
                }
            }
        }

        if self.quiet {
            command.arg("--config-quiet");
        }
        if let Some(cwd) = &self.cwd {
            command.arg("--config-cwd").arg(cwd);
        }
        if let Some(port) = self.web_port {
            command.arg("--web-port").arg(port.to_string());
        }
        if let Some(frontend) = &self.web_frontend {
            command.arg("--frontend-url").arg(frontend);
        }
    }
}

/// Config file selection mode for a newly auto-started hidden daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoStartConfigSelection {
    /// Do not load startup config files.
    Disabled,
    /// Load RMUX's default config search path.
    Default,
    /// Load these explicit config files in order.
    Files(Vec<PathBuf>),
}

/// Ensures the RMUX server is reachable, auto-starting it when absent.
///
/// This boundary is reserved for command paths that match tmux's
/// `CMD_STARTSERVER` startup inventory. Other command paths must keep using
/// [`crate::connect`] or [`crate::connect_or_absent`] directly so they do not
/// spawn a daemon as a side effect.
pub fn ensure_server_running(socket_path: &Path) -> Result<Connection, AutoStartError> {
    ensure_server_running_with_config(socket_path, AutoStartConfig::disabled())
}

/// Ensures the server is reachable, passing config load options if launched.
#[cfg(unix)]
pub fn ensure_server_running_with_config(
    socket_path: &Path,
    config: AutoStartConfig,
) -> Result<Connection, AutoStartError> {
    ensure_server_running_unix(socket_path, config)
}

/// Ensures the server is reachable, passing config load options if launched.
#[cfg(windows)]
pub fn ensure_server_running_with_config(
    socket_path: &Path,
    config: AutoStartConfig,
) -> Result<Connection, AutoStartError> {
    ensure_server_running_windows(socket_path, config)
}

/// Ensures the server is reachable, passing config load options if launched.
#[cfg(not(any(unix, windows)))]
pub fn ensure_server_running_with_config(
    socket_path: &Path,
    config: AutoStartConfig,
) -> Result<Connection, AutoStartError> {
    ensure_server_running_polling(socket_path, config)
}

#[cfg(unix)]
fn ensure_server_running_unix(
    socket_path: &Path,
    config: AutoStartConfig,
) -> Result<Connection, AutoStartError> {
    let binary_path = rmux_binary_path().map_err(AutoStartError::BinaryPath)?;
    let launcher_binary_path = binary_path.clone();
    let launcher_socket_path = socket_path.to_path_buf();
    let launcher_config = config.clone();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| AutoStartError::Client(ClientError::Io(error)))?;
    let outcome = runtime.block_on(connect_or_start_with(
        socket_path,
        move || async move {
            spawn_hidden_daemon_for(
                &launcher_binary_path,
                &launcher_socket_path,
                &launcher_config,
            )
        },
        DEFAULT_STARTUP_DEADLINE,
        STARTUP_POLL_INTERVAL,
    ));

    let connection = startup_outcome_into_connection(
        outcome.map_err(|error| auto_start_error_from_startup(error, &binary_path, socket_path))?,
    )?;

    let connection = probe_connected_server(connection, &config)?;
    upgrade_restart::ensure_daemon_fresh_or_restart(connection, socket_path, &binary_path, &config)
}

#[cfg(unix)]
fn startup_outcome_into_connection(outcome: StartupOutcome) -> Result<Connection, AutoStartError> {
    let stream = outcome
        .into_stream()
        .into_std()
        .map_err(|error| AutoStartError::Client(ClientError::Io(error)))?;
    stream
        .set_nonblocking(false)
        .map_err(|error| AutoStartError::Client(ClientError::Io(error)))?;
    Connection::new(stream).map_err(AutoStartError::Client)
}

#[cfg(windows)]
fn ensure_server_running_windows(
    socket_path: &Path,
    config: AutoStartConfig,
) -> Result<Connection, AutoStartError> {
    let binary_path = rmux_binary_path().map_err(AutoStartError::BinaryPath)?;
    let launcher_binary_path = binary_path.clone();
    let launcher_socket_path = socket_path.to_path_buf();
    let launcher_config = config.clone();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| AutoStartError::Client(ClientError::Io(error)))?;
    let outcome = runtime.block_on(connect_or_start_with(
        socket_path,
        move || async move {
            spawn_hidden_daemon_for(
                &launcher_binary_path,
                &launcher_socket_path,
                &launcher_config,
            )
        },
        DEFAULT_STARTUP_DEADLINE,
        STARTUP_POLL_INTERVAL,
    ));

    let connection = startup_outcome_into_connection(
        outcome.map_err(|error| auto_start_error_from_startup(error, &binary_path, socket_path))?,
    )?;
    upgrade_restart::ensure_daemon_fresh_or_restart(connection, socket_path, &binary_path, &config)
}

#[cfg(windows)]
fn startup_outcome_into_connection(outcome: StartupOutcome) -> Result<Connection, AutoStartError> {
    Connection::new(outcome.into_stream()).map_err(AutoStartError::Client)
}

#[cfg(not(windows))]
fn probe_connected_server(
    mut connection: Connection,
    config: &AutoStartConfig,
) -> Result<Connection, AutoStartError> {
    if !config.loads_startup_config() {
        probe_server_readiness(&mut connection).map_err(AutoStartError::Client)?;
    }
    Ok(connection)
}

#[cfg(windows)]
fn probe_connected_server(
    connection: Connection,
    _config: &AutoStartConfig,
) -> Result<Connection, AutoStartError> {
    Ok(connection)
}

#[cfg(unix)]
fn auto_start_error_from_startup(
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
fn auto_start_error_from_startup(
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

#[cfg(not(any(unix, windows)))]
fn ensure_server_running_polling(
    socket_path: &Path,
    config: AutoStartConfig,
) -> Result<Connection, AutoStartError> {
    if config.loads_startup_config() {
        return ensure_server_running_with_probe(
            socket_path,
            AUTO_START_TIMEOUT,
            POLL_INTERVAL,
            || crate::connect_or_absent(socket_path),
            || launch_hidden_daemon(socket_path, &config),
            |_| Ok(()),
        );
    }

    ensure_server_running_with(
        socket_path,
        AUTO_START_TIMEOUT,
        POLL_INTERVAL,
        || crate::connect_or_absent(socket_path),
        || launch_hidden_daemon(socket_path, &config),
    )
}

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
                "{message} on '{}'; detach existing clients, then run `rmux -S {} kill-server` before retrying",
                socket_path.display(),
                shell_quote_path(socket_path)
            ),
            Self::TimedOut {
                socket_path,
                waited,
            } => write!(
                formatter,
                "timed out after {}s waiting for rmux server socket '{}'",
                waited.as_secs(),
                socket_path.display()
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

#[cfg(not(any(unix, windows)))]
fn ensure_server_running_with<ConnectFn, LaunchFn>(
    socket_path: &Path,
    timeout: Duration,
    poll_interval: Duration,
    connect: ConnectFn,
    launch: LaunchFn,
) -> Result<Connection, AutoStartError>
where
    ConnectFn: FnMut() -> Result<ConnectResult, ClientError>,
    LaunchFn: FnMut() -> Result<(), AutoStartError>,
{
    ensure_server_running_with_probe(
        socket_path,
        timeout,
        poll_interval,
        connect,
        launch,
        probe_server_readiness,
    )
}

#[cfg(any(all(test, unix), not(any(unix, windows))))]
fn ensure_server_running_with_probe<ConnectFn, LaunchFn, ProbeFn>(
    socket_path: &Path,
    timeout: Duration,
    poll_interval: Duration,
    mut connect: ConnectFn,
    mut launch: LaunchFn,
    mut probe: ProbeFn,
) -> Result<Connection, AutoStartError>
where
    ConnectFn: FnMut() -> Result<ConnectResult, ClientError>,
    LaunchFn: FnMut() -> Result<(), AutoStartError>,
    ProbeFn: FnMut(&mut Connection) -> Result<(), ClientError>,
{
    match connect().map_err(AutoStartError::Client)? {
        ConnectResult::Connected(mut connection) => {
            probe(&mut connection).map_err(AutoStartError::Client)?;
            return Ok(connection);
        }
        ConnectResult::Absent => {}
    }

    launch()?;
    wait_for_server(
        socket_path,
        timeout,
        poll_interval,
        &mut connect,
        &mut probe,
    )
}

#[cfg(any(all(test, unix), not(any(unix, windows))))]
fn wait_for_server<ConnectFn, ProbeFn>(
    socket_path: &Path,
    timeout: Duration,
    poll_interval: Duration,
    connect: &mut ConnectFn,
    probe: &mut ProbeFn,
) -> Result<Connection, AutoStartError>
where
    ConnectFn: FnMut() -> Result<ConnectResult, ClientError>,
    ProbeFn: FnMut(&mut Connection) -> Result<(), ClientError>,
{
    let start = Instant::now();
    let deadline = start + timeout;

    loop {
        match connect() {
            Ok(crate::ConnectResult::Connected(mut connection)) => match probe(&mut connection) {
                Ok(()) => return Ok(connection),
                Err(error) if is_transient_connect_error(&error) => {}
                Err(error) => return Err(AutoStartError::Client(error)),
            },
            Ok(crate::ConnectResult::Absent) => {}
            Err(error) if is_transient_connect_error(&error) => {}
            Err(error) => return Err(AutoStartError::Client(error)),
        }

        let now = Instant::now();
        if now >= deadline {
            return Err(AutoStartError::TimedOut {
                socket_path: socket_path.to_path_buf(),
                waited: timeout,
            });
        }

        std::thread::sleep(poll_interval.min(deadline.saturating_duration_since(now)));
    }
}

fn is_transient_connect_error(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Io(io_error)
            if matches!(
                io_error.kind(),
                io::ErrorKind::WouldBlock
                    | io::ErrorKind::Interrupted
                    | io::ErrorKind::TimedOut
            )
    )
}

#[cfg(not(windows))]
fn probe_server_readiness(connection: &mut Connection) -> Result<(), ClientError> {
    let response = connection.list_sessions(ListSessionsRequest {
        format: None,
        filter: None,
        sort_order: None,
        reversed: false,
    })?;
    match response {
        Response::ListSessions(_) => Ok(()),
        other => Err(ClientError::Protocol(rmux_proto::RmuxError::Server(
            format!("unexpected readiness response: {other:?}"),
        ))),
    }
}

#[cfg(not(any(unix, windows)))]
fn launch_hidden_daemon(
    socket_path: &Path,
    config: &AutoStartConfig,
) -> Result<(), AutoStartError> {
    let binary_path = rmux_binary_path().map_err(AutoStartError::BinaryPath)?;
    spawn_hidden_daemon_for(&binary_path, socket_path, config).map_err(|error| {
        AutoStartError::Launch {
            path: binary_path,
            error,
        }
    })
}

fn spawn_hidden_daemon_for(
    binary_path: &Path,
    socket_path: &Path,
    config: &AutoStartConfig,
) -> io::Result<()> {
    let command = hidden_daemon_command(binary_path, socket_path, config, true);
    match spawn_hidden_daemon(command) {
        Ok(()) => Ok(()),
        Err(error) if rmux_os::daemon::should_retry_hidden_daemon_without_breakaway(&error) => {
            let command = hidden_daemon_command(binary_path, socket_path, config, false);
            spawn_hidden_daemon(command)
        }
        Err(error) => Err(error),
    }
}

fn hidden_daemon_command(
    binary_path: &Path,
    socket_path: &Path,
    config: &AutoStartConfig,
    allow_job_breakaway: bool,
) -> Command {
    let mut command = Command::new(binary_path);
    command
        .arg(INTERNAL_DAEMON_FLAG)
        .arg(socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    config.append_hidden_daemon_args(&mut command);
    rmux_os::daemon::configure_hidden_daemon_command(&mut command, allow_job_breakaway);
    command
}

fn spawn_hidden_daemon(mut command: Command) -> io::Result<()> {
    let child = rmux_os::daemon::spawn_hidden_daemon_command(&mut command)?;
    // Intentionally drop without `wait()`: the daemon must outlive the
    // short-lived client process that launched it.
    drop(child);
    Ok(())
}

fn rmux_binary_path() -> io::Result<PathBuf> {
    let current_exe = env::current_exe()?;
    match env::var_os(BINARY_OVERRIDE_ENV).filter(|_| binary_override_enabled_for_tests()) {
        Some(path) => Ok(PathBuf::from(path)),
        None => Ok(current_exe),
    }
}

fn binary_override_enabled_for_tests() -> bool {
    cfg!(debug_assertions)
        && env::var_os(BINARY_OVERRIDE_TEST_OPT_IN_ENV).is_some_and(|value| value == "1")
}

#[cfg(all(test, unix))]
#[path = "auto_start/tests.rs"]
mod tests;
