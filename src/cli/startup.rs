use std::path::Path;

use rmux_client::AutoStartConfig;
use rmux_server::{DaemonConfig, ServerDaemon};
use tokio::runtime::Builder;

use crate::cli_args::{Cli, ConfigFileSelection};

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

    pub(in crate::cli) fn for_command(&self, command_has_start_server_flag: bool) -> Self {
        Self {
            no_start_server: self.no_start_server || !command_has_start_server_flag,
            config: self.config.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct StartupConfig {
    pub(super) server: ServerStartupConfig,
    pub(super) auto_start: AutoStartConfig,
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
    let cwd = std::env::current_dir().ok();
    match cli.config_file_selection() {
        ConfigFileSelection::Default => {
            let quiet = true;
            StartupConfig {
                server: ServerStartupConfig::Default {
                    quiet,
                    cwd: cwd.clone(),
                },
                auto_start: AutoStartConfig::default_files(quiet, cwd),
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
            }
        }
    }
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

pub(super) fn run_foreground_server(
    socket_path: &Path,
    startup_config: &StartupConfig,
) -> Result<i32, ExitFailure> {
    let config = apply_server_startup_config(
        DaemonConfig::new(socket_path.to_path_buf()),
        &startup_config.server,
    );
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| ExitFailure::new(1, error.to_string()))?;

    runtime
        .block_on(async move {
            let server = ServerDaemon::new(config).bind().await?;
            server.wait().await
        })
        .map(|()| 0)
        .map_err(|error| ExitFailure::new(1, error.to_string()))
}
