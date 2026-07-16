use std::error::Error as StdError;
use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rmux_os::process_tree::ProcessTreeChild;
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
    let mut child = ProcessTreeChild::spawn(&mut process).map_err(AttachError::Io)?;
    let foreground = child.foreground_terminal(fd).map_err(AttachError::Io)?;
    let wait_result = wait_for_shell_child(&mut child, termination::requested_signal);
    let restore_result = foreground.restore();
    if let Err(error) = wait_result {
        restore_result?;
        return Err(AttachError::Io(error));
    }
    restore_result?;
    Ok(())
}

fn wait_for_shell_child(
    child: &mut ProcessTreeChild,
    requested_signal: impl Fn() -> Option<i32>,
) -> io::Result<()> {
    loop {
        if child.has_exited()? {
            child.wait()?;
            return Ok(());
        }
        if child.has_stopped()? {
            terminate_stopped_shell_tree(child)?;
            return Ok(());
        }
        let Some(signal) = requested_signal() else {
            thread::sleep(SHELL_CHILD_WAIT_INTERVAL);
            continue;
        };

        child.forward_signal(signal).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to forward attach interruption to shell tree: {error}"),
            )
        })?;
        let _ = wait_for_child_exit_until(child, Instant::now() + SHELL_CHILD_TERMINATION_GRACE)?;

        // Keep the exited Unix group leader waitable until after the force
        // signal. That prevents its PGID from being recycled while also
        // terminating descendants that ignored the forwarded signal.
        child.terminate().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to terminate interrupted shell tree: {error}"),
            )
        })?;
        let _ = wait_for_child_exit_until(child, Instant::now() + SHELL_CHILD_TERMINATION_GRACE);
        child.wait().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to reap interrupted shell tree: {error}"),
            )
        })?;
        return Err(termination::interruption_error());
    }
}

fn terminate_stopped_shell_tree(child: &mut ProcessTreeChild) -> io::Result<()> {
    child.terminate().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to terminate stopped shell tree: {error}"),
        )
    })?;
    child.wait().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to reap stopped shell tree: {error}"),
        )
    })?;
    Ok(())
}

fn wait_for_child_exit_until(child: &mut ProcessTreeChild, deadline: Instant) -> io::Result<bool> {
    while Instant::now() < deadline {
        if child.has_exited()? {
            return Ok(true);
        }
        thread::sleep(SHELL_CHILD_WAIT_INTERVAL);
    }
    child.has_exited()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use rmux_os::process_tree::ProcessTreeChild;

    use super::wait_for_shell_child;

    fn spawn_shell(script: &str) -> ProcessTreeChild {
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ProcessTreeChild::spawn(&mut command).expect("spawn isolated shell child")
    }

    #[test]
    fn shell_child_wait_is_bounded_after_hup_or_term() {
        for signal in [libc::SIGHUP, libc::SIGTERM] {
            let mut child = spawn_shell("exec sleep 60");
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

            assert_eq!(
                error.kind(),
                std::io::ErrorKind::Interrupted,
                "unexpected shell wait error after signal {signal}: {error}"
            );
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "shell wait should not remain blocked after signal {signal}"
            );
            assert!(
                child.has_exited().expect("query child status"),
                "the interrupted shell child should be reaped"
            );
        }
    }

    #[test]
    fn shell_child_wait_force_kills_a_child_that_ignores_term() {
        let mut child = spawn_shell("trap '' TERM; exec sleep 60");
        std::thread::sleep(Duration::from_millis(40));

        let started = Instant::now();
        let error = wait_for_shell_child(&mut child, || Some(libc::SIGTERM))
            .expect_err("ignored termination must still bound the shell wait");

        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            child.has_exited().expect("query child status"),
            "the force-killed shell child should be reaped"
        );
    }

    #[test]
    fn shell_child_wait_terminates_signal_ignoring_descendants() {
        let pid_file = unique_descendant_pid_file();
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "sh -c 'trap \"\" TERM; exec sleep 60' & printf '%s' \"$!\" > \"$RMUX_DESCENDANT_PID\"; wait",
            ])
            .env("RMUX_DESCENDANT_PID", &pid_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = ProcessTreeChild::spawn(&mut command).expect("spawn shell process tree");

        let descendant_pid = wait_for_descendant_pid(&pid_file);
        let cleanup = DescendantCleanup {
            pid: descendant_pid,
            pid_file,
        };
        let error = wait_for_shell_child(&mut child, || Some(libc::SIGTERM))
            .expect_err("termination must interrupt the complete shell process tree");

        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_exists(descendant_pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !process_exists(descendant_pid),
            "signal-ignoring descendant {descendant_pid} survived attach interruption"
        );
        drop(cleanup);
    }

    #[test]
    fn shell_child_wait_preserves_background_descendants_after_normal_exit() {
        let pid_file = unique_descendant_pid_file();
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "sleep 60 </dev/null >/dev/null 2>&1 & printf '%s' \"$!\" > \"$RMUX_DESCENDANT_PID\"",
            ])
            .env("RMUX_DESCENDANT_PID", &pid_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = ProcessTreeChild::spawn(&mut command).expect("spawn shell process tree");

        let descendant_pid = wait_for_descendant_pid(&pid_file);
        let cleanup = DescendantCleanup {
            pid: descendant_pid,
            pid_file,
        };
        wait_for_shell_child(&mut child, || None).expect("foreground shell should exit normally");

        assert!(
            process_exists(descendant_pid),
            "normal shell completion must preserve an intentional background descendant"
        );
        drop(cleanup);
    }

    struct DescendantCleanup {
        pid: i32,
        pid_file: PathBuf,
    }

    impl Drop for DescendantCleanup {
        fn drop(&mut self) {
            unsafe {
                // SAFETY: The test owns the spawned descendant PID and uses a
                // force signal only as failure-path cleanup.
                libc::kill(self.pid, libc::SIGKILL);
            }
            let _ = std::fs::remove_file(&self.pid_file);
        }
    }

    fn unique_descendant_pid_file() -> PathBuf {
        std::env::temp_dir().join(format!(
            "rmux-attach-descendant-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ))
    }

    fn wait_for_descendant_pid(pid_file: &PathBuf) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(contents) = std::fs::read_to_string(pid_file) {
                if let Ok(pid) = contents.parse() {
                    return pid;
                }
            }
            assert!(
                Instant::now() < deadline,
                "shell did not publish its descendant pid"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn process_exists(pid: i32) -> bool {
        let result = unsafe {
            // SAFETY: Signal zero performs an existence check without changing
            // the target process.
            libc::kill(pid, 0)
        };
        result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}

#[cfg(all(
    test,
    not(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))
))]
#[path = "terminal_stopped_tests.rs"]
mod stopped_tests;
