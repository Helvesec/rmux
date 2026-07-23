use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};

use super::{
    ensure_not_cancelled, ensure_path_identity, CancellationState, FileIdentity, PHASE_OPENED,
};

const INTERNAL_FIFO_READER_FLAG: &str = "--__internal-fifo-reader";
const HELPER_IDENTITY_EXIT_CODE: i32 = 65;
const HELPER_USAGE_EXIT_CODE: i32 = 64;
const HELPER_IO_EXIT_CODE: i32 = 74;
const HELPER_POLL_TIMEOUT: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 5_000_000,
};
const FIFO_EOF_RECHECK_TIMEOUT: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 100_000_000,
};
static FIFO_HELPER_HOST_ENABLED: AtomicBool = AtomicBool::new(false);

pub(super) fn try_read_fifo(state: &CancellationState) -> Option<io::Result<Vec<u8>>> {
    if let Err(error) = ensure_not_cancelled(state) {
        return Some(Err(error));
    }
    if let Err(error) = ensure_path_identity(state) {
        return Some(Err(error));
    }

    let executable = match helper_executable() {
        Ok(Some(executable)) => executable,
        Ok(None) => return None,
        Err(error) => return Some(Err(error)),
    };
    Some(read_fifo_with_helper(state, executable))
}

fn read_fifo_with_helper(state: &CancellationState, executable: PathBuf) -> io::Result<Vec<u8>> {
    let mut command = Command::new(executable);
    command
        .arg(INTERNAL_FIFO_READER_FLAG)
        .arg(&state.path)
        .arg(state.special.identity.device.to_string())
        .arg(state.special.identity.inode.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = HelperChild::spawn(&mut command)?;
    let mut output = child.take_stdout()?;
    let output_flags = fcntl_getfl(&output).map_err(errno_to_io)?;
    fcntl_setfl(&output, output_flags | OFlags::NONBLOCK).map_err(errno_to_io)?;
    state.phase.store(PHASE_OPENED, Ordering::Release);

    let mut content = Vec::new();
    loop {
        if let Err(error) = ensure_not_cancelled(state) {
            child.terminate();
            return Err(error);
        }

        drain_available(&mut output, &mut content)?;
        if let Some(status) = child.try_wait()? {
            fcntl_setfl(&output, output_flags).map_err(errno_to_io)?;
            output.read_to_end(&mut content)?;
            return finish_helper(child, status, content);
        }

        let mut descriptors = [PollFd::new(
            &output,
            PollFlags::IN | PollFlags::HUP | PollFlags::ERR,
        )];
        match poll(&mut descriptors, Some(&HELPER_POLL_TIMEOUT)) {
            Ok(_) => {}
            Err(error) if error == rustix::io::Errno::INTR => {}
            Err(error) => {
                child.terminate();
                return Err(errno_to_io(error));
            }
        }
    }
}

pub(super) fn run_helper_if_requested<I>(arguments: I) -> Option<i32>
where
    I: IntoIterator<Item = OsString>,
{
    // Calling this entrypoint on normal startup is the capability handshake:
    // it proves that re-executing the current binary can consume the private
    // helper flag. Published rmux-server embedders can opt in without relying
    // on an executable name or an adjacent RMUX installation.
    FIFO_HELPER_HOST_ENABLED.store(true, Ordering::Release);
    let mut arguments = arguments.into_iter();
    if arguments.next().as_deref() != Some(OsStr::new(INTERNAL_FIFO_READER_FLAG)) {
        return None;
    }

    let exit_code = match parse_helper_arguments(&mut arguments).and_then(run_helper) {
        Ok(()) => 0,
        Err(HelperError::Usage(message)) => {
            let _ = writeln!(io::stderr().lock(), "{message}");
            HELPER_USAGE_EXIT_CODE
        }
        Err(HelperError::Identity(message)) => {
            let _ = writeln!(io::stderr().lock(), "{message}");
            HELPER_IDENTITY_EXIT_CODE
        }
        Err(HelperError::Io(error)) => {
            let _ = writeln!(io::stderr().lock(), "{error}");
            HELPER_IO_EXIT_CODE
        }
    };
    Some(exit_code)
}

#[derive(Debug)]
struct HelperArguments {
    path: PathBuf,
    expected_identity: FileIdentity,
}

#[derive(Debug)]
enum HelperError {
    Usage(String),
    Identity(String),
    Io(io::Error),
}

fn parse_helper_arguments(
    arguments: &mut impl Iterator<Item = OsString>,
) -> Result<HelperArguments, HelperError> {
    let path = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| HelperError::Usage("missing internal FIFO path".to_owned()))?;
    let device = parse_identity_component(arguments.next(), "device")?;
    let inode = parse_identity_component(arguments.next(), "inode")?;
    if arguments.next().is_some() {
        return Err(HelperError::Usage(
            "unexpected internal FIFO reader argument".to_owned(),
        ));
    }
    Ok(HelperArguments {
        path,
        expected_identity: FileIdentity { device, inode },
    })
}

