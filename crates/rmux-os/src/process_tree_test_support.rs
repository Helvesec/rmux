//! Linux process-exit synchronization for native lifecycle regression tests.

/// Holds an owned Linux test descendant at its ptrace exit event.
pub struct HeldExit {
    pid: i32,
    stopped: bool,
    released: bool,
}

impl HeldExit {
    /// Seizes a descendant created by the calling test on this same thread.
    pub fn seize(pid: i32) -> Self {
        // SAFETY: This test owns the descendant; SEIZE does not dereference the
        // null address or the integer options argument and never resumes it.
        let result = unsafe {
            libc::ptrace(
                libc::PTRACE_SEIZE,
                pid,
                std::ptr::null_mut::<libc::c_void>(),
                libc::PTRACE_O_TRACEEXIT as usize as *mut libc::c_void,
            )
        };
        assert_eq!(
            result,
            0,
            "seize descendant: {}",
            std::io::Error::last_os_error()
        );
        Self {
            pid,
            stopped: false,
            released: false,
        }
    }

    /// Waits until SIGKILL reaches the descendant, without letting it exit.
    pub fn wait_for_exit_stop(&mut self) {
        let mut status = 0;
        // SAFETY: The status pointer is valid; only this owned tracee is waited.
        assert_eq!(
            unsafe { libc::waitpid(self.pid, &mut status, libc::__WALL) },
            self.pid
        );
        self.stopped = libc::WIFSTOPPED(status);
        assert!(self.stopped);
        assert_eq!(status >> 16, libc::PTRACE_EVENT_EXIT);
    }

    /// Releases and reaps the pending SIGKILL exit on the tracing thread.
    pub fn release(&mut self) {
        // SAFETY: The tracee is stopped at its exit event, so CONT only releases
        // its pending exit. No signal is sent to an unowned or recycled PID.
        assert_eq!(
            unsafe { libc::ptrace(libc::PTRACE_CONT, self.pid, 0, 0) },
            0
        );
        let mut status = 0;
        // SAFETY: The status pointer is valid and the traced exit is ours.
        assert_eq!(
            unsafe { libc::waitpid(self.pid, &mut status, libc::__WALL) },
            self.pid
        );
        self.released = true;
        assert!(libc::WIFSIGNALED(status));
        assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
    }
}

impl Drop for HeldExit {
    fn drop(&mut self) {
        if !self.released {
            if !self.stopped {
                // SAFETY: This is the still-owned tracee, not a recycled PID.
                unsafe { libc::kill(self.pid, libc::SIGKILL) };
                self.wait_for_exit_stop();
            }
            self.release();
        }
    }
}
