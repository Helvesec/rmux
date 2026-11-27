use std::io;
use std::process::{Child, ExitStatus};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rustix::process::{waitid, Pid, WaitId, WaitIdOptions};

use super::group_signals::reports_exit;

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

    /// Blocks, without reaping and without holding `active`, until the group
    /// leader has terminated. The leader then stays a zombie, so its ID cannot
    /// be reused before [`Self::reap_exited`] takes the lock.
    ///
    /// A stopped leader is still alive: `terminate` must keep the lock free to
    /// deliver the SIGKILL that ends it. Darwin answers this `waitid` at once for
    /// such a leader, so a non-exit answer waits for the exit event instead.
    pub(super) fn wait_for_leader_exit(&self) -> io::Result<()> {
        let pid =
            Pid::from_raw(self.id).ok_or_else(|| io::Error::other("child process id is zero"))?;
        loop {
            let status = waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
            )?;
            if status.as_ref().is_some_and(reports_exit) {
                return Ok(());
            }
            wait_for_exit_event(self.id)?;
        }
    }

    /// Reaps a child already observed to have exited by
    /// [`Self::wait_for_leader_exit`]. Holding the same lock as `terminate`
    /// closes the check/signal versus reap/PID-reuse race.
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

/// Blocks until `pid`, a child of this process, exits. Its exit status stays
/// unreaped: `EVFILT_PROC` only observes the exit.
#[cfg(target_vendor = "apple")]
fn wait_for_exit_event(pid: i32) -> io::Result<()> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    // SAFETY: `kqueue` takes no arguments and returns a new descriptor or -1.
    let raw_queue = unsafe { libc::kqueue() };
    if raw_queue < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw_queue` is a fresh descriptor that nothing else owns.
    let queue = unsafe { OwnedFd::from_raw_fd(raw_queue) };
    let change = libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    loop {
        let mut event = std::mem::MaybeUninit::<libc::kevent>::zeroed();
        // SAFETY: `change` and `event` each point to one valid `kevent`, and a
        // null timeout blocks until the registered exit or an error event.
        let ready = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                &change,
                1,
                event.as_mut_ptr(),
                1,
                std::ptr::null(),
            )
        };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        // SAFETY: the storage was zeroed and `kevent` succeeded.
        let event = unsafe { event.assume_init() };
        if ready == 0 || event.flags & libc::EV_ERROR == 0 {
            return Ok(());
        }
        // An exited leader is a zombie that EVFILT_PROC can no longer attach to.
        return match i32::try_from(event.data) {
            Ok(libc::ESRCH) => Ok(()),
            Ok(errno) => Err(io::Error::from_raw_os_error(errno)),
            Err(_) => Err(io::Error::other("kevent reported an invalid error")),
        };
    }
}

/// POSIX `waitid(WEXITED)` reports only terminations, as measured on Linux; a
/// non-exit answer here was never observed. Poll instead of spinning on it.
#[cfg(not(target_vendor = "apple"))]
fn wait_for_exit_event(_pid: i32) -> io::Result<()> {
    std::thread::sleep(Duration::from_millis(10));
    Ok(())
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

    fn spawn_stopped_leader(script: &str) -> ProcessTreeChild {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        let mut child = ProcessTreeChild::spawn(&mut command).expect("spawn process tree");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !child.has_stopped().expect("probe stopped leader") {
            assert!(std::time::Instant::now() < deadline, "leader never stopped");
            std::thread::sleep(Duration::from_millis(5));
        }
        child
    }

    /// What a waiter observed while a stopped leader was terminated under it.
    struct StoppedLeaderWait {
        status: ExitStatus,
        waiter_cpu: Duration,
        waiter_elapsed: Duration,
    }

    /// CPU time consumed so far by the calling thread.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    fn thread_cpu_time() -> Duration {
        let mut now = std::mem::MaybeUninit::<libc::timespec>::zeroed();
        // SAFETY: `now` points to writable storage for one timespec.
        let result =
            unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, now.as_mut_ptr()) };
        assert_eq!(result, 0, "thread CPU clock");
        // SAFETY: `clock_gettime` succeeded and filled the storage.
        let now = unsafe { now.assume_init() };
        Duration::new(
            u64::try_from(now.tv_sec).expect("non-negative seconds"),
            u32::try_from(now.tv_nsec).expect("nanoseconds below one second"),
        )
    }

    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    fn thread_cpu_time() -> Duration {
        Duration::ZERO
    }

    /// The one deadline that `terminate` and `wait` share once termination starts.
    const BLOCKED_CALL_DEADLINE: Duration = Duration::from_secs(5);

    /// Names how a helper thread answered by the shared deadline.
    fn answer_state<T>(answer: &Result<T, std::sync::mpsc::RecvTimeoutError>) -> &'static str {
        match answer {
            Ok(_) => "returned",
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => "still blocked",
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => "ended without answering",
        }
    }

    /// Blocks one thread in `wait` on a stopped leader for `stopped_for`, then
    /// terminates the tree from another thread. Both calls share one deadline,
    /// and the test fails instead of hanging if either is still blocked.
    ///
    /// A blocked leader is never signalled by PID: only the waiter reaps it,
    /// so once the deadline has passed its PID may already name another
    /// process. It stays stopped in its own process group until this test
    /// process exits; the kernel then sends that orphaned group SIGHUP and
    /// SIGCONT, which end it.
    fn wait_then_terminate_stopped_leader(stopped_for: Duration) -> StoppedLeaderWait {
        use std::sync::mpsc;
        use std::time::Instant;

        let mut child = spawn_stopped_leader("kill -STOP $$; exit 0");
        let controller = child.controller();
        let (waited_tx, waited) = mpsc::channel();
        std::thread::spawn(move || {
            let started = Instant::now();
            let cpu = thread_cpu_time();
            let status = child.wait();
            let _ = waited_tx.send((status, thread_cpu_time() - cpu, started.elapsed()));
        });
        std::thread::sleep(stopped_for);
        let (terminated_tx, terminated) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = terminated_tx.send(controller.terminate());
        });

        let deadline = Instant::now() + BLOCKED_CALL_DEADLINE;
        let terminate_answer =
            terminated.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let wait_answer = waited.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let (Ok(terminate_result), Ok((status, waiter_cpu, waiter_elapsed))) =
            (&terminate_answer, &wait_answer)
        else {
            panic!(
                "{BLOCKED_CALL_DEADLINE:?} after terminating a stopped leader: terminate {}, wait {}",
                answer_state(&terminate_answer),
                answer_state(&wait_answer)
            );
        };
        terminate_result.as_ref().expect("terminate succeeds");
        StoppedLeaderWait {
            status: *status.as_ref().expect("wait succeeds"),
            waiter_cpu: *waiter_cpu,
            waiter_elapsed: *waiter_elapsed,
        }
    }

    #[test]
    fn stopped_leader_never_blocks_a_concurrent_terminate() {
        use std::os::unix::process::ExitStatusExt as _;

        let observed = wait_then_terminate_stopped_leader(Duration::from_millis(200));
        assert_eq!(observed.status.signal(), Some(libc::SIGKILL));
    }

    /// Darwin keeps answering `waitid(WEXITED)` for a stopped leader, so a
    /// waiter that retried it would burn a core until the leader exits.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    #[test]
    fn waiting_on_a_stopped_leader_does_not_spin() {
        let observed = wait_then_terminate_stopped_leader(Duration::from_millis(500));
        assert!(
            observed.waiter_cpu * 10 < observed.waiter_elapsed,
            "wait burnt {:?} CPU over {:?}",
            observed.waiter_cpu,
            observed.waiter_elapsed
        );
    }

    #[test]
    fn continued_leader_is_waited_until_its_real_exit() {
        use std::sync::mpsc;

        let mut child = spawn_stopped_leader("kill -STOP $$; exit 7");
        let leader = i32::try_from(child.child_mut().id()).expect("pid fits i32");
        let (waited_tx, waited) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = waited_tx.send(child.wait());
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            waited.try_recv().is_err(),
            "wait returned for a stopped leader"
        );
        // SAFETY: `leader` is this test's child and is still stopped: nothing
        // but this SIGCONT lets it exit, so it cannot have been reaped yet.
        assert_eq!(unsafe { libc::kill(leader, libc::SIGCONT) }, 0);

        let status = waited
            .recv_timeout(Duration::from_secs(5))
            .expect("wait returns once the leader exits")
            .expect("wait succeeds");
        assert_eq!(status.code(), Some(7));
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
