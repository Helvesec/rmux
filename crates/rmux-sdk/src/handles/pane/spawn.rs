use std::future::{Future, IntoFuture};
use std::path::PathBuf;
use std::pin::Pin;

use crate::RmuxError;
use crate::{Pane, PaneRef, PaneRespawnOptions, ProcessCommandSpec, ProcessSpec, Result};

/// Builder returned by [`Pane::spawn`].
#[derive(Debug)]
pub struct PaneSpawnBuilder<'a> {
    pane: &'a Pane,
    command: ProcessCommandSpec,
    cwd: Option<PathBuf>,
    env: Vec<String>,
    kill_existing: bool,
    title: Option<String>,
    keep_alive_on_exit: Option<bool>,
}

impl<'a> PaneSpawnBuilder<'a> {
    pub(crate) fn argv(pane: &'a Pane, command: Vec<String>) -> Self {
        Self {
            pane,
            command: ProcessCommandSpec::Argv(command),
            cwd: None,
            env: Vec::new(),
            kill_existing: false,
            title: None,
            keep_alive_on_exit: None,
        }
    }

    pub(crate) fn shell(pane: &'a Pane, command: String) -> Self {
        Self {
            pane,
            command: ProcessCommandSpec::Shell(command),
            cwd: None,
            env: Vec::new(),
            kill_existing: false,
            title: None,
            keep_alive_on_exit: None,
        }
    }

    /// Sets the process working directory.
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Adds one process environment override.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push(format!("{}={}", key.into(), value.into()));
        self
    }

    /// Allows replacing an already-running process.
    #[must_use]
    pub const fn kill_existing(mut self, kill_existing: bool) -> Self {
        self.kill_existing = kill_existing;
        self
    }

    /// Sets a UX title label after the spawn succeeds.
    #[must_use]
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Controls whether the pane stays visible after the spawned process
    /// exits.
    #[must_use]
    pub const fn keep_alive_on_exit(mut self, keep_alive: bool) -> Self {
        self.keep_alive_on_exit = Some(keep_alive);
        self
    }

    async fn run(self) -> Result<PaneRef> {
        if self.command.is_empty() {
            return Err(RmuxError::SpawnFailed {
                message: rmux_proto::PROCESS_COMMAND_EMPTY_MESSAGE.to_owned(),
            });
        }
        let (pane, _) = self.pane.begin_pinned_operation_handle().await?;
        let target = pane
            .respawn(PaneRespawnOptions {
                kill: self.kill_existing,
                start_directory: self.cwd,
                process: ProcessSpec {
                    command: None,
                    process_command: Some(self.command),
                    environment: non_empty(self.env),
                },
                keep_alive_on_exit: self.keep_alive_on_exit,
            })
            .await?;
        if let Some(title) = self.title {
            pane.set_title(title).await?;
        }
        Ok(target)
    }
}

impl<'a> IntoFuture for PaneSpawnBuilder<'a> {
    type Output = Result<PaneRef>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.run())
    }
}

fn non_empty(values: Vec<String>) -> Option<Vec<String>> {
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}
