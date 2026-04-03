use std::ffi::OsString;
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};

use rustix::process::{getpid, ioctl_tiocsctty, kill_process_group, setsid, Pid};
use rustix::termios::tcsetpgrp;

use crate::{PtyError, PtyMaster, PtyPair, Result, Signal, TerminalSize};

/// A command configuration for spawning a process inside a newly allocated PTY.
#[derive(Clone, Debug)]
pub struct ChildCommand {
    program: PathBuf,
    arg0: Option<OsString>,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    clear_env: bool,
    current_dir: Option<PathBuf>,
    size: Option<TerminalSize>,
}

impl ChildCommand {
    /// Creates a PTY child command that will execute `program`.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arg0: None,
            args: Vec::new(),
            env: Vec::new(),
            clear_env: false,
            current_dir: None,
            size: None,
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
    child: Child,
    pid: Pid,
}

impl PtyChild {
    /// Returns the PTY session leader's process identifier.
    ///
    /// The spawned child creates a fresh session and foreground process group,
    /// so this PID is also the PTY process-group identifier used for later
    /// signal delivery.
    #[must_use]
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Waits for the child process to exit and reaps it.
    pub fn wait(&mut self) -> Result<ExitStatus> {
        Ok(self.child.wait()?)
    }

    /// Attempts to reap the child process without blocking.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    /// Sends a signal to the PTY foreground process group.
    ///
    /// PTY-backed sessions commonly fan out into multiple processes while
    /// sharing the foreground group created during spawn. Signaling the group
    /// preserves teardown correctness even when the session leader has already
    /// delegated work to descendants.
    pub fn kill(&self, signal: Signal) -> Result<()> {
        kill_process_group(self.pid, signal)?;
        Ok(())
    }
}

fn spawn_child(command: ChildCommand) -> Result<SpawnedPty> {
    let pair = match command.size {
        Some(size) => PtyPair::open_with_size(size)?,
        None => PtyPair::open()?,
    };
    let (master, slave) = pair.into_split();
    let raw_master_fd = master.as_fd().as_raw_fd();

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
        // SAFETY: This closes only the child process' inherited copy of the
        // PTY master fd. The parent still owns its separate descriptor.
        unsafe { rustix::io::close(raw_master_fd) };

        setsid().map_err(std::io::Error::from)?;

        // SAFETY: `stdin` has already been wired to the PTY slave by
        // `Command`, so fd 0 is a valid borrowed descriptor for the rest
        // of the closure.
        let slave_stdin = unsafe { BorrowedFd::borrow_raw(0) };
        ioctl_tiocsctty(slave_stdin).map_err(std::io::Error::from)?;
        tcsetpgrp(slave_stdin, getpid()).map_err(std::io::Error::from)?;

        Ok(())
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
    let pid =
        Pid::from_raw(i32::try_from(child.id()).map_err(|_| PtyError::InvalidPid(child.id()))?)
            .ok_or(PtyError::InvalidPid(child.id()))?;

    Ok(SpawnedPty {
        master,
        child: PtyChild { child, pid },
    })
}
