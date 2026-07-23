use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::time::{Duration, Instant};

pub(crate) fn read(fd: BorrowedFd<'_>, buffer: &mut [u8]) -> io::Result<usize> {
    loop {
        match try_read(fd, buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => wait_until_readable(fd)?,
            result => return result,
        }
    }
}

pub(crate) fn try_read(fd: BorrowedFd<'_>, buffer: &mut [u8]) -> io::Result<usize> {
    rustix::io::read(fd, buffer).map_err(io::Error::from)
}

pub(crate) fn write_all_with_timeout(
    fd: BorrowedFd<'_>,
    mut buffer: &[u8],
    timeout: Duration,
) -> io::Result<()> {
    let mut last_progress = Instant::now();
    while !buffer.is_empty() {
        if last_progress.elapsed() >= timeout {
            return Err(write_ready_timeout(timeout));
        }
        match rustix::io::write(fd, buffer) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0")),
            Ok(bytes_written) => {
                buffer = &buffer[bytes_written..];
                last_progress = Instant::now();
            }
            Err(rustix::io::Errno::INTR) => continue,
            Err(rustix::io::Errno::AGAIN) => wait_until_writable(fd, last_progress, timeout)?,
            Err(error) => return Err(error.into()),
        }
    }

    Ok(())
}

pub(crate) fn try_write_immediate(fd: BorrowedFd<'_>, buffer: &[u8]) -> io::Result<usize> {
    let mut written = 0;
    while written < buffer.len() {
        match rustix::io::write(fd, &buffer[written..]) {
            Ok(0) => {
                if written == 0 {
                    return Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0"));
                }
                return Ok(written);
            }
            Ok(bytes_written) => written += bytes_written,
            Err(rustix::io::Errno::INTR) => continue,
            Err(rustix::io::Errno::AGAIN) => return Ok(written),
            Err(error) => return Err(error.into()),
        }
    }

    Ok(written)
}

fn wait_until_writable(
    fd: BorrowedFd<'_>,
    last_progress: Instant,
    timeout: Duration,
) -> io::Result<()> {
    loop {
        let remaining = timeout.saturating_sub(last_progress.elapsed());
        if remaining.is_zero() {
            return Err(write_ready_timeout(timeout));
        }
        let mut poll_fd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: `poll_fd` points to one initialized pollfd entry and the
        // borrowed fd stays valid for the duration of this blocking call.
        let ready = unsafe { libc::poll(&mut poll_fd, 1, poll_timeout_ms(remaining)) };
        if ready > 0 {
            if poll_fd.revents & libc::POLLOUT != 0 {
                return Ok(());
            }
            if poll_fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "pty is no longer writable",
                ));
            }
            continue;
        }
        if ready == 0 {
            return Err(write_ready_timeout(timeout));
        }

        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
    }
}

fn wait_until_readable(fd: BorrowedFd<'_>) -> io::Result<()> {
    loop {
        let mut poll_fd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `poll_fd` points to one initialized pollfd entry and the
        // borrowed fd stays valid for the duration of this blocking call.
        let ready = unsafe { libc::poll(&mut poll_fd, 1, -1) };
        if ready > 0 {
            return Ok(());
        }
        if ready == 0 {
            continue;
        }

        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
    }
}

fn poll_timeout_ms(remaining: Duration) -> libc::c_int {
    let millis = remaining.as_millis().max(1);
    i32::try_from(millis).unwrap_or(i32::MAX)
}

fn write_ready_timeout(timeout: Duration) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("pty write made no progress for {} ms", timeout.as_millis()),
    )
}
