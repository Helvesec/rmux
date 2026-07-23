use std::os::fd::{BorrowedFd, OwnedFd, RawFd};

use std::io;
use std::mem::MaybeUninit;
use std::time::Duration;

use rustix::fs::{fcntl_getfl, fcntl_setfl, open, Mode, OFlags};
use rustix::process::{
    getpid, ioctl_tiocsctty, kill_process as rustix_kill_process, kill_process_group, setsid,
};
use rustix::pty::{grantpt, ioctl_tiocgptpeer, openpt, ptsname, unlockpt, OpenptFlags};
use rustix::termios::{tcgetwinsize, tcsetpgrp, tcsetwinsize};

use super::unix_io;
use crate::{size, ProcessId, Result, Signal, TerminalGeometry, TerminalSize};

pub(crate) fn open_pty_pair() -> Result<(OwnedFd, OwnedFd)> {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;

    let slave = open_slave(&master)?;

    Ok((master, slave))
}

fn open_slave(master: &OwnedFd) -> Result<OwnedFd> {
    match ioctl_tiocgptpeer(
        master,
        OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC,
    ) {
        Ok(slave) => Ok(slave),
        Err(peer_error) => open_slave_by_name(master).map_err(|_| peer_error.into()),
    }
}

fn open_slave_by_name(master: &OwnedFd) -> Result<OwnedFd> {
    let slave_name = ptsname(master, Vec::new())?;
    Ok(open(
        slave_name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
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
    // SAFETY: `waitid` with WSTOPPED populates `si_pid` when a stopped child
    // is available. A zero pid means WNOHANG had no event.
    let si_pid = unsafe { info.si_pid() };
    if si_pid == 0 {
        Ok(None)
    } else {
        // SAFETY: a non-zero `si_pid` means this siginfo_t carries a real
        // child status for the requested child.
        Ok(Some(unsafe { info.si_status() }))
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
