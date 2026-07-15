use std::path::Path;

use rmux_client::AutoStartConfig;
use rmux_server::{DaemonConfig, ServerDaemon};

use crate::cli_args::{Cli, Command, ConfigFileSelection, StartServerArgs, TopLevelCommandScan};
use crate::server_runtime::build_daemon_runtime;

use super::ExitFailure;

#[derive(Debug, Clone)]
pub(in crate::cli) struct StartupOptions {
    pub(in crate::cli) no_start_server: bool,
    pub(in crate::cli) config: AutoStartConfig,
}

impl StartupOptions {
    pub(in crate::cli) fn new(no_start_server: bool, config: AutoStartConfig) -> Self {
        Self {
            no_start_server,
            config,
        }
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
