use std::ffi::OsString;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
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

    /// Spawns the configured command inside a newly allocated PTY.
    pub fn spawn(self) -> Result<SpawnedPty> {
        spawn_child(self)
    }
}

/// A spawned process together with the PTY master used to communicate with it.
#[derive(Debug)]
pub struct SpawnedPty {
    master: PtyMaster,
    child: PtyChild,
}

impl SpawnedPty {
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

/// A handle for signaling and reaping a PTY-backed child process.
#[derive(Debug)]
pub struct PtyChild {
    #[cfg(unix)]
    child: Child,
    #[cfg(windows)]
    child: backend::WindowsChild,
    pid: ProcessId,
}

impl PtyChild {
    /// Returns the PTY session leader's process identifier.
    ///
    /// The spawned child creates a fresh session and foreground process group,
    /// so this PID is also the PTY process-group identifier used for later
    /// signal delivery.
    #[must_use]
    pub fn pid(&self) -> ProcessId {
        self.pid
    }

    /// Waits for the child process to exit and reaps it.
    pub fn wait(&mut self) -> Result<ExitStatus> {
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

    /// Attempts to reap the child process without blocking.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
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

    /// Clones a wait-only handle without retaining the Windows Job Object.
    ///
    /// Popup teardown relies on dropping the owning child to close the last
    /// kill-on-close Job handle before this clone closes the shared ConPTY.
    #[cfg(windows)]
    pub fn try_clone_for_wait(&self) -> Result<Self> {
        Ok(Self {
            child: backend::try_clone_child_for_wait(&self.child)?,
            pid: self.pid,
        })
    }

    /// Clones the process, ConPTY, and Windows Job Object handles needed by a
    /// pane exit watcher to terminate surviving descendants before close.
    #[cfg(windows)]
    pub fn try_clone_for_exit_teardown(&self) -> Result<Self> {
        Ok(Self {
            child: backend::try_clone_child_for_exit_teardown(&self.child)?,
            pid: self.pid,
        })
    }

    /// Closes the backing ConPTY after the child has exited.
    ///
    /// Windows keeps the ConPTY output pipe alive while the pseudo console is
    /// open. The server's exit watcher calls this after `wait()` so the output
    /// reader observes EOF instead of blocking indefinitely on an already-dead
    /// child process.
    #[cfg(windows)]
    pub fn close_pseudoconsole(&self) {
        backend::close_child_pseudoconsole(&self.child);
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

    /// Sends a signal directly to the PTY session leader.
    ///
    /// This is a teardown fallback for shells that move foreground jobs into a
    /// different process group while the session leader is still the child that
    /// must be reaped by RMUX.
    pub fn kill_session_leader(&self, signal: Signal) -> Result<()> {
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

    /// Continues the PTY foreground process group if the session leader is
    /// currently stopped.
    ///
    /// This mirrors tmux's SIGCHLD policy for stopped panes while leaving
    /// SIGTTIN/SIGTTOU alone so background terminal I/O remains governed by
    /// normal Unix job-control rules.
    #[cfg(unix)]
    pub fn continue_if_stopped(&self) -> Result<bool> {
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
        child: PtyChild { child, pid },
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
            child: PtyChild { child, pid },
        })
    }

    #[cfg(not(windows))]
    {
        Err(PtyError::Unsupported(unsupported_op::SPAWN_PTY_CHILD))
    }
}
