use std::error::Error as StdError;
use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rmux_proto::AttachShellCommand;
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use rustix::process::{kill_process, Pid, Signal};
use rustix::termios::{
    tcflush, tcgetattr, tcsetattr, OptionalActions, QueueSelector, SpecialCodeIndex, Termios,
};

use super::terminal_cleanup::fallback_attach_stop_sequence;
use super::termination;

const TERMINATION_OUTPUT_RETRY: Duration = Duration::from_millis(100);
const TERMINATION_OUTPUT_RETRY_INTERVAL: Duration = Duration::from_millis(1);
const SHELL_CHILD_WAIT_INTERVAL: Duration = Duration::from_millis(10);
const SHELL_CHILD_TERMINATION_GRACE: Duration = Duration::from_millis(250);

pub(super) fn current_process_pid() -> io::Result<Pid> {
    let raw = i32::try_from(std::process::id())
        .map_err(|_| io::Error::other("process id does not fit in i32"))?;
    Pid::from_raw(raw).ok_or_else(|| io::Error::other("process id must be positive"))
}

/// Result type for raw-terminal lifecycle operations.
pub type Result<T> = std::result::Result<T, AttachError>;

/// Errors produced while entering or restoring raw terminal mode.
#[derive(Debug)]
pub enum AttachError {
    /// Duplicating the target file descriptor failed before raw mode was applied.
    Io(io::Error),
    /// A termios syscall failed while applying or restoring raw mode.
    Termios(rustix::io::Errno),
}

impl fmt::Display for AttachError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "terminal descriptor operation failed: {error}"),
            Self::Termios(errno) => write!(formatter, "terminal mode operation failed: {errno}"),
        }
    }
}

impl StdError for AttachError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Termios(errno) => Some(errno),
        }
    }
}

impl From<io::Error> for AttachError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rustix::io::Errno> for AttachError {
    fn from(errno: rustix::io::Errno) -> Self {
        Self::Termios(errno)
    }
}

/// A drop guard that applies raw mode to a terminal and restores the original
/// settings when dropped.
///
/// The guard duplicates the target file descriptor so restoration still works
/// even if the caller later drops the original handle.
#[derive(Debug)]
#[must_use = "keep the guard alive for as long as raw terminal mode is required"]
pub struct RawTerminal {
    fd: OwnedFd,
    original_termios: Termios,
}

impl RawTerminal {
    /// Enters raw mode for the process stdin file descriptor.
    pub fn enter() -> Result<Self> {
        Self::from_fd(&io::stdin())
    }

    /// Enters raw mode for the provided terminal file descriptor.
    ///
    /// The descriptor must refer to a terminal device. The guard duplicates the
    /// descriptor before applying raw mode so the caller may drop the original
    /// handle after creation.
    pub fn from_fd<Fd>(fd: &Fd) -> Result<Self>
    where
        Fd: AsFd,
    {
        let owned_fd = fd.as_fd().try_clone_to_owned()?;
        let original_termios = tcgetattr(&owned_fd)?;
        let mut raw_termios = original_termios.clone();
        configure_raw_mode(&mut raw_termios);
        tcsetattr(&owned_fd, OptionalActions::Now, &raw_termios)?;

        Ok(Self {
            fd: owned_fd,
            original_termios,
        })
    }

    /// Restores the terminal settings captured when the guard was created.
    ///
    /// This provides explicit restore support for callers that want error
    /// feedback before the guard later runs its drop path.
    pub fn restore(&self) -> Result<()> {
        tcsetattr(&self.fd, OptionalActions::Now, &self.original_termios)?;
        Ok(())
    }

    fn reapply_raw_mode(&self) -> Result<()> {
        let mut raw_termios = self.original_termios.clone();
        configure_raw_mode(&mut raw_termios);
        tcsetattr(&self.fd, OptionalActions::Now, &raw_termios)?;
        Ok(())
    }

