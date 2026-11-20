//! Darwin `grantpt` that cannot spin forever in the kernel.
//!
//! xnu answers `TIOCPTYGRANT` with `ERESTART` while the pair has no replica
//! devfs node (`_devfs_setattr` in `bsd/kern/tty_dev.c`). That node is missing
//! for good when `devfs_make_node` lost a name collision with the previous
//! owner of the same minor: its last close frees the minor before it removes
//! the old `/dev/ttysNNN` node (`ptmx_free_ioctl` in `bsd/kern/tty_ptmx.c`).
//! The kernel then re-executes the ioctl forever. The calling thread burns a
//! CPU in system time and, holding the allocation lock, blocks every later
//! pane of the process.
//!
//! Each restart looks the descriptor number up again. A watchdog therefore
//! replaces a stalled master with `/dev/null`: the restarted ioctl fails
//! (`EBADF` during the swap, `ENOTTY` after it), and the caller discards the
//! pair. The swap closed the master, which frees its minor.
//!
//! One watchdog thread serves the process. A grant only arms and disarms it
//! under a mutex, so a healthy allocation never waits for another thread to
//! be scheduled.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use rustix::fs::{open, Mode, OFlags};
use rustix::io::{fcntl_setfd, Errno, FdFlags};

/// A healthy grant is a single ioctl; across 16 614 grants on a host at load
/// 50, the longest took 288 ms. Only a kernel that keeps restarting it, or a
/// thread descheduled this long, reaches the limit; both cost one retry.
const GRANT_STALL_LIMIT: Duration = Duration::from_secs(2);

/// How one grant attempt ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Grant {
    Granted,
    /// The watchdog replaced the master with `/dev/null`; discard the pair.
    Stalled,
}

/// Runs `grant` on `master` under the process watchdog. `grant` must operate
/// on `master`'s descriptor number. Without a watchdog thread the grant runs
/// unwatched, as it did before the watchdog existed.
pub(super) fn grant_or_escape_stall(
    master: BorrowedFd<'_>,
    grant: impl FnOnce() -> Result<(), Errno>,
) -> Result<Grant, Errno> {
    static WATCHDOG: OnceLock<Option<&'static GrantWatchdog>> = OnceLock::new();
    let watchdog = WATCHDOG.get_or_init(|| match GrantWatchdog::spawn(GRANT_STALL_LIMIT) {
        Ok(watchdog) => Some(watchdog),
        Err(error) => {
            tracing::warn!(%error, "pty grant watchdog unavailable; granting unwatched");
            None
        }
    });
    match watchdog {
        Some(watchdog) => watchdog.watch(master, grant),
        None => grant().map(|()| Grant::Granted),
    }
}

/// A grant in progress. Its descriptor stays owned by the caller until the
/// caller disarms it, which needs the watchdog lock.
struct ArmedGrant {
    ticket: u64,
    master: RawFd,
    deadline: Instant,
    escaped: bool,
}

#[derive(Default)]
struct Armed {
    next_ticket: u64,
    grants: Vec<ArmedGrant>,
}

/// Disarms on drop, so an unwinding grant cannot leave its descriptor number
/// armed after the caller closed it.
struct Watch<'a> {
    watchdog: &'a GrantWatchdog,
    ticket: u64,
}

impl Watch<'_> {
    fn disarm(self) -> bool {
        let escaped = self.watchdog.disarm(self.ticket);
        std::mem::forget(self);
        escaped
    }
}

impl Drop for Watch<'_> {
    fn drop(&mut self) {
        self.watchdog.disarm(self.ticket);
    }
}

pub(super) struct GrantWatchdog {
    limit: Duration,
    armed: Mutex<Armed>,
    changed: Condvar,
}

