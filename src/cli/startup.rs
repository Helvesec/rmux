use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use rmux_client::{
    AutoStartConfig, Connection, EnsuredServerConnection, ServerConnectionProvenance,
};
use rmux_server::{DaemonConfig, ServerDaemon};

use crate::cli_args::{Cli, Command, ConfigFileSelection, StartServerArgs, TopLevelCommandScan};
use crate::server_runtime::build_daemon_runtime;

use super::ExitFailure;

#[derive(Debug)]
pub(in crate::cli) struct StartServerConnection {
    pub(in crate::cli) connection: Connection,
    pub(in crate::cli) provenance: ServerConnectionProvenance,
}

#[derive(Debug)]
struct StartupEndpointState {
    socket_path: PathBuf,
    connection: Option<Connection>,
    started_by_caller: bool,
}

/// The daemon endpoint of one CLI invocation, shared by every cloned
/// [`StartupOptions`].
///
/// Runtime alias resolution borrows the startup connection first, and the
/// queue's first server-starting command then consumes that same connection.
/// Nothing may keep it alive past that point: a leftover idle connection is
/// counted as concurrent activity by the daemon and permanently cancels a
/// queued exit-empty shutdown.
///
/// Two facts outlive the connection because later commands still need them:
///
/// * whether this invocation started the daemon, so an attach later in the
///   same queue still cleans up the empty daemon it created;
/// * the endpoint actually serving the daemon, which Windows rotates when
///   auto-start replaces a stale managed generation.
#[derive(Debug, Clone)]
pub(in crate::cli) struct StartupEndpoint {
    inner: Rc<RefCell<StartupEndpointState>>,
}

impl StartupEndpoint {
    /// Records a resolved endpoint that this invocation has not connected to.
    pub(in crate::cli) fn resolved(socket_path: PathBuf) -> Self {
        Self::from_state(StartupEndpointState {
            socket_path,
            connection: None,
            started_by_caller: false,
        })
    }

    /// Adopts the connection opened before the typed command queue is parsed.
    pub(in crate::cli) fn prestarted(outcome: EnsuredServerConnection) -> Self {
        let started_by_caller = started_by_caller(outcome.provenance());
        let (connection, socket_path) = outcome.into_connection_and_socket_path();
        Self::from_state(StartupEndpointState {
            socket_path,
            connection: Some(connection),
            started_by_caller,
        })
    }

    fn from_state(state: StartupEndpointState) -> Self {
        Self {
            inner: Rc::new(RefCell::new(state)),
        }
    }

    /// Returns the endpoint currently serving this invocation.
    pub(in crate::cli) fn socket_path(&self) -> PathBuf {
        self.inner.borrow().socket_path.clone()
    }

    /// Reports how the queue obtained the daemon it is talking to.
    pub(in crate::cli) fn provenance(&self) -> ServerConnectionProvenance {
        if self.inner.borrow().started_by_caller {
            ServerConnectionProvenance::StartedByCaller
        } else {
            ServerConnectionProvenance::JoinedExisting
        }
    }

    pub(in crate::cli) fn with_connection_mut<T>(
        &self,
        use_connection: impl FnOnce(&mut Connection) -> T,
    ) -> T {
        let mut inner = self.inner.borrow_mut();
        let connection = inner
            .connection
            .as_mut()
            .expect("startup connection must exist during alias resolution");
        use_connection(connection)
    }

    /// Hands the startup connection to the first command that needs one.
    pub(in crate::cli) fn take_connection(&self) -> Option<Connection> {
        self.inner.borrow_mut().connection.take()
    }

    /// Adopts the endpoint and provenance of a daemon started mid-queue.
    ///
    /// Auto-start may launch the daemon on a different endpoint than the one
    /// resolved at process start, and the remaining commands must follow it.
    pub(in crate::cli) fn record_ensured(
        &self,
        socket_path: &Path,
        provenance: ServerConnectionProvenance,
    ) {
        let mut inner = self.inner.borrow_mut();
        if inner.socket_path != socket_path {
            inner.socket_path = socket_path.to_path_buf();
        }
        inner.started_by_caller |= started_by_caller(provenance);
    }
}

