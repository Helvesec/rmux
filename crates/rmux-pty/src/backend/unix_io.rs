use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

/// Linux wakes a PTY master sleeping in `poll(POLLOUT)` only when the slave's
/// line-discipline buffer drains below its unthrottle threshold, not each time
/// the child consumes input and frees room. A child that drains slowly thus
/// makes room long before any wakeup, so a blocked writer re-tries the write at
/// this cadence: progress is measured on the queue itself, not on one wakeup.
const WRITE_ROOM_RECHECK_INTERVAL: Duration = Duration::from_millis(10);

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
    let remaining = timeout.saturating_sub(last_progress.elapsed());
    if remaining.is_zero() {
        return Err(write_ready_timeout(timeout));
    }
    wait_for_write_room(fd, remaining.min(WRITE_ROOM_RECHECK_INTERVAL))
}

/// Waits at most `wait` for the PTY to report write room. Returning `Ok` does
/// not prove room: the caller re-tries the write. Fails once the PTY can no
/// longer be written.
fn wait_for_write_room(fd: BorrowedFd<'_>, wait: Duration) -> io::Result<()> {
    loop {
        let mut poll_fd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: `poll_fd` points to one initialized pollfd entry and the
        // borrowed fd stays valid for the duration of this blocking call.
        let ready = unsafe { libc::poll(&mut poll_fd, 1, poll_timeout_ms(wait)) };
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
            // A silent poll does not prove the queue is still full: let the
            // caller re-try the write; a bounded writer owns its deadline.
            return Ok(());
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

/// The ordered input backlog of one Unix PTY master, shared by every clone of
/// its endpoint.
///
/// The kernel takes only what fits in the slave's input buffer, a few KiB.
/// tmux keeps the rest in a per-pane buffer and writes it whenever the fd
/// becomes writable, however long the program on the slave stays busy, so a
/// paste into a shell that is still running a command arrives whole. This
/// backlog does the same: [`Self::push`] never waits, and while bytes remain
/// one drainer thread writes them in order. Nothing is dropped while the PTY
/// accepts input; the backlog is abandoned only once the PTY can no longer be
/// written or every endpoint of it is closed.
#[derive(Debug, Default)]
pub(crate) struct PtyInputQueue {
    state: Mutex<PtyInputBacklog>,
}

#[derive(Debug, Default)]
struct PtyInputBacklog {
    /// Bytes accepted from callers and not yet accepted by the kernel.
    pending: VecDeque<u8>,
    /// Whether a drainer thread owns `pending`. While false, `pending` is
    /// empty and the next push may write directly.
    draining: bool,
}

impl PtyInputQueue {
    /// Appends `bytes` after every earlier push and returns without waiting
    /// for the program on the slave to read them.
    ///
    /// Fails only when the PTY already refuses input, for example because its
    /// slave is gone, or when no drainer thread can be started; nothing of
    /// `bytes` is kept in either case.
    pub(crate) fn push(self: &Arc<Self>, fd: &Arc<OwnedFd>, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut backlog = self.lock();
        if backlog.draining {
            backlog.pending.extend(bytes);
            return Ok(());
        }
        let written = try_write_immediate(fd.as_fd(), bytes)?;
        if written == bytes.len() {
            return Ok(());
        }
        backlog.pending.extend(&bytes[written..]);
        backlog.draining = true;
        let queue = Arc::clone(self);
        let fd = Arc::downgrade(fd);
        let spawned = std::thread::Builder::new()
            .name("rmux-pty-input".to_owned())
            .spawn(move || queue.drain(&fd));
        if let Err(error) = spawned {
            backlog.pending.clear();
            backlog.draining = false;
            return Err(error);
        }
        Ok(())
    }

    /// Writes the backlog until it is empty. The drainer holds the fd only
    /// weakly between attempts, so closing the last endpoint closes the PTY
    /// and ends the drainer instead of being kept open by it.
    fn drain(&self, fd: &Weak<OwnedFd>) {
        loop {
            let Some(fd) = fd.upgrade() else {
                return self.abandon();
            };
            match self.write_pending(fd.as_fd()) {
                Ok(true) => return,
                Ok(false) => {}
                Err(_) => return self.abandon(),
            }
            if wait_for_write_room(fd.as_fd(), WRITE_ROOM_RECHECK_INTERVAL).is_err() {
                return self.abandon();
            }
        }
    }

    /// Writes from the front of the backlog until it is empty (`true`) or the
    /// kernel takes no more (`false`). An empty backlog clears `draining` in
    /// the same critical section, so a concurrent push either appends behind
    /// this drainer or writes directly after it, never before its bytes.
    fn write_pending(&self, fd: BorrowedFd<'_>) -> io::Result<bool> {
        let mut backlog = self.lock();
        loop {
            if backlog.pending.is_empty() {
                backlog.draining = false;
                return Ok(true);
            }
            let (front, _) = backlog.pending.as_slices();
            match try_write_immediate(fd, front)? {
                0 => return Ok(false),
                written => drop(backlog.pending.drain(..written)),
            }
        }
    }

    fn abandon(&self) {
        let mut backlog = self.lock();
        backlog.pending.clear();
        backlog.draining = false;
    }

    fn lock(&self) -> MutexGuard<'_, PtyInputBacklog> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