    pub(super) fn run_lock_command(&self, command: &str) -> Result<()> {
        self.restore()?;
        let result = run_shell_command_with_terminal(&self.fd, "sh", command, None);
        let reapply_result = self.reapply_raw_mode();
        if let Err(error) = result {
            reapply_result?;
            return Err(error);
        }
        reapply_result?;
        Ok(())
    }

    pub(super) fn run_lock_shell_command(&self, command: &AttachShellCommand) -> Result<()> {
        self.restore()?;
        let result = run_shell_command_with_terminal(
            &self.fd,
            command.shell(),
            command.command(),
            Some(command.cwd()),
        );
        let reapply_result = self.reapply_raw_mode();
        if let Err(error) = result {
            reapply_result?;
            return Err(error);
        }
        reapply_result
    }

    pub(super) fn suspend_self(&self) -> Result<()> {
        self.restore()?;
        kill_process(current_process_pid()?, Signal::TSTP)?;
        self.reapply_raw_mode()?;
        Ok(())
    }

    pub(super) fn run_detach_exec_command(&self, command: &str) -> Result<()> {
        self.restore()?;
        run_shell_command_with_terminal(&self.fd, "sh", command, None)
    }

    pub(super) fn run_detach_exec_shell_command(&self, command: &AttachShellCommand) -> Result<()> {
        self.restore()?;
        run_shell_command_with_terminal(
            &self.fd,
            command.shell(),
            command.command(),
            Some(command.cwd()),
        )
    }

    pub(super) fn restore_attach_terminal_state(&self) -> Result<()> {
        let mut terminal = File::from(self.fd.as_fd().try_clone_to_owned()?);
        let term = std::env::var("TERM").unwrap_or_default();
        terminal.write_all(&fallback_attach_stop_sequence(&term))?;
        terminal.flush()?;
        Ok(())
    }

    pub(super) fn restore_after_termination(&self) -> Result<()> {
        self.restore()?;
        let _flags = self.interrupt_output_writer()?;
        let mut terminal = File::from(self.fd.as_fd().try_clone_to_owned()?);
        write_cleanup_with_deadline(
            &mut terminal,
            &fallback_attach_stop_sequence(&std::env::var("TERM").unwrap_or_default()),
        )
        .map_err(AttachError::Io)
    }

    pub(super) fn interrupt_output_writer(&self) -> Result<FileStatusFlagsGuard<'_>> {
        let flags = FileStatusFlagsGuard::set_nonblocking(self.fd.as_fd())?;
        tcflush(&self.fd, QueueSelector::OFlush)?;
        Ok(flags)
    }

    pub(super) fn flush_pending_input(&self) -> Result<()> {
        tcflush(&self.fd, QueueSelector::IFlush)?;
        Ok(())
    }
}

pub(super) struct FileStatusFlagsGuard<'fd> {
    fd: BorrowedFd<'fd>,
    original: OFlags,
}

impl<'fd> FileStatusFlagsGuard<'fd> {
    fn set_nonblocking(fd: BorrowedFd<'fd>) -> Result<Self> {
        let original = fcntl_getfl(fd).map_err(io::Error::from)?;
        fcntl_setfl(fd, original | OFlags::NONBLOCK).map_err(io::Error::from)?;
        Ok(Self { fd, original })
    }
}

impl Drop for FileStatusFlagsGuard<'_> {
    fn drop(&mut self) {
        let _ = fcntl_setfl(self.fd, self.original);
    }
}

fn write_cleanup_with_deadline(output: &mut File, mut bytes: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + TERMINATION_OUTPUT_RETRY;
    while !bytes.is_empty() {
        match output.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write terminal cleanup after attach termination",
                ))
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(TERMINATION_OUTPUT_RETRY_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    }
    output.flush()
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn configure_raw_mode(termios: &mut Termios) {
    termios.make_raw();
    termios.special_codes[SpecialCodeIndex::VMIN] = 1;
    termios.special_codes[SpecialCodeIndex::VTIME] = 0;
}

