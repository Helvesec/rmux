use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use crate::pty::SessionLeaderExit;
use crate::{
    ProcessId, ProcessShell, PtyMaster, Result, RmuxShell, ShellChild, Signal, TerminalSize,
};

/// A command configuration for spawning a process inside a newly allocated PTY.
#[derive(Clone, Debug)]
#[cfg_attr(not(unix), allow(dead_code))]
pub struct ChildCommand {
    pub(crate) program: PathBuf,
    pub(crate) arg0: Option<OsString>,
    pub(crate) args: Vec<OsString>,
    pub(crate) windows_verbatim_args: Option<OsString>,
    pub(crate) env: Vec<(OsString, OsString)>,
    pub(crate) clear_env: bool,
    pub(crate) current_dir: Option<PathBuf>,
    pub(crate) size: Option<TerminalSize>,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) allow_explicit_job_breakaway: bool,
}

impl ChildCommand {
    /// Creates a PTY child command that will execute `program`.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arg0: None,
            args: Vec::new(),
            windows_verbatim_args: None,
            env: Vec::new(),
            clear_env: false,
            current_dir: None,
            size: None,
            allow_explicit_job_breakaway: false,
        }
    }

    /// Overrides `argv[0]` without changing the executable path.
    #[must_use]
    pub fn arg0(mut self, arg0: impl Into<OsString>) -> Self {
        self.arg0 = Some(arg0.into());
        self
    }

    /// Appends a single process argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Appends multiple process arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Appends raw command-line text on Windows after the normal argument list.
    ///
    /// This is reserved for command interpreters such as `cmd.exe` whose `/C`
    /// tail has quoting rules that cannot be represented as independent argv
    /// tokens. Non-Windows backends ignore the field.
    #[must_use]
    pub fn windows_verbatim_args(mut self, args: impl Into<OsString>) -> Self {
        self.windows_verbatim_args = Some(args.into());
        self
    }

    /// Sets or overrides a process environment variable.
    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Clears the inherited process environment before applying explicit entries.
    #[must_use]
    pub fn clear_env(mut self) -> Self {
        self.clear_env = true;
        self
    }

    /// Sets the child working directory.
    #[must_use]
    pub fn current_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(path.into());
        self
    }

    /// Sets an initial PTY size for the child process.
    #[must_use]
    pub fn size(mut self, size: TerminalSize) -> Self {
        self.size = Some(size);
        self
    }

    /// Allows Windows descendants that explicitly request job breakaway to
    /// outlive this PTY child while ordinary descendants remain job-scoped.
    ///
    /// This is a no-op on non-Windows platforms.
    #[must_use]
    pub fn allow_explicit_job_breakaway(mut self) -> Self {
        self.allow_explicit_job_breakaway = true;
        self
    }

    /// Returns the executable path this command runs.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Returns the `argv[0]` override, when one was configured.
    #[must_use]
    pub fn argv0(&self) -> Option<&OsStr> {
        self.arg0.as_deref()
    }

    /// Returns the process arguments that follow `argv[0]`.
    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.args
    }

    /// Returns the explicit environment entries in application order.
    #[must_use]
    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.env
    }

    /// Returns whether the inherited environment is cleared before the
    /// entries from [`environment`](Self::environment) are applied.
    #[must_use]
    pub fn clears_env(&self) -> bool {
        self.clear_env
    }

    /// Returns the child working directory, when one was configured.
    #[must_use]
    pub fn working_dir(&self) -> Option<&Path> {
        self.current_dir.as_deref()
    }

    /// Returns the initial PTY size, when one was configured.
    #[must_use]
    pub fn initial_size(&self) -> Option<TerminalSize> {
        self.size
    }

    /// Spawns the configured command through the default [`ProcessShell`].
    pub fn spawn(self) -> Result<SpawnedPty> {
        ProcessShell.spawn(self)
    }
}

/// A spawned process together with the PTY master used to communicate with it.
#[derive(Debug)]
pub struct SpawnedPty {
    master: PtyMaster,
    child: PtyChild,
}

impl SpawnedPty {
    /// Pairs a PTY master with the shell running on the other endpoint.
    ///
    /// [`RmuxShell`] implementations outside this crate use this to return
    /// their spawn result.
    #[must_use]
    pub fn new(master: PtyMaster, child: PtyChild) -> Self {
        #[cfg(unix)]
        let child = {
            let exit = master.io().record_session_leader(child.pid());
            child.with_session_leader_exit(exit)
        };
        Self { master, child }
    }

    /// Returns the PTY master endpoint.
    #[must_use]
    pub fn master(&self) -> &PtyMaster {
        &self.master
    }

    /// Returns the child-process handle.
    #[must_use]
    pub fn child(&self) -> &PtyChild {
        &self.child
    }

    /// Returns the child-process handle mutably for waiting and reaping.
    #[must_use]
    pub fn child_mut(&mut self) -> &mut PtyChild {
        &mut self.child
    }

