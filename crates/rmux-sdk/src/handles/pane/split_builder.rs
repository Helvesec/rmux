use std::future::{Future, IntoFuture};
use std::path::PathBuf;
use std::pin::Pin;

use crate::handles::split::SplitDirection;
use crate::{Pane, ProcessCommandSpec, ProcessSpec, Result, RmuxError};

use super::split::split_pane_with_process;

/// Builder returned by [`Pane::split_with`].
#[derive(Debug)]
pub struct PaneSplitBuilder<'a> {
    pane: &'a Pane,
    direction: SplitDirection,
    process: ProcessSpec,
    cwd: Option<PathBuf>,
    title: Option<String>,
    keep_alive_on_exit: Option<bool>,
}

impl<'a> PaneSplitBuilder<'a> {
    pub(crate) fn new(pane: &'a Pane, direction: SplitDirection) -> Self {
        Self {
            pane,
            direction,
            process: ProcessSpec::default(),
            cwd: None,
            title: None,
            keep_alive_on_exit: None,
        }
    }

    /// Spawns the new pane with the supplied argv.
    ///
    /// On Windows, `.bat` and `.cmd` targets are rejected; use a native
    /// executable for literal argv, or [`Self::shell`] when shell
    /// interpretation is intentional.
    #[must_use]
    pub fn spawn<I, S>(mut self, command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.process.process_command = Some(ProcessCommandSpec::Argv(
            command.into_iter().map(Into::into).collect(),
        ));
        self
    }

    /// Spawns the new pane through the user's shell.
    #[must_use]
    pub fn shell(mut self, command: impl Into<String>) -> Self {
        self.process.process_command = Some(ProcessCommandSpec::Shell(command.into()));
        self
    }

    /// Sets the process working directory for the new pane.
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Adds one process environment override for the new pane.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let entry = format!("{}={}", key.into(), value.into());
        self.process
            .environment
            .get_or_insert_with(Vec::new)
            .push(entry);
        self
    }

    /// Sets a UX title label after the split succeeds.
    #[must_use]
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Controls whether the new pane stays visible after its process exits.
    #[must_use]
    pub const fn keep_alive_on_exit(mut self, keep_alive: bool) -> Self {
        self.keep_alive_on_exit = Some(keep_alive);
        self
    }

    async fn run(self) -> Result<Pane> {
        if self
            .process
            .process_command
            .as_ref()
            .is_some_and(ProcessCommandSpec::is_empty)
        {
            return Err(RmuxError::SpawnFailed {
                message: rmux_proto::PROCESS_COMMAND_EMPTY_MESSAGE.to_owned(),
            });
        }
        let operation_pane = self.pane.begin_operation_handle();
        let outcome = split_pane_with_process(
            operation_pane.transport(),
            operation_pane.split_target_text(),
            self.direction,
            self.process,
            self.cwd,
            self.keep_alive_on_exit,
        )
        .await?;
        let pane = Pane::new_by_id(
            outcome.target,
            outcome.pane_id,
            operation_pane.endpoint().clone(),
            operation_pane.configured_default_timeout(),
            operation_pane.transport().clone(),
        );
        if let Some(title) = self.title {
            let mut scoped_pane = pane.clone();
            scoped_pane.transport = operation_pane.transport().clone();
            scoped_pane.set_title(title).await?;
        }
        Ok(pane)
    }
}

impl<'a> IntoFuture for PaneSplitBuilder<'a> {
    type Output = Result<Pane>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.run())
    }
}
