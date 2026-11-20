//! Thread-local signal setup for Unix child-process test fixtures.

use std::io;

/// Runs a fixture with SIGINT blocked on the calling thread, restoring its
/// previous mask afterwards, including when the fixture unwinds.
///
/// This changes neither process-wide dispositions nor any other thread's mask.
pub fn with_blocked_interrupt<T>(run: impl FnOnce() -> T) -> io::Result<T> {
    // SAFETY: both sets are writable initialized sigset_t storage. Each libc
    // call accesses only these sets and the calling thread's signal mask.
    let mut saved = unsafe {
        let mut blocked = std::mem::zeroed();
        let mut previous = std::mem::zeroed();
        if libc::sigemptyset(&mut blocked) != 0 || libc::sigaddset(&mut blocked, libc::SIGINT) != 0
        {
            return Err(io::Error::last_os_error());
        }
        let error = libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous);
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        RestoreMask {
            previous,
            restored: false,
        }
    };
    let result = run();
    saved.restore()?;
    Ok(result)
}

struct RestoreMask {
    previous: libc::sigset_t,
    restored: bool,
}

impl RestoreMask {
    fn restore(&mut self) -> io::Result<()> {
        // SAFETY: previous was filled by pthread_sigmask on this thread. This
        // private guard never leaves with_blocked_interrupt or crosses threads.
        let error = unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous, std::ptr::null_mut())
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        self.restored = true;
        Ok(())
    }
}

impl Drop for RestoreMask {
    fn drop(&mut self) {
        if !self.restored {
            let _ = self.restore();
        }
    }
}
