use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd, RawFd};
use std::sync::Mutex;
use std::time::Duration;

use rustix::fs::{fcntl_getfl, fcntl_setfl, open, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::{
    getpid, ioctl_tiocsctty, kill_process as rustix_kill_process, kill_process_group, setsid,
};
use rustix::pty::{grantpt, ptsname, unlockpt};
use rustix::termios::{tcgetwinsize, tcsetpgrp, tcsetwinsize};

use super::macos_grant::{grant_or_escape_stall, Grant};
use super::unix_io;
use crate::{size, ProcessId, Result, Signal, TerminalGeometry, TerminalSize};

// Concurrent Darwin PTY allocations can race while allocating a minor number
// and expose the kernel's internal EREDRIVEOPEN error. Serialize allocation
// within this process; descriptor setup and child spawning remain independent.
// The lock cannot see other processes allocating PTYs at the same time, so
// that leaked errno is retried a bounded number of times below.
static PTY_ALLOCATION_LOCK: Mutex<()> = Mutex::new(());

// xnu's private EREDRIVEOPEN ("redrive the open"). The kernel already redrives
// it internally and only leaks it to userspace when that budget is exhausted
// under cross-process contention; any other errno is returned at once.
const DARWIN_EREDRIVEOPEN: i32 = -6;
// Total allocation attempts per pair. Immediate retries, no wall-clock delay.
const PTY_ALLOCATION_ATTEMPTS: usize = 8;

// Both ends are opened close-on-exec. Darwin's `openpty` returns inheritable
// descriptors, and a child that another thread starts before they are marked
// (`run-shell`, a status job, a copy pipe) keeps the pane's master and slave
// for its whole life: it can read the pane's output and write its input.
const PTY_OPEN_FLAGS: OFlags = OFlags::RDWR.union(OFlags::NOCTTY).union(OFlags::CLOEXEC);

pub(crate) fn open_pty_pair() -> Result<(OwnedFd, OwnedFd)> {
    let _allocation_guard = PTY_ALLOCATION_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    open_pty_pair_retrying_redrive()
}

/// Why one allocation attempt produced no pair.
enum AttemptFailure {
    Os(Errno),
    /// The grant kept restarting in the kernel; see `macos_grant`.
    GrantStalled,
}

impl From<Errno> for AttemptFailure {
    fn from(errno: Errno) -> Self {
        Self::Os(errno)
    }
}

/// Allocates one pair, retrying only the leaked Darwin EREDRIVEOPEN errno and
/// a stalled grant up to [`PTY_ALLOCATION_ATTEMPTS`] times. A final errno is
/// returned unchanged; a final stalled grant is reported as `ETIMEDOUT`.
fn open_pty_pair_retrying_redrive() -> Result<(OwnedFd, OwnedFd)> {
    let mut attempt = 1;
    loop {
        let retryable = match open_pty_pair_once() {
            Ok(pair) => return Ok(pair),
            Err(AttemptFailure::Os(errno)) => {
                if errno.raw_os_error() != DARWIN_EREDRIVEOPEN {
                    return Err(errno.into());
                }
                errno
            }
            Err(AttemptFailure::GrantStalled) => Errno::TIMEDOUT,
        };
        if attempt == PTY_ALLOCATION_ATTEMPTS {
            return Err(retryable.into());
        }
        attempt += 1;
    }
}

/// The steps of Darwin's `openpty`, with both descriptors close-on-exec from
/// the system call that creates them.
fn open_pty_pair_once() -> std::result::Result<(OwnedFd, OwnedFd), AttemptFailure> {
    let master = open(c"/dev/ptmx", PTY_OPEN_FLAGS, Mode::empty())?;
    let grant = grant_or_escape_stall(master.as_fd(), || grantpt(&master))?;
    if grant == Grant::Stalled {
        // The swap closed this attempt's master and its minor; `master` now
        // names /dev/null.
        return Err(AttemptFailure::GrantStalled);
    }
    unlockpt(&master)?;
    let slave_name = ptsname(&master, Vec::new())?;
    let slave = open(slave_name.as_c_str(), PTY_OPEN_FLAGS, Mode::empty())?;
    Ok((master, slave))
}

pub(crate) fn query_size(fd: BorrowedFd<'_>) -> Result<TerminalSize> {
    Ok(size::from_winsize(tcgetwinsize(fd)?))
}

pub(crate) fn apply_size(fd: BorrowedFd<'_>, size: TerminalSize) -> Result<()> {
    tcsetwinsize(fd, size::into_winsize(size))?;
    Ok(())
}

pub(crate) fn apply_geometry(fd: BorrowedFd<'_>, geometry: TerminalGeometry) -> Result<()> {
    tcsetwinsize(fd, size::into_winsize_geometry(geometry))?;
    Ok(())
}

pub(crate) fn setup_child_controlling_terminal(raw_master_fd: RawFd) -> std::io::Result<()> {
    // SAFETY: This closes only the child process' inherited copy of the PTY
    // master fd. The parent still owns its separate descriptor.
    unsafe { rustix::io::close(raw_master_fd) };

    setsid().map_err(std::io::Error::from)?;

    // SAFETY: `stdin` has already been wired to the PTY slave by `Command`, so
    // fd 0 is a valid borrowed descriptor for the rest of the pre-exec setup.
    let slave_stdin = unsafe { BorrowedFd::borrow_raw(0) };
    ioctl_tiocsctty(slave_stdin).map_err(std::io::Error::from)?;
    tcsetpgrp(slave_stdin, getpid()).map_err(std::io::Error::from)?;

    Ok(())
}

pub(crate) fn kill_foreground_process_group(pid: ProcessId, signal: Signal) -> Result<()> {
    kill_process_group(pid.as_rustix_pid()?, signal.as_rustix_signal())?;
    Ok(())
}

pub(crate) fn kill_process(pid: ProcessId, signal: Signal) -> Result<()> {
    rustix_kill_process(pid.as_rustix_pid()?, signal.as_rustix_signal())?;
    Ok(())
}

pub(crate) fn stopped_signal(pid: ProcessId) -> Result<Option<i32>> {
    let mut info = MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: `info` points to writable storage for one siginfo_t. WNOWAIT
    // observes the stopped status without consuming the child's eventual exit
    // status, which remains owned by `std::process::Child`.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid.as_u32() as libc::id_t,
            info.as_mut_ptr(),
            libc::WSTOPPED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        let errno = last_errno();
        if errno == rustix::io::Errno::CHILD {
            return Ok(None);
        }
        return Err(errno.into());
    }

    // SAFETY: `info` was zero-initialized before the call and `waitid`
    // returned success, so reading the initialized siginfo_t is valid.
    let info = unsafe { info.assume_init() };
    // SAFETY: `waitid` with WSTOPPED populates the SIGCHLD status field when
    // a stopped child is available. A zero status means WNOHANG had no event.
    let status = unsafe { info.si_status() };
    if status == 0 {
        Ok(None)
    } else {
        Ok(Some(status))
    }
}

pub(crate) fn read(fd: BorrowedFd<'_>, buffer: &mut [u8]) -> io::Result<usize> {
    unix_io::read(fd, buffer)
}

pub(crate) fn try_read(fd: BorrowedFd<'_>, buffer: &mut [u8]) -> io::Result<usize> {
    unix_io::try_read(fd, buffer)
}

pub(crate) fn write_all_with_timeout(
    fd: BorrowedFd<'_>,
    buffer: &[u8],
    timeout: Duration,
) -> io::Result<()> {
    unix_io::write_all_with_timeout(fd, buffer, timeout)
}

pub(crate) fn try_write_immediate(fd: BorrowedFd<'_>, buffer: &[u8]) -> io::Result<usize> {
    unix_io::try_write_immediate(fd, buffer)
}

pub(crate) fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    let flags = fcntl_getfl(fd).map_err(io::Error::other)?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK).map_err(io::Error::other)?;
    Ok(())
}

fn last_errno() -> rustix::io::Errno {
    let raw = std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO);
    rustix::io::Errno::from_raw_os_error(raw)
}

#[cfg(test)]
#[path = "macos_tests.rs"]
mod tests;
