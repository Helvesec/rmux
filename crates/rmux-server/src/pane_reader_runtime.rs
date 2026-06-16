use std::future::Future;

use tokio::runtime::Handle;
use tokio::task::AbortHandle;

#[derive(Clone, Debug)]
pub(crate) struct PaneReaderRuntime {
    handle: Handle,
}

impl PaneReaderRuntime {
    /// Captures the long-lived server runtime used for Unix PTY reader tasks.
    ///
    /// Attached prompt commands can execute on short-lived helper runtimes; pane
    /// readers must not be spawned there because they outlive the command that
    /// created the pane.
    pub(crate) fn current() -> Option<Self> {
        Handle::try_current().ok().map(|handle| Self { handle })
    }

    #[cfg(test)]
    pub(crate) fn from_handle(handle: Handle) -> Self {
        Self { handle }
    }

    pub(crate) fn spawn<F>(&self, task: F) -> AbortHandle
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.handle.spawn(task).abort_handle()
    }
}
