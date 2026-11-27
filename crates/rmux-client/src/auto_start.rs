//! Hidden daemon auto-start support for tmux `CMD_STARTSERVER` commands.

use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use rmux_bootstrap::startup_unix::{
    StartupOutcome, DEFAULT_STARTUP_DEADLINE, STARTUP_POLL_INTERVAL,
};
#[cfg(windows)]
use rmux_bootstrap::startup_windows::{
    connect_or_start_blocking_selected_with, StartupOutcome, DEFAULT_STARTUP_DEADLINE,
    STARTUP_POLL_INTERVAL,
};

#[cfg(unix)]
use crate::ClientError;
use crate::Connection;

mod error;
mod launch;
#[cfg(any(all(test, unix), not(any(unix, windows))))]
mod polling;
mod readiness;
#[cfg(unix)]
mod startup_owner;
mod upgrade_restart;

#[cfg(any(unix, windows))]
use error::auto_start_error_from_startup;
#[cfg(all(test, unix))]
use error::incompatible_daemon_kill_server_command;
pub use error::AutoStartError;
#[cfg(all(test, unix, not(target_os = "linux")))]
use launch::hidden_daemon_command;
#[cfg(any(unix, test))]
use launch::hidden_daemon_command_base;
#[cfg(all(test, target_os = "linux"))]
use launch::hidden_daemon_command_preserving_fd;
#[cfg(not(any(unix, windows)))]
use launch::launch_hidden_daemon;
#[cfg(any(unix, windows))]
use launch::rmux_binary_path;
#[cfg(unix)]
use launch::spawn_hidden_daemon;
#[cfg(not(unix))]
use launch::spawn_hidden_daemon_for;
#[cfg(all(test, unix))]
use launch::{
    hidden_daemon_binary_path, hidden_daemon_binary_path_for_config,
    hidden_daemon_binary_path_for_executable_paths,
};
#[cfg(not(any(unix, windows)))]
use polling::ensure_server_running_polling;
#[cfg(all(test, unix))]
use polling::ensure_server_running_with_probe;
#[cfg(windows)]
use readiness::probe_connected_server_windows;
#[cfg(not(any(unix, windows)))]
use readiness::probe_server_readiness;
#[cfg(test)]
use readiness::startup_readiness_poll_sleep;
use readiness::{is_transient_connect_error, probe_connected_server};

/// The undocumented CLI flag that switches `rmux` into hidden daemon mode.
///
/// This constant is shared with `src/main.rs` so both sides of the re-exec
/// protocol stay in sync.
pub const INTERNAL_DAEMON_FLAG: &str = "--__internal-daemon";

/// Config loading policy to pass to a newly auto-started hidden daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoStartConfig {
    selection: AutoStartConfigSelection,
    quiet: bool,
    cwd: Option<PathBuf>,
    web_frontend: Option<String>,
    web_port: Option<u16>,
    web_required: bool,
    binary_override: Option<PathBuf>,
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
            web_required: false,
            binary_override: None,
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
            web_required: false,
            binary_override: None,
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
            web_required: false,
            binary_override: None,
        }
    }

    /// Overrides the web-share listener port for a newly auto-started daemon.
    #[must_use]
    pub const fn with_web_port(mut self, port: u16) -> Self {
        self.web_port = Some(port);
        self.web_required = true;
        self
    }

    /// Overrides the frontend origin used by newly auto-started web shares.
    #[must_use]
    pub fn with_web_frontend(mut self, frontend: String) -> Self {
        self.web_frontend = Some(frontend);
        self.web_required = true;
        self
    }

    /// Requires a daemon compiled with web-share support for this autostart.
    #[must_use]
    pub const fn with_web_required(mut self) -> Self {
        self.web_required = true;
        self
    }

    /// Uses an explicit binary when this client must auto-start a hidden daemon.
    #[must_use]
    pub fn with_binary_override(mut self, binary_path: PathBuf) -> Self {
        self.binary_override = Some(binary_path);
        self
    }

    #[cfg(not(windows))]
    #[cfg(not(any(unix, windows)))]
    fn loads_startup_config(&self) -> bool {
        !matches!(self.selection, AutoStartConfigSelection::Disabled)
    }

    fn append_hidden_daemon_args(&self, command: &mut Command) {
        use rmux_bootstrap::hidden_daemon::{append_config_args, ConfigSelection};

        let selection = match &self.selection {
            AutoStartConfigSelection::Disabled => ConfigSelection::Disabled,
            AutoStartConfigSelection::Default => ConfigSelection::Default,
            AutoStartConfigSelection::Files(files) => ConfigSelection::Files(files.as_slice()),
        };
        append_config_args(
            command,
            selection,
            self.quiet,
            self.cwd.as_deref(),
            self.web_port,
            self.web_frontend.as_deref(),
        );
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

/// Identifies whether the current command started the daemon it connected to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerConnectionProvenance {
    /// This command won the serialized startup race and launched the daemon.
    StartedByCaller,
    /// The daemon already existed, including when another caller won the race.
    JoinedExisting,
}