const fn started_by_caller(provenance: ServerConnectionProvenance) -> bool {
    matches!(provenance, ServerConnectionProvenance::StartedByCaller)
}

#[derive(Debug, Clone)]
pub(in crate::cli) struct StartupOptions {
    pub(in crate::cli) no_start_server: bool,
    pub(in crate::cli) config: AutoStartConfig,
    pub(in crate::cli) endpoint: StartupEndpoint,
}

impl StartupOptions {
    pub(in crate::cli) fn new(
        no_start_server: bool,
        config: AutoStartConfig,
        endpoint: StartupEndpoint,
    ) -> Self {
        Self {
            no_start_server,
            config,
            endpoint,
        }
    }

    /// Returns the endpoint the next queued command must use.
    pub(in crate::cli) fn socket_path(&self) -> PathBuf {
        self.endpoint.socket_path()
    }

    pub(in crate::cli) fn for_command(
        &self,
        command_has_start_server_flag: bool,
        command_requires_web: bool,
        start_server_args: Option<&StartServerArgs>,
    ) -> Self {
        let mut config = if command_requires_web {
            self.config.clone().with_web_required()
        } else {
            self.config.clone()
        };
        if let Some(args) = start_server_args {
            config = apply_web_auto_start_config(config, args);
        }
        Self {
            no_start_server: self.no_start_server || !command_has_start_server_flag,
            config,
            endpoint: self.endpoint.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct StartupConfig {
    pub(super) server: ServerStartupConfig,
    pub(super) auto_start: AutoStartConfig,
    pub(super) web_frontend: Option<String>,
    pub(super) web_port: Option<u16>,
}

#[derive(Debug, Clone)]
pub(super) enum ServerStartupConfig {
    Default {
        quiet: bool,
        cwd: Option<std::path::PathBuf>,
    },
    Files {
        files: Vec<std::path::PathBuf>,
        quiet: bool,
        cwd: Option<std::path::PathBuf>,
    },
}

pub(super) fn startup_config_from_cli(cli: &Cli) -> StartupConfig {
    startup_config_from_selection(cli.config_file_selection(), cli.command.as_ref())
}

pub(super) fn startup_config_from_top_level_scan(
    scan: &TopLevelCommandScan,
    first_command: &Command,
) -> StartupConfig {
    let selection = match scan.config_files.as_slice() {
        [] => ConfigFileSelection::Default,
        files => ConfigFileSelection::Custom(files),
    };
    startup_config_from_selection(selection, Some(first_command))
}

fn startup_config_from_selection(
    selection: ConfigFileSelection<'_>,
    command: Option<&Command>,
) -> StartupConfig {
    let cwd = std::env::current_dir().ok();
    let web = start_server_web_args(command);
    let mut config = match selection {
        ConfigFileSelection::Default => {
            let quiet = true;
            StartupConfig {
                server: ServerStartupConfig::Default {
                    quiet,
                    cwd: cwd.clone(),
                },
                auto_start: AutoStartConfig::default_files(quiet, cwd),
                web_frontend: web.web_frontend.clone(),
                web_port: web.web_port,
            }
        }
        ConfigFileSelection::Custom(files) => {
            let quiet = false;
            let files = files.to_vec();
            StartupConfig {
                server: ServerStartupConfig::Files {
                    files: files.clone(),
                    quiet,
                    cwd: cwd.clone(),
                },
                auto_start: AutoStartConfig::custom_files(files, quiet, cwd),
                web_frontend: web.web_frontend.clone(),
                web_port: web.web_port,
            }
        }
    };
    config.auto_start = apply_web_auto_start_config(config.auto_start, &web);
    config
}

fn start_server_web_args(command: Option<&Command>) -> StartServerArgs {
    match command {
        Some(Command::StartServer(args)) => args.clone(),
        _ => StartServerArgs::default(),
    }
}

fn apply_web_auto_start_config(
    mut config: AutoStartConfig,
    args: &StartServerArgs,
) -> AutoStartConfig {
    if let Some(port) = args.web_port {
        config = config.with_web_port(port);
    }
    if let Some(frontend) = &args.web_frontend {
        config = config.with_web_frontend(frontend.clone());
    }
    config
}

fn apply_server_startup_config(
    config: DaemonConfig,
    startup: &ServerStartupConfig,
) -> DaemonConfig {
    match startup {
        ServerStartupConfig::Default { quiet, cwd } => {
            config.with_default_config_load(*quiet, cwd.clone())
        }
        ServerStartupConfig::Files { files, quiet, cwd } => {
            config.with_config_files(files.clone(), *quiet, cwd.clone())
        }
    }
}

fn apply_web_daemon_config(config: DaemonConfig, startup: &StartupConfig) -> DaemonConfig {
    let config = match startup.web_port {
        Some(port) => config.with_web_port(port),
        None => config,
    };
    match &startup.web_frontend {
        Some(frontend) => config.with_web_frontend(frontend.clone()),
        None => config,
    }
}

pub(super) fn run_foreground_server(
    socket_path: &Path,
    startup_config: &StartupConfig,
) -> Result<i32, ExitFailure> {
    let config = apply_web_daemon_config(
        apply_server_startup_config(
            DaemonConfig::new(socket_path.to_path_buf()),
            &startup_config.server,
        ),
        startup_config,
    );
    let runtime = build_daemon_runtime().map_err(|error| ExitFailure::new(1, error.to_string()))?;

    runtime
        .block_on(async move {
            let server = ServerDaemon::new(config).bind().await?;
            server.wait().await
        })
        .map(|()| 0)
        .map_err(|error| ExitFailure::new(1, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{StartupEndpoint, StartupOptions};
    use rmux_client::{AutoStartConfig, ServerConnectionProvenance};
    use std::path::{Path, PathBuf};

    fn startup_options(socket_path: &str) -> StartupOptions {
        StartupOptions::new(
            false,
            AutoStartConfig::disabled(),
            StartupEndpoint::resolved(PathBuf::from(socket_path)),
        )
    }

    #[test]
    fn rotated_startup_endpoint_reaches_later_queued_commands() {
        let startup = startup_options("/endpoint/original");
        let starting_command = startup.for_command(true, false, None);
        let later_command = startup.for_command(false, false, None);

        starting_command.endpoint.record_ensured(
            Path::new("/endpoint/rotated"),
            ServerConnectionProvenance::StartedByCaller,
        );

        assert_eq!(
            later_command.socket_path(),
            PathBuf::from("/endpoint/rotated"),
            "a command queued after an auto-start must follow the rotated endpoint"
        );
        assert_eq!(startup.socket_path(), PathBuf::from("/endpoint/rotated"));
    }

    #[test]
    fn startup_provenance_outlives_the_command_that_consumes_the_connection() {
        let startup = startup_options("/endpoint/original");
        let starting_command = startup.for_command(true, false, None);

        starting_command.endpoint.record_ensured(
            Path::new("/endpoint/original"),
            ServerConnectionProvenance::StartedByCaller,
        );
        assert!(starting_command.endpoint.take_connection().is_none());

        let attach_command = startup.for_command(true, false, None);
        assert_eq!(
            attach_command.endpoint.provenance(),
            ServerConnectionProvenance::StartedByCaller,
            "an attach queued after the startup connection was consumed must \
             still clean up the daemon this invocation started"
        );
    }

    #[test]
    fn joining_an_existing_daemon_keeps_joined_provenance() {
        let startup = startup_options("/endpoint/original");

        startup.endpoint.record_ensured(
            Path::new("/endpoint/original"),
            ServerConnectionProvenance::JoinedExisting,
        );

        assert_eq!(
            startup.endpoint.provenance(),
            ServerConnectionProvenance::JoinedExisting
        );
    }
}
