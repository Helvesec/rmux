use std::error::Error as StdError;
use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rmux_proto::AttachShellCommand;
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use rustix::process::{kill_process, Pid, Signal};
use rustix::termios::{
    tcflush, tcgetattr, tcsetattr, OptionalActions, QueueSelector, SpecialCodeIndex, Termios,
};

use super::terminal_cleanup::fallback_attach_stop_sequence;

const TERMINATION_OUTPUT_RETRY: Duration = Duration::from_millis(100);
const TERMINATION_OUTPUT_RETRY_INTERVAL: Duration = Duration::from_millis(1);

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
    process.status().map_err(AttachError::Io)?;
    Ok(())
}