/// A ready server connection together with its startup provenance.
pub struct EnsuredServerConnection {
    connection: Connection,
    provenance: ServerConnectionProvenance,
    socket_path: PathBuf,
}

impl EnsuredServerConnection {
    fn new(
        connection: Connection,
        provenance: ServerConnectionProvenance,
        socket_path: PathBuf,
    ) -> Self {
        Self {
            connection,
            provenance,
            socket_path,
        }
    }

    /// Returns how this command obtained the connected daemon.
    #[must_use]
    pub const fn provenance(&self) -> ServerConnectionProvenance {
        self.provenance
    }

    /// Returns the exact endpoint serving this connection.
    ///
    /// On Windows this may differ from the originally resolved path when a
    /// managed endpoint generation rotates during startup.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Consumes the outcome and returns the ready connection.
    #[must_use]
    pub fn into_connection(self) -> Connection {
        self.connection
    }

    /// Consumes the outcome and preserves both the connection and endpoint.
    #[must_use]
    pub fn into_connection_and_socket_path(self) -> (Connection, PathBuf) {
        (self.connection, self.socket_path)
    }
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
pub fn ensure_server_running_with_config(
    socket_path: &Path,
    config: AutoStartConfig,
) -> Result<Connection, AutoStartError> {
    ensure_server_running_with_config_outcome(socket_path, config)
        .map(EnsuredServerConnection::into_connection)
}

/// Ensures the server is reachable and preserves who started the daemon.
///
/// Callers that conditionally clean up an empty daemon must use this function
/// instead of inferring ownership from a failed preflight connection. The
/// startup serializers can report [`ServerConnectionProvenance::JoinedExisting`]
/// when another process wins the race.
#[cfg(windows)]
pub fn ensure_server_running_with_config_outcome(
    socket_path: &Path,
    config: AutoStartConfig,
) -> Result<EnsuredServerConnection, AutoStartError> {
    ensure_server_running_windows(socket_path, config)
}

/// Ensures the server is reachable and preserves who started the daemon.
///
/// Callers that conditionally clean up an empty daemon must use this function
/// instead of inferring ownership from a failed preflight connection. The
/// startup serializers can report [`ServerConnectionProvenance::JoinedExisting`]
/// when another process wins the race.
#[cfg(unix)]
pub fn ensure_server_running_with_config_outcome(
    socket_path: &Path,
    config: AutoStartConfig,
) -> Result<EnsuredServerConnection, AutoStartError> {
    ensure_server_running_unix(socket_path, config)
}

/// Ensures the server is reachable and preserves who started the daemon.
#[cfg(not(any(unix, windows)))]
pub fn ensure_server_running_with_config_outcome(
    socket_path: &Path,
    config: AutoStartConfig,
) -> Result<EnsuredServerConnection, AutoStartError> {
    ensure_server_running_polling(socket_path, config)
}

#[cfg(unix)]
fn ensure_server_running_unix(
    socket_path: &Path,
    config: AutoStartConfig,
) -> Result<EnsuredServerConnection, AutoStartError> {
    let binary_path = rmux_binary_path(&config).map_err(AutoStartError::BinaryPath)?;
    let (provenance, connection, startup_owner) = startup_owner::connect_ready(
        || {
            startup_owner::connect_or_start(
                socket_path,
                &binary_path,
                |binary| rmux_os::build_identity::resolve(binary.as_os_str()),
                |verified| startup_owner::launch_verified(verified, socket_path, &config),
                DEFAULT_STARTUP_DEADLINE,
            )
        },
        |connection| probe_connected_server(connection, &config, socket_path),
        DEFAULT_STARTUP_DEADLINE,
    )?;

    let connected = upgrade_restart::ensure_daemon_fresh_or_restart(
        connection,
        socket_path,
        &binary_path,
        &config,
    )?;
    if let Some(owner) = startup_owner {
        owner.release();
    }
    Ok(EnsuredServerConnection::new(
        connected.connection,
        provenance,
        connected.socket_path,
    ))
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
) -> Result<EnsuredServerConnection, AutoStartError> {
    let binary_path = rmux_binary_path(&config).map_err(AutoStartError::BinaryPath)?;
    let launcher_binary_path = binary_path.clone();
    let launcher_config = config.clone();

    let (outcome, selected_endpoint) = connect_or_start_blocking_selected_with(
        socket_path,
        move |reserved_socket_path| {
            spawn_hidden_daemon_for(
                &launcher_binary_path,
                reserved_socket_path,
                &launcher_config,
            )
        },
        DEFAULT_STARTUP_DEADLINE,
        STARTUP_POLL_INTERVAL,
    )
    .map_err(|error| auto_start_error_from_startup(error, &binary_path, socket_path))?;
    let selected_socket_path = selected_endpoint.into_path();
    let provenance = startup_outcome_provenance(&outcome);
    let connection = startup_outcome_into_connection(outcome)?;
    let (connection, readiness_status) =
        probe_connected_server_windows(connection, &config, &selected_socket_path)?;
    let connected = upgrade_restart::ensure_daemon_fresh_or_restart_after_windows_readiness(
        connection,
        &selected_socket_path,
        &binary_path,
        &config,
        readiness_status,
    )?;
    Ok(EnsuredServerConnection::new(
        connected.connection,
        provenance,
        connected.socket_path,
    ))
}

#[cfg(any(unix, windows))]
fn startup_outcome_provenance(outcome: &StartupOutcome) -> ServerConnectionProvenance {
    if outcome.is_owner() {
        ServerConnectionProvenance::StartedByCaller
    } else {
        ServerConnectionProvenance::JoinedExisting
    }
}

#[cfg(windows)]
fn startup_outcome_into_connection(outcome: StartupOutcome) -> Result<Connection, AutoStartError> {
    Connection::new(outcome.into_stream()).map_err(AutoStartError::Client)
}

#[cfg(all(test, unix))]
#[path = "auto_start/tests.rs"]
mod tests;

#[cfg(all(test, windows))]
mod windows_tests {
    use std::time::Duration;

