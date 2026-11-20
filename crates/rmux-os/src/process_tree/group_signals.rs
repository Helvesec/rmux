//! Exit/stop probes and Unix signal forwarding for a spawned process tree.

use std::io;

#[cfg(all(
    unix,
    not(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))
))]
use rustix::process::{waitid, Pid, WaitId, WaitIdOptions, WaitIdStatus};

use super::ProcessTreeChild;

/// Reports whether a `waitid(WEXITED)` answer describes a terminated child.
///
/// Darwin also answers `WEXITED` for a child that is only stopped (measured:
/// `si_code == CLD_STOPPED`, immediately and even without `WNOHANG`). Such a
/// child is still alive, so only an exit, a kill or a core dump counts.
#[cfg(all(
    unix,
    not(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))
))]
pub(super) fn reports_exit(status: &WaitIdStatus) -> bool {
    status.exited() || status.killed() || status.dumped()
}

impl ProcessTreeChild {
    /// Reports whether the direct child has exited without making its Unix
    /// process-group identifier reusable before a possible tree signal.
    pub fn has_exited(&mut self) -> io::Result<bool> {
        if !self.armed {
            return Ok(true);
        }

        #[cfg(all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        ))]
        {
            let pid = Pid::from_raw(self.process_group)
                .ok_or_else(|| io::Error::other("child process id is zero"))?;
            Ok(waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            )?
            .as_ref()
            .is_some_and(reports_exit))
        }

        #[cfg(any(
            not(unix),
            all(
                unix,
                any(
                    target_os = "cygwin",
                    target_os = "horizon",
                    target_os = "openbsd",
                    target_os = "redox",
                    target_os = "wasi"
                )
            )
        ))]
        {
            self.child.try_wait().map(|status| status.is_some())
        }
    }

    /// Reports whether the direct Unix child is stopped without consuming the
    /// status that remains owned by [`Self::wait`].
    #[cfg(unix)]
    pub fn has_stopped(&mut self) -> io::Result<bool> {
        if !self.armed {
            return Ok(false);
        }

        #[cfg(not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        )))]
        {
            let pid = Pid::from_raw(self.process_group)
                .ok_or_else(|| io::Error::other("child process id is zero"))?;
            Ok(waitid(
                WaitId::Pid(pid),
                WaitIdOptions::STOPPED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            )?
            .is_some_and(|status| status.stopped()))
        }

        #[cfg(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))]
        {
            Ok(false)
        }
    }

    /// Forwards a raw Unix signal to the complete child process group.
    #[cfg(all(
        unix,
        not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))
    ))]
    pub fn forward_signal(&mut self, signal: i32) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        let result = unsafe {
            // SAFETY: `process_group` is the positive PID of the child group
            // leader created by this value. A negative PID addresses exactly
            // that process group, and the caller supplies an OS signal value.
            libc::kill(-self.process_group, signal)
        };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH)
            || (error.raw_os_error() == Some(libc::EPERM) && self.has_exited()?)
        {
            Ok(())
        } else {
            Err(error)
        }
    }

    /// Forwards a raw Unix signal to the direct child on targets that cannot
    /// inspect an exited group leader without reaping it.
    #[cfg(all(
        unix,
        any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        )
    ))]
    pub fn forward_signal(&mut self, signal: i32) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        let result = unsafe {
            // SAFETY: `process_group` is also the direct child PID. Limiting
            // the signal to that PID avoids a recycled-group race on targets
            // without waitid(WNOWAIT).
            libc::kill(self.process_group, signal)
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
}
