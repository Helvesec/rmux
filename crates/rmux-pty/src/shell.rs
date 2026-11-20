use std::fmt;
use std::process::ExitStatus;

use crate::{ChildCommand, ProcessId, Result, Signal, SpawnedPty};

/// Lifecycle control for whatever runs on the child side of a pane PTY.
///
/// Implemented by the operating-system process backend behind [`ProcessShell`]
/// and by any custom [`RmuxShell`] backend, for example an in-process shell
/// library. Backends without an operating-system wait status build their
/// [`ExitStatus`] with `std::os::unix::process::ExitStatusExt::from_raw`.
///
/// The Windows methods are ConPTY-specific. A Windows pseudo console has no
/// in-process child endpoint, so the process backend is the only implementor
/// there and every method is required.
///
/// [`ProcessShell`]: crate::ProcessShell
pub trait ShellChild: fmt::Debug + Send + Sync {
    /// Returns the process identifier RMUX attributes the pane to.
    ///
    /// The process backend returns the PTY session leader, which is also the
    /// PTY foreground process-group identifier. An in-process shell reports the
    /// host process instead; RMUX derives `#{pane_pid}` and foreground-process
    /// probing from this value either way.
    fn pid(&self) -> ProcessId;

    /// Waits for the shell to exit and reaps it.
    fn wait(&mut self) -> Result<ExitStatus>;

    /// Attempts to reap the shell without blocking.
    fn try_wait(&mut self) -> Result<Option<ExitStatus>>;

    /// Sends a signal to the PTY foreground process group, or to the backend's
    /// equivalent of it.
    fn kill(&self, signal: Signal) -> Result<()>;

    /// Sends a signal directly to the PTY session leader.
    fn kill_session_leader(&self, signal: Signal) -> Result<()>;

    /// Continues a stopped foreground process group, reporting `Ok(false)` when
    /// nothing was stopped.
    #[cfg(unix)]
    fn continue_if_stopped(&self) -> Result<bool>;

    /// Waits until the Windows pane leader and every job-scoped descendant exit.
    #[cfg(windows)]
    fn wait_for_process_tree_exit(&mut self) -> Result<ExitStatus>;

    /// Observes Windows process-tree exit without blocking.
    #[cfg(windows)]
    fn try_wait_for_process_tree_exit(&mut self) -> Result<Option<ExitStatus>>;

    /// Clones a wait-only handle without retaining the Windows Job Object.
    #[cfg(windows)]
    fn try_clone_for_wait(&self) -> Result<Box<dyn ShellChild>>;

    /// Clones the handles a pane exit watcher needs to observe the complete
    /// Windows process tree before the pseudo console is closed.
    #[cfg(windows)]
    fn try_clone_for_exit_teardown(&self) -> Result<Box<dyn ShellChild>>;

    /// Closes the backing ConPTY after the shell has exited.
    #[cfg(windows)]
    fn close_pseudoconsole(&self);
}

/// A pluggable pane shell backend.
///
/// An implementation turns a spawn request into a shell attached to the child
/// side of a newly allocated PTY. RMUX reads, writes, and resizes the returned
/// PTY master exactly as it does for an operating-system process, so the
/// multiplexer's I/O path is unaffected by the choice of backend.
pub trait RmuxShell: fmt::Debug + Send + Sync {
    /// Spawns the shell described by `command` inside a newly allocated PTY.
    fn spawn(&self, command: ChildCommand) -> Result<SpawnedPty>;
}

#[cfg(all(test, unix))]
#[path = "shell_tests.rs"]
mod tests;
