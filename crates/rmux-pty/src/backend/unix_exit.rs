//! Non-consuming observation of a spawned child's exit.

use crate::ProcessId;

/// Returns true only when `pid` is this process' child and has exited.
///
/// `WNOWAIT` leaves the exit status for the owner of the child handle to
/// reap. A pid that is not (or no longer) a waitable child, or any error,
/// answers false: the caller keeps its slower fallback instead of guessing.
/// Darwin also answers `WEXITED` for a child that is only stopped
/// (`CLD_STOPPED`), so only an exit, a kill or a core dump counts.
pub(crate) fn child_has_exited(pid: ProcessId) -> bool {
    let raw_pid: libc::id_t = pid.as_u32();
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    loop {
        // SAFETY: `info` points to writable storage for one siginfo_t, and the
        // flags only query this process' own child without reaping it.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                raw_pid,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            break;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return false;
        }
    }
    // SAFETY: the storage was zeroed before the call and `waitid` succeeded.
    let info = unsafe { info.assume_init() };
    // SAFETY: `waitid` fills the SIGCHLD fields; with WNOHANG and no event,
    // the zeroed pid field stays zero.
    let reported_pid = unsafe { info.si_pid() };
    reported_pid != 0
        && matches!(
            info.si_code,
            libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED
        )
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    use super::child_has_exited;
    use crate::ProcessId;

    fn is_stopped(child: &Child) -> bool {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: `info` points to writable storage for one siginfo_t, and the
        // flags only observe this test's own child without consuming a status.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id(),
                info.as_mut_ptr(),
                libc::WSTOPPED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        // SAFETY: the storage was zeroed and, on success, filled by `waitid`.
        result == 0 && unsafe { info.assume_init().si_pid() } != 0
    }

    fn eventually(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    #[test]
    fn stopped_child_is_not_reported_as_exited() {
        let mut child = Command::new("sh")
            .args(["-c", "kill -STOP $$; exit 0"])
            .spawn()
            .expect("spawn child");
        let pid = ProcessId::new(child.id()).expect("child pid");
        let stopped = eventually(|| is_stopped(&child));
        let stopped_answer = child_has_exited(pid);

        child.kill().expect("kill child");
        let killed_answer = eventually(|| child_has_exited(pid));
        child.wait().expect("reap child");

        assert!(stopped, "child never stopped");
        assert!(!stopped_answer, "a stopped child is still alive");
        assert!(killed_answer, "a killed child has exited");
    }
}