fn parse_identity_component(value: Option<OsString>, name: &str) -> Result<u64, HelperError> {
    value
        .and_then(|value| value.into_string().ok())
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| HelperError::Usage(format!("invalid internal FIFO {name}")))
}

fn run_helper(arguments: HelperArguments) -> Result<(), HelperError> {
    let mut fifo = File::open(&arguments.path).map_err(HelperError::Io)?;
    validate_fifo_identity(
        &arguments.path,
        &fifo.metadata().map_err(HelperError::Io)?,
        arguments.expected_identity,
    )?;
    copy_fifo_nonblocking(&mut fifo, &mut io::stdout().lock()).map_err(HelperError::Io)
}

fn copy_fifo_nonblocking(fifo: &mut File, output: &mut impl Write) -> io::Result<()> {
    set_fifo_nonblocking(fifo)?;

    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match fifo.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(length) => output.write_all(&buffer[..length])?,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_for_fifo_input(fifo)?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

fn set_fifo_nonblocking(fifo: &File) -> io::Result<()> {
    let flags = fcntl_getfl(fifo).map_err(errno_to_io)?;
    fcntl_setfl(fifo, flags | OFlags::NONBLOCK).map_err(errno_to_io)
}

fn wait_for_fifo_input(fifo: &File) -> io::Result<()> {
    let mut descriptors = [PollFd::new(
        fifo,
        PollFlags::IN | PollFlags::HUP | PollFlags::ERR,
    )];
    // Darwin does not reliably report POLLHUP when the last writer closes
    // immediately. Re-probe the nonblocking descriptor at a low idle rate so
    // a missed hangup still becomes an observable EOF instead of a deadlock.
    match poll(&mut descriptors, Some(&FIFO_EOF_RECHECK_TIMEOUT)) {
        Ok(_) => Ok(()),
        Err(error) if error == rustix::io::Errno::INTR => Ok(()),
        Err(error) => Err(errno_to_io(error)),
    }
}

fn validate_fifo_identity(
    path: &Path,
    metadata: &Metadata,
    expected: FileIdentity,
) -> Result<(), HelperError> {
    let actual = FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    if metadata.file_type().is_fifo() && actual == expected {
        Ok(())
    } else {
        Err(HelperError::Identity(format!(
            "buffer FIFO '{}' changed during blocking open",
            path.display()
        )))
    }
}

fn helper_executable() -> io::Result<Option<PathBuf>> {
    if FIFO_HELPER_HOST_ENABLED.load(Ordering::Acquire) {
        return env::current_exe().map(Some);
    }
    Ok(None)
}

fn drain_available(output: &mut impl Read, content: &mut Vec<u8>) -> io::Result<()> {
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match output.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(length) => content.extend_from_slice(&buffer[..length]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

fn finish_helper(
    mut child: HelperChild,
    status: ExitStatus,
    content: Vec<u8>,
) -> io::Result<Vec<u8>> {
    let diagnostic = child.read_stderr();
    child.disarm();
    if status.success() {
        return Ok(content);
    }

    let message = if diagnostic.trim().is_empty() {
        format!("macOS FIFO reader helper exited with {status}")
    } else {
        diagnostic.trim().to_owned()
    };
    let kind = if status.code() == Some(HELPER_IDENTITY_EXIT_CODE) {
        io::ErrorKind::InvalidInput
    } else {
        io::ErrorKind::Other
    };
    Err(io::Error::new(kind, message))
}

struct HelperChild {
    child: Child,
    armed: bool,
}

impl HelperChild {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        command.spawn().map(|child| Self { child, armed: true })
    }

    fn take_stdout(&mut self) -> io::Result<std::process::ChildStdout> {
        self.child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("macOS FIFO reader helper stdout was not captured"))
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    fn read_stderr(&mut self) -> String {
        let mut diagnostic = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut diagnostic);
        }
        diagnostic
    }

    fn terminate(&mut self) {
        if self.armed {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.armed = false;
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for HelperChild {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn errno_to_io(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::fs::Mode;

    #[test]
    fn helper_configures_fifo_reads_as_nonblocking() {
        let path = env::temp_dir().join(format!(
            "rmux-darwin-fifo-nonblocking-read-{}",
            std::process::id()
        ));
        let output = Command::new("mkfifo")
            .arg(&path)
            .output()
            .expect("run mkfifo");
        assert!(output.status.success(), "mkfifo failed: {output:?}");
        let descriptor = rustix::fs::open(
            &path,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .expect("open nonblocking FIFO reader");
        let fifo = File::from(descriptor);
        let flags = fcntl_getfl(&fifo).expect("inspect initial FIFO flags");
        fcntl_setfl(&fifo, flags - OFlags::NONBLOCK).expect("clear nonblocking FIFO flag");

        set_fifo_nonblocking(&fifo).expect("configure nonblocking FIFO reader");

        let flags = fcntl_getfl(&fifo).expect("inspect configured FIFO flags");
        assert!(flags.contains(OFlags::NONBLOCK));
        std::fs::remove_file(path).expect("remove test FIFO");
    }
}
