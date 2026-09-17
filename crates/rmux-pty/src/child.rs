use std::ffi::{OsStr, OsString};
use std::fmt;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
#[cfg(unix)]
use std::process::{Child, Command, Stdio};

#[cfg(all(not(unix), not(windows)))]
use crate::unsupported_op;
#[cfg(all(not(unix), not(windows)))]
use crate::PtyError;
#[cfg(any(unix, windows))]
use crate::{backend, PtyPair};
use crate::{ProcessId, PtyMaster, Result, Signal, TerminalSize};

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
pub trait ShellChild: fmt::Debug + Send {
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

/// The default backend: an operating-system process, forked and executed on
/// Unix and launched into a ConPTY on Windows.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessShell;

impl RmuxShell for ProcessShell {
    fn spawn(&self, command: ChildCommand) -> Result<SpawnedPty> {
        spawn_child(command)
    }
}

/// The [`ShellChild`] implementation backing [`ProcessShell`].
#[derive(Debug)]
#[cfg_attr(all(not(unix), not(windows)), allow(dead_code))]
struct ProcessChild {
    #[cfg(unix)]
    child: Child,
    #[cfg(windows)]
    child: backend::WindowsChild,
    pid: ProcessId,
}

impl ShellChild for ProcessChild {
    fn pid(&self) -> ProcessId {
        self.pid
    }

    fn wait(&mut self) -> Result<ExitStatus> {
        #[cfg(unix)]
        {
            Ok(self.child.wait()?)
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                backend::wait_child(&mut self.child)
            }

            #[cfg(not(windows))]
            {
                Err(PtyError::Unsupported(unsupported_op::WAIT_FOR_PTY_CHILD))
            }
        }
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        #[cfg(unix)]
        {
            Ok(self.child.try_wait()?)
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                backend::try_wait_child(&mut self.child)
            }

            #[cfg(not(windows))]
            {
                Err(PtyError::Unsupported(
                    unsupported_op::TRY_WAIT_FOR_PTY_CHILD,
                ))
            }
        }
    }

    fn kill(&self, signal: Signal) -> Result<()> {
        #[cfg(unix)]
        {
            backend::kill_foreground_process_group(self.pid, signal)
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                backend::kill_child(&self.child, signal)
            }

            #[cfg(not(windows))]
            {
                let _ = signal;
                Err(PtyError::Unsupported(unsupported_op::SIGNAL_PTY_FOREGROUND))
            }
        }
    }

    fn kill_session_leader(&self, signal: Signal) -> Result<()> {
        #[cfg(unix)]
        {
            backend::kill_process(self.pid, signal)?;
            Ok(())
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                backend::kill_child(&self.child, signal)
            }

            #[cfg(not(windows))]
            {
                let _ = signal;
                Err(PtyError::Unsupported(
                    unsupported_op::SIGNAL_PTY_SESSION_LEADER,
                ))
            }
        }
    }

    #[cfg(unix)]
    fn continue_if_stopped(&self) -> Result<bool> {
        let Some(stop_signal) = backend::stopped_signal(self.pid)? else {
            return Ok(false);
        };
        if stop_signal == libc::SIGTTIN || stop_signal == libc::SIGTTOU {
            return Ok(false);
        }

        backend::kill_foreground_process_group(self.pid, Signal::CONT)
            .or_else(|_| backend::kill_process(self.pid, Signal::CONT))?;
        Ok(true)
    }

    #[cfg(windows)]
    fn wait_for_process_tree_exit(&mut self) -> Result<ExitStatus> {
        backend::wait_child_process_tree(&mut self.child)
    }

    #[cfg(windows)]
    fn try_wait_for_process_tree_exit(&mut self) -> Result<Option<ExitStatus>> {
        backend::try_wait_child_process_tree(&mut self.child)
    }

    #[cfg(windows)]
    fn try_clone_for_wait(&self) -> Result<Box<dyn ShellChild>> {
        Ok(Box::new(Self {
            child: backend::try_clone_child_for_wait(&self.child)?,
            pid: self.pid,
        }))
    }

    #[cfg(windows)]
    fn try_clone_for_exit_teardown(&self) -> Result<Box<dyn ShellChild>> {
        Ok(Box::new(Self {
            child: backend::try_clone_child_for_exit_teardown(&self.child)?,
            pid: self.pid,
        }))
    }

    #[cfg(windows)]
    fn close_pseudoconsole(&self) {
        backend::close_child_pseudoconsole(&self.child);
    }
}

