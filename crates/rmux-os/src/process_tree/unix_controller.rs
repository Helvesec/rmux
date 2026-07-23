use std::io;
use std::process::{Child, ExitStatus};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// A process-group identity whose numeric ID remains usable only while its
/// direct child still owns that ID.
pub(super) struct UnixProcessGroup {
    id: i32,
    active: Mutex<bool>,
}

impl UnixProcessGroup {
    pub(super) fn new(id: i32) -> Self {
        Self {
            id,
            active: Mutex::new(true),
        }
    }

    pub(super) fn terminate(&self) -> io::Result<()> {
        self.terminate_with(|id| {
            let result = unsafe {
                // SAFETY: The negative value targets the process group created
                // before user code began executing. The state lock keeps that
                // identity armed until the group leader is reaped.
                libc::kill(-id, libc::SIGKILL)
            };
            if result == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        })
    }

    pub(super) fn terminate_and_wait(&self, timeout: Duration) -> io::Result<bool> {
        let active = self.lock_active();
        if !*active {
            return Ok(true);
        }
        let result = unsafe {
            // SAFETY: The negative value targets the process group created
            // before user code began executing. Keeping `active` locked also
            // keeps the group leader unreaped, so its numeric ID cannot be
            // recycled while liveness is observed.
            libc::kill(-self.id, libc::SIGKILL)
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(true);
            }
            if error.raw_os_error() == Some(libc::EPERM)
                && super::unix_group_liveness::wait_for_no_live_members(self.id, Duration::ZERO)?
            {
                return Ok(true);
            }
            return Err(error);
        }
        super::unix_group_liveness::wait_for_no_live_members(self.id, timeout)
    }

    pub(super) fn try_reap(&self, child: &mut Child) -> io::Result<Option<ExitStatus>> {
        let mut active = self.lock_active();
        let status = child.try_wait()?;
        if status.is_some() {
            *active = false;
        }
        Ok(status)
    }

    /// Reaps a child already observed with `waitid(WNOWAIT)`. Holding the same
    /// lock as `terminate` closes the check/signal versus reap/PID-reuse race.
    pub(super) fn reap_exited(&self, child: &mut Child) -> io::Result<ExitStatus> {
        let mut active = self.lock_active();
        let status = child.wait()?;
        *active = false;
        Ok(status)
    }

    pub(super) fn disarm(&self) {
        *self.lock_active() = false;
    }

    fn terminate_with(&self, signal: impl FnOnce(i32) -> io::Result<()>) -> io::Result<()> {
        let active = self.lock_active();
        if !*active {
            return Ok(());
        }
        match signal(self.id) {
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            result => result,
        }
    }

    fn lock_active(&self) -> MutexGuard<'_, bool> {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::process::Command;

    use super::*;
    use crate::process_tree::ProcessTreeChild;

    #[test]
    fn waiting_disarms_every_cloned_controller_before_pid_reuse() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);
        let mut child = ProcessTreeChild::spawn(&mut command).expect("spawn process tree");
        let controller = child.controller();

        assert!(child.wait().expect("wait for child").success());
        let signal_attempted = Cell::new(false);
        controller
            .process_group
            .terminate_with(|_| {
                signal_attempted.set(true);
                Ok(())
            })
            .expect("reaped controller is a no-op");

        assert!(!signal_attempted.get());
    }

    #[test]
    fn reaped_identity_never_signals_a_reused_group_id() {
        let group = UnixProcessGroup::new(42);
        group.disarm();
        let signal_attempted = Cell::new(false);

        group
            .terminate_with(|_| {
                signal_attempted.set(true);
                Ok(())
            })
            .expect("inactive group termination is a no-op");

        assert!(!signal_attempted.get());
    }

    #[test]
    fn permission_denied_is_not_reported_as_success() {
        let group = UnixProcessGroup::new(42);

        let error = group
            .terminate_with(|_| Err(io::Error::from_raw_os_error(libc::EPERM)))
            .expect_err("permission denial must remain observable");

        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
    }

    #[test]
    fn missing_process_group_is_idempotent_success() {
        let group = UnixProcessGroup::new(42);

        group
            .terminate_with(|_| Err(io::Error::from_raw_os_error(libc::ESRCH)))
            .expect("an already-gone process group is terminated");
    }
}