    /// Consumes the wrapper and returns the PTY master and child handle.
    #[must_use]
    pub fn into_parts(self) -> (PtyMaster, PtyChild) {
        (self.master, self.child)
    }
}

/// A handle for signaling and reaping the shell behind a PTY.
#[derive(Debug)]
pub struct PtyChild {
    inner: Box<dyn ShellChild>,
    /// The exit record of the PTY this child was paired with by
    /// [`SpawnedPty::new`], marked when this handle reaps the process.
    #[cfg(unix)]
    session_leader_exit: Option<Arc<SessionLeaderExit>>,
}

impl PtyChild {
    /// Wraps a [`ShellChild`] produced by an [`RmuxShell`] backend.
    #[must_use]
    pub fn new(child: impl ShellChild + 'static) -> Self {
        Self {
            inner: Box::new(child),
            #[cfg(unix)]
            session_leader_exit: None,
        }
    }

    /// Returns the PTY session leader's process identifier.
    ///
    /// The spawned child creates a fresh session and foreground process group,
    /// so this PID is also the PTY process-group identifier used for later
    /// signal delivery.
    #[must_use]
    pub fn pid(&self) -> ProcessId {
        self.inner.pid()
    }

    /// Waits for the child process to exit and reaps it.
    pub fn wait(&mut self) -> Result<ExitStatus> {
        let status = self.inner.wait()?;
        self.note_reaped();
        Ok(status)
    }

    /// Attempts to reap the child process without blocking.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        let status = self.inner.try_wait()?;
        if status.is_some() {
            self.note_reaped();
        }
        Ok(status)
    }

    #[cfg(unix)]
    fn with_session_leader_exit(mut self, exit: Arc<SessionLeaderExit>) -> Self {
        self.session_leader_exit = Some(exit);
        self
    }

    fn note_reaped(&self) {
        #[cfg(unix)]
        if let Some(exit) = &self.session_leader_exit {
            exit.mark_reaped();
        }
    }

    /// Waits until the Windows pane leader and every job-scoped descendant exit.
    #[cfg(windows)]
    pub fn wait_for_process_tree_exit(&mut self) -> Result<ExitStatus> {
        self.inner.wait_for_process_tree_exit()
    }

    /// Observes Windows process-tree exit without blocking.
    #[cfg(windows)]
    pub fn try_wait_for_process_tree_exit(&mut self) -> Result<Option<ExitStatus>> {
        self.inner.try_wait_for_process_tree_exit()
    }

    /// Clones a wait-only handle without retaining the Windows Job Object.
    ///
    /// Popup teardown relies on dropping the owning child to close the last
    /// kill-on-close Job handle before this clone closes the shared ConPTY.
    #[cfg(windows)]
    pub fn try_clone_for_wait(&self) -> Result<Self> {
        Ok(Self {
            inner: self.inner.try_clone_for_wait()?,
        })
    }

    /// Clones the process, ConPTY, and Windows Job Object handles needed by a
    /// pane exit watcher to observe the complete process tree before close.
    #[cfg(windows)]
    pub fn try_clone_for_exit_teardown(&self) -> Result<Self> {
        Ok(Self {
            inner: self.inner.try_clone_for_exit_teardown()?,
        })
    }

    /// Closes the backing ConPTY after the child has exited.
    ///
    /// Windows keeps the ConPTY output pipe alive while the pseudo console is
    /// open. The server's exit watcher calls this after the entire pane process
    /// tree exits so the output reader observes EOF without destroying a live
    /// descendant that still owns the console.
    #[cfg(windows)]
    pub fn close_pseudoconsole(&self) {
        self.inner.close_pseudoconsole();
    }

    /// Sends an interrupt request to the PTY foreground process group.
    pub fn interrupt(&self) -> Result<()> {
        self.kill(Signal::INT)
    }

    /// Sends a forceful kill request to the PTY foreground process group.
    pub fn terminate_forcefully(&self) -> Result<()> {
        self.kill(Signal::KILL)
    }

    /// Sends a signal to the PTY foreground process group.
    ///
    /// PTY-backed sessions commonly fan out into multiple processes while
    /// sharing the foreground group created during spawn. Signaling the group
    /// preserves teardown correctness even when the session leader has already
    /// delegated work to descendants.
    pub fn kill(&self, signal: Signal) -> Result<()> {
        self.inner.kill(signal)
    }

    /// Sends a signal directly to the PTY session leader.
    ///
    /// This is a teardown fallback for shells that move foreground jobs into a
    /// different process group while the session leader is still the child that
    /// must be reaped by RMUX.
    pub fn kill_session_leader(&self, signal: Signal) -> Result<()> {
        self.inner.kill_session_leader(signal)
    }

    /// Continues the PTY foreground process group if the session leader is
    /// currently stopped.
    ///
    /// This mirrors tmux's SIGCHLD policy for stopped panes while leaving
    /// SIGTTIN/SIGTTOU alone so background terminal I/O remains governed by
    /// normal Unix job-control rules.
    #[cfg(unix)]
    pub fn continue_if_stopped(&self) -> Result<bool> {
        self.inner.continue_if_stopped()
    }
}