fn run_shell_command_with_terminal(
    fd: &OwnedFd,
    shell: &str,
    command: &str,
    cwd: Option<&str>,
) -> Result<()> {
    let stdin = File::from(fd.as_fd().try_clone_to_owned()?);
    let stdout = File::from(fd.as_fd().try_clone_to_owned()?);
    let stderr = File::from(fd.as_fd().try_clone_to_owned()?);
    let mut process = Command::new(shell);
    process
        .arg("-c")
        .arg(command)
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if let Some(cwd) = cwd {
        process.current_dir(cwd);
    }
    let mut child = process.spawn().map_err(AttachError::Io)?;
    wait_for_shell_child(&mut child, termination::requested_signal).map_err(AttachError::Io)?;
    Ok(())
}

fn wait_for_shell_child(
    child: &mut Child,
    requested_signal: impl Fn() -> Option<i32>,
) -> io::Result<()> {
    loop {
        if child_has_exited(child)? {
            return Ok(());
        }
        let Some(signal) = requested_signal() else {
            thread::sleep(SHELL_CHILD_WAIT_INTERVAL);
            continue;
        };

        forward_signal_to_child(child, signal)?;
        if wait_for_child_exit_until(child, Instant::now() + SHELL_CHILD_TERMINATION_GRACE)? {
            return Err(termination::interruption_error());
        }

        // The shell may trap or ignore the forwarded signal. Bound that grace
        // period too so terminal restoration and the attach guard can re-raise
        // the original signal promptly.
        let _ = child.kill();
        let _ = wait_for_child_exit_until(child, Instant::now() + SHELL_CHILD_TERMINATION_GRACE);
        return Err(termination::interruption_error());
    }
}

fn child_has_exited(child: &mut Child) -> io::Result<bool> {
    loop {
        match child.try_wait() {
            Ok(status) => return Ok(status.is_some()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

fn wait_for_child_exit_until(child: &mut Child, deadline: Instant) -> io::Result<bool> {
    while Instant::now() < deadline {
        if child_has_exited(child)? {
            return Ok(true);
        }
        thread::sleep(SHELL_CHILD_WAIT_INTERVAL);
    }
    child_has_exited(child)
}

fn forward_signal_to_child(child: &Child, signal: i32) -> io::Result<()> {
    let pid = i32::try_from(child.id())
        .map_err(|_| io::Error::other("shell child process id does not fit in i32"))?;
    let result = unsafe {
        // SAFETY: `pid` is the live child owned by the caller and `signal` was
        // captured from the attach termination handler.
        libc::kill(pid, signal)
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::wait_for_shell_child;

    #[test]
    fn shell_child_wait_is_bounded_after_hup_or_term() {
        for signal in [libc::SIGHUP, libc::SIGTERM] {
            let mut child = Command::new("sh")
                .args(["-c", "exec sleep 60"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn sleeping shell child");
            let requested = Arc::new(AtomicI32::new(0));
            let request_from_thread = Arc::clone(&requested);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(40));
                request_from_thread.store(signal, Ordering::SeqCst);
            });

            let started = Instant::now();
            let error = wait_for_shell_child(&mut child, || {
                let signal = requested.load(Ordering::SeqCst);
                (signal != 0).then_some(signal)
            })
            .expect_err("termination must interrupt the shell wait");

            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "shell wait should not remain blocked after signal {signal}"
            );
            assert!(
                child.try_wait().expect("query child status").is_some(),
                "the interrupted shell child should be reaped"
            );
        }
    }

    #[test]
    fn shell_child_wait_force_kills_a_child_that_ignores_term() {
        let mut child = Command::new("sh")
            .args(["-c", "trap '' TERM; exec sleep 60"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn signal-ignoring shell child");
        std::thread::sleep(Duration::from_millis(40));

        let started = Instant::now();
        let error = wait_for_shell_child(&mut child, || Some(libc::SIGTERM))
            .expect_err("ignored termination must still bound the shell wait");

        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            child.try_wait().expect("query child status").is_some(),
            "the force-killed shell child should be reaped"
        );
    }
}