    use super::{startup_readiness_poll_sleep, STARTUP_POLL_INTERVAL};

    #[test]
    fn windows_startup_readiness_poll_uses_short_backoff() {
        let mut attempt = 0;
        let remaining = Duration::from_secs(1);

        let sleeps = (0..8)
            .map(|_| startup_readiness_poll_sleep(&mut attempt, remaining))
            .collect::<Vec<_>>();

        assert_eq!(
            sleeps,
            [
                Duration::from_millis(1),
                Duration::from_millis(2),
                Duration::from_millis(4),
                Duration::from_millis(8),
                Duration::from_millis(16),
                Duration::from_millis(32),
                STARTUP_POLL_INTERVAL,
                STARTUP_POLL_INTERVAL,
            ]
        );
    }

    #[test]
    fn windows_startup_readiness_poll_respects_remaining_deadline() {
        let mut attempt = 6;

        assert_eq!(
            startup_readiness_poll_sleep(&mut attempt, Duration::from_millis(7)),
            Duration::from_millis(7)
        );
    }
}

#[cfg(test)]
mod argv_tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn hidden_daemon_config_argv_preserves_selection_and_order() {
        #[cfg(unix)]
        let raw = {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![b'x', 255])
        };
        #[cfg(windows)]
        let raw = {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[120, 0xd800])
        };
        for (config, selection) in [
            (AutoStartConfig::disabled(), vec![]),
            (
                AutoStartConfig::default_files(true, None),
                vec![OsString::from("--config-default")],
            ),
            (AutoStartConfig::custom_files(vec![], true, None), vec![]),
            (
                AutoStartConfig::custom_files(
                    vec![PathBuf::from(&raw), PathBuf::from("two")],
                    true,
                    None,
                ),
                vec![
                    "--config-file".into(),
                    raw.clone(),
                    "--config-file".into(),
                    "two".into(),
                ],
            ),
        ] {
            let mut config = config
                .with_web_port(1)
                .with_web_frontend("frontend".to_owned());
            config.cwd = Some(PathBuf::from(&raw));
            let command = hidden_daemon_command_base(Path::new("unused"), Path::new(&raw), &config);
            let mut expected = vec![OsString::from("--__internal-daemon"), raw.clone()];
            expected.extend(selection);
            expected.extend([
                "--config-quiet".into(),
                "--config-cwd".into(),
                raw.clone(),
                "--web-port".into(),
                "1".into(),
                "--frontend-url".into(),
                "frontend".into(),
            ]);
            assert_eq!(command.get_args().collect::<Vec<_>>(), expected);
        }
    }
}