/// A handle for signaling and reaping the shell behind a PTY.
#[derive(Debug)]
pub struct PtyChild {
    inner: Box<dyn ShellChild>,
}

impl PtyChild {
    /// Wraps a [`ShellChild`] produced by an [`RmuxShell`] backend.
    #[must_use]
    pub fn new(child: impl ShellChild + 'static) -> Self {
        Self {
            inner: Box::new(child),
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
        self.inner.wait()
    }

    /// Attempts to reap the child process without blocking.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        self.inner.try_wait()
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

#[cfg(unix)]
fn spawn_child(command: ChildCommand) -> Result<SpawnedPty> {
    let pair = match command.size {
        Some(size) => PtyPair::open_with_size(size)?,
        None => PtyPair::open()?,
    };
    let (master, slave) = pair.into_split();
    let raw_master_fd = master.raw_fd();
    let startup_slave = slave.try_clone()?.into_owned_fd();
    let master = master.with_startup_slave(startup_slave);

    let stdin = File::from(slave.try_clone()?.into_owned_fd());
    let stdout = File::from(slave.try_clone()?.into_owned_fd());
    let stderr = File::from(slave.into_owned_fd());

    let mut std_command = Command::new(&command.program);
    if let Some(arg0) = &command.arg0 {
        std_command.arg0(arg0);
    }
    std_command.args(&command.args);
    std_command.stdin(Stdio::from(stdin));
    std_command.stdout(Stdio::from(stdout));
    std_command.stderr(Stdio::from(stderr));
    if command.clear_env {
        std_command.env_clear();
    }
    if let Some(current_dir) = &command.current_dir {
        std_command.current_dir(current_dir);
    }

    for (key, value) in &command.env {
        std_command.env(key, value);
    }

    let pre_exec = move || {
        rmux_os::signals::reset_child_signal_dispositions()?;
        backend::setup_child_controlling_terminal(raw_master_fd)
    };

    // SAFETY: The closure only performs post-fork child setup that is required
    // for PTY correctness: it closes the child's inherited master fd copy,
    // creates a new session, installs the slave as the controlling terminal,
    // and sets the child process group to the foreground process group on the
    // PTY. The closure does not touch parent-owned Rust state after fork.
    unsafe {
        std_command.pre_exec(pre_exec);
    }

    let child = std_command.spawn()?;
    let pid = ProcessId::new(child.id())?;

    Ok(SpawnedPty {
        master,
        child: PtyChild::new(ProcessChild { child, pid }),
    })
}

#[cfg(not(unix))]
fn spawn_child(_command: ChildCommand) -> Result<SpawnedPty> {
    #[cfg(windows)]
    {
        let pair = match _command.size {
            Some(size) => PtyPair::open_with_size(size)?,
            None => PtyPair::open()?,
        };
        let master = pair.into_master();
        let child = backend::spawn_child(_command, master.windows_pty())?;
        let pid = child.pid();
        Ok(SpawnedPty {
            master,
            child: PtyChild::new(ProcessChild { child, pid }),
        })
    }

    #[cfg(not(windows))]
    {
        Err(PtyError::Unsupported(unsupported_op::SPAWN_PTY_CHILD))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io::Write;
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;
    use std::process::ExitStatus;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::{ChildCommand, ProcessId, PtyChild, RmuxShell, ShellChild, SpawnedPty};
    use crate::{PtyMaster, PtyPair, PtySlave, Result, Signal, TerminalSize};

    /// A shell child that never touches the operating system: it reports the
    /// host process, records every signal, and exits once signaled.
    #[derive(Debug)]
    struct ScriptedChild {
        exit: ExitStatus,
        killed: Arc<AtomicBool>,
        signals: Arc<Mutex<Vec<Signal>>>,
        _terminal: PtySlave,
    }

    impl ShellChild for ScriptedChild {
        fn pid(&self) -> ProcessId {
            ProcessId::new(std::process::id()).expect("host pid")
        }

        fn wait(&mut self) -> Result<ExitStatus> {
            Ok(self.exit)
        }

        fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
            Ok(self.killed.load(Ordering::SeqCst).then_some(self.exit))
        }

        fn kill(&self, signal: Signal) -> Result<()> {
            self.signals.lock().expect("signal log").push(signal);
            self.killed.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn kill_session_leader(&self, signal: Signal) -> Result<()> {
            self.kill(signal)
        }

        fn continue_if_stopped(&self) -> Result<bool> {
            Ok(false)
        }
    }

    #[derive(Debug)]
    struct ScriptedShell {
        output: &'static [u8],
        exit: ExitStatus,
        seen: Arc<Mutex<Option<ChildCommand>>>,
        signals: Arc<Mutex<Vec<Signal>>>,
    }

    impl RmuxShell for ScriptedShell {
        fn spawn(&self, command: ChildCommand) -> Result<SpawnedPty> {
            let size = command.initial_size().unwrap_or(TerminalSize::new(80, 24));
            *self.seen.lock().expect("spawn log") = Some(command);

            let (master, slave) = PtyPair::open_with_size(size)?.into_split();
            let mut terminal = File::from(slave.try_clone()?.into_owned_fd());
            terminal.write_all(self.output)?;
            drop(terminal);

            Ok(SpawnedPty::new(
                master,
                PtyChild::new(ScriptedChild {
                    exit: self.exit,
                    killed: Arc::new(AtomicBool::new(false)),
                    signals: Arc::clone(&self.signals),
                    _terminal: slave,
                }),
            ))
        }
    }

    fn read_from_master(master: &PtyMaster, len: usize) -> Vec<u8> {
        let io = master.io();
        let mut buffer = vec![0_u8; len];
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut offset = 0;
        while offset < buffer.len() {
            match io.try_read(&mut buffer[offset..]) {
                Ok(0) => panic!("pty master reached end of file after {offset} bytes"),
                Ok(read) => offset += read,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    assert!(Instant::now() < deadline, "timed out reading pty master");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("pty master read failed: {error}"),
            }
        }
        buffer
    }

    #[test]
    fn custom_shell_backend_drives_pty_child_through_the_trait() {
        let seen = Arc::new(Mutex::new(None));
        let signals = Arc::new(Mutex::new(Vec::new()));
        let shell: Arc<dyn RmuxShell> = Arc::new(ScriptedShell {
            output: b"scripted\n",
            exit: ExitStatusExt::from_raw(3 << 8),
            seen: Arc::clone(&seen),
            signals: Arc::clone(&signals),
        });

        let mut spawned = shell
            .spawn(
                ChildCommand::new("/opt/brush")
                    .arg0("-brush")
                    .args(["-c", "true"])
                    .env("RMUX_TEST", "1")
                    .current_dir("/tmp")
                    .clear_env()
                    .size(TerminalSize::new(80, 24)),
            )
            .expect("scripted shell spawns");

        let recorded = seen
            .lock()
            .expect("spawn log")
            .take()
            .expect("spawn request");
        assert_eq!(recorded.program(), Path::new("/opt/brush"));
        assert_eq!(recorded.argv0(), Some(OsStr::new("-brush")));
        assert_eq!(recorded.arguments(), [OsStr::new("-c"), OsStr::new("true")]);
        assert_eq!(
            recorded.environment(),
            [(
                OsStr::new("RMUX_TEST").to_owned(),
                OsStr::new("1").to_owned()
            )]
        );
        assert!(recorded.clears_env());
        assert_eq!(recorded.working_dir(), Some(Path::new("/tmp")));
        assert_eq!(recorded.initial_size(), Some(TerminalSize::new(80, 24)));

        assert_eq!(
            spawned.master().size().expect("master size"),
            TerminalSize::new(80, 24)
        );
        assert_eq!(read_from_master(spawned.master(), 10), b"scripted\r\n");

        let child = spawned.child_mut();
        assert_eq!(child.pid().as_u32(), std::process::id());
        assert_eq!(child.try_wait().expect("try_wait"), None);

        child.interrupt().expect("interrupt");
        child.terminate_forcefully().expect("terminate");
        assert_eq!(
            *signals.lock().expect("signal log"),
            [Signal::INT, Signal::KILL]
        );
        assert_eq!(
            child.try_wait().expect("try_wait").and_then(|s| s.code()),
            Some(3)
        );
    }
}