impl GrantWatchdog {
    /// Starts a watchdog thread that lives for the rest of the process.
    pub(super) fn spawn(limit: Duration) -> io::Result<&'static Self> {
        let watchdog: &'static Self = Box::leak(Box::new(Self {
            limit,
            armed: Mutex::new(Armed::default()),
            changed: Condvar::new(),
        }));
        std::thread::Builder::new()
            .name("rmux-pty-grant-watch".to_owned())
            .spawn(move || watchdog.run())?;
        Ok(watchdog)
    }

    pub(super) fn watch(
        &self,
        master: BorrowedFd<'_>,
        grant: impl FnOnce() -> Result<(), Errno>,
    ) -> Result<Grant, Errno> {
        let watch = self.arm(master.as_raw_fd());
        let outcome = grant();
        if watch.disarm() {
            return Ok(Grant::Stalled);
        }
        outcome.map(|()| Grant::Granted)
    }

    fn lock(&self) -> MutexGuard<'_, Armed> {
        self.armed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn arm(&self, master: RawFd) -> Watch<'_> {
        let mut armed = self.lock();
        let ticket = armed.next_ticket;
        armed.next_ticket = ticket.wrapping_add(1);
        armed.grants.push(ArmedGrant {
            ticket,
            master,
            deadline: Instant::now() + self.limit,
            escaped: false,
        });
        self.changed.notify_one();
        Watch {
            watchdog: self,
            ticket,
        }
    }

    /// Ends one watch and reports whether the watchdog escaped its grant.
    fn disarm(&self, ticket: u64) -> bool {
        let mut armed = self.lock();
        let index = armed
            .grants
            .iter()
            .position(|grant| grant.ticket == ticket)
            .expect("an armed grant stays registered until its watch ends");
        armed.grants.swap_remove(index).escaped
    }

    fn run(&self) {
        let mut armed = self.lock();
        loop {
            let now = Instant::now();
            for grant in armed.grants.iter_mut() {
                if !grant.escaped && grant.deadline <= now {
                    self.escape(grant, now);
                }
            }
            let next_deadline = armed
                .grants
                .iter()
                .filter(|grant| !grant.escaped)
                .map(|grant| grant.deadline)
                .min();
            armed = match next_deadline {
                None => self
                    .changed
                    .wait(armed)
                    .unwrap_or_else(PoisonError::into_inner),
                Some(deadline) => {
                    self.changed
                        .wait_timeout(armed, deadline.saturating_duration_since(now))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
        }
    }

    /// Runs with the watchdog lock held: the caller is still inside its grant
    /// (it disarms under this lock), so it still owns `grant.master`.
    fn escape(&self, grant: &mut ArmedGrant, now: Instant) {
        match replace_with_dev_null(grant.master) {
            Ok(()) => {
                grant.escaped = true;
                tracing::warn!(
                    fd = grant.master,
                    "pty grant restarted for {} ms; discarding the pair",
                    self.limit.as_millis()
                );
            }
            Err(error) => {
                tracing::warn!(%error, "cannot escape a stalled pty grant; retrying");
                grant.deadline = now + self.limit;
            }
        }
    }
}

/// Points descriptor number `master` at `/dev/null`.
fn replace_with_dev_null(master: RawFd) -> io::Result<()> {
    let null = open(c"/dev/null", OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())?;
    loop {
        // SAFETY: `master` is open and owned by a caller blocked in its grant
        // (see `escape`). xnu keeps the number reserved while `dup2` swaps the
        // file, so no other open can take it; a concurrent lookup sees EBADF,
        // which ends the restarted grant as well.
        if unsafe { libc::dup2(null.as_raw_fd(), master) } != -1 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    // SAFETY: `master` was just made a valid descriptor by `dup2`, and the
    // caller keeps owning it after this borrow ends.
    let replaced = unsafe { BorrowedFd::borrow_raw(master) };
    // `dup2` clears close-on-exec; a concurrently spawned child must not
    // inherit the replacement either.
    fcntl_setfd(replaced, FdFlags::CLOEXEC)?;
    Ok(())
}

#[cfg(test)]
#[path = "macos_grant_tests.rs"]
mod tests;
