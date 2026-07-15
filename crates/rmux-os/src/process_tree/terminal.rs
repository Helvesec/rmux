use std::io;
use std::os::fd::{AsFd, BorrowedFd};

use rustix::process::Pid;
#[cfg(not(any(
    target_os = "cygwin",
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
use rustix::termios::{tcgetpgrp, tcsetpgrp};

use super::ProcessTreeChild;

/// Restores the terminal foreground process group when dropped.
pub struct ForegroundTerminalGuard<'fd> {
    fd: Option<BorrowedFd<'fd>>,
    previous_process_group: Option<Pid>,
}

impl ForegroundTerminalGuard<'_> {
    fn empty() -> Self {
        Self {
            fd: None,
            previous_process_group: None,
        }
    }

    /// Restores the process group explicitly so callers can observe failures.
    pub fn restore(mut self) -> io::Result<()> {
        self.restore_inner()
    }

    fn restore_inner(&mut self) -> io::Result<()> {
        let (Some(fd), Some(previous_process_group)) = (self.fd, self.previous_process_group)
        else {
            return Ok(());
        };
        set_terminal_foreground(fd, previous_process_group)?;
        self.fd = None;
        self.previous_process_group = None;
        Ok(())
    }
}

impl Drop for ForegroundTerminalGuard<'_> {
    fn drop(&mut self) {
        let _ = self.restore_inner();
    }
}

impl ProcessTreeChild {
    /// Gives this child process group foreground access to `terminal` until
    /// the returned guard is restored or dropped.
    ///
    /// A child that raced ahead and was stopped for background terminal I/O is
    /// continued only after the foreground transfer succeeds.
    pub fn foreground_terminal<'fd, Fd>(
        &mut self,
        terminal: &'fd Fd,
    ) -> io::Result<ForegroundTerminalGuard<'fd>>
    where
        Fd: AsFd + ?Sized,
    {
        #[cfg(not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        )))]
        {
            if self.has_exited()? {
                return Ok(ForegroundTerminalGuard::empty());
            }
            let terminal = terminal.as_fd();
            let previous_process_group = match tcgetpgrp(terminal) {
                Ok(process_group) => process_group,
                Err(_) if self.has_exited()? => {
                    return Ok(ForegroundTerminalGuard::empty());
                }
                Err(error) => return Err(io::Error::from(error)),
            };
            let process_group = Pid::from_raw(self.process_group)
                .ok_or_else(|| io::Error::other("child process group id is zero"))?;
            let guard = ForegroundTerminalGuard {
                fd: Some(terminal),
                previous_process_group: Some(previous_process_group),
            };
            if let Err(error) = set_terminal_foreground(terminal, process_group) {
                if self.has_exited()? {
                    return Ok(ForegroundTerminalGuard::empty());
                }
                return Err(error);
            }
            self.forward_signal(libc::SIGCONT)?;
            Ok(guard)
        }

        #[cfg(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))]
        {
            let _ = terminal;
            Ok(ForegroundTerminalGuard::empty())
        }
    }
}

#[cfg(not(any(
    target_os = "cygwin",
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
fn set_terminal_foreground(terminal: BorrowedFd<'_>, process_group: Pid) -> io::Result<()> {
    let signal_mask = SigttouMaskGuard::block()?;
    let foreground_result = tcsetpgrp(terminal, process_group).map_err(io::Error::from);
    let restore_result = signal_mask.restore();
    restore_result?;
    foreground_result
}

#[cfg(any(
    target_os = "cygwin",
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
))]
fn set_terminal_foreground(_terminal: BorrowedFd<'_>, _process_group: Pid) -> io::Result<()> {
    Ok(())
}

#[cfg(not(any(
    target_os = "cygwin",
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
struct SigttouMaskGuard {
    previous: libc::sigset_t,
    armed: bool,
}

#[cfg(not(any(
    target_os = "cygwin",
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
impl SigttouMaskGuard {
    fn block() -> io::Result<Self> {
        let mut blocked = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
        let empty_result = unsafe {
            // SAFETY: `blocked` points to writable storage for one sigset_t.
            libc::sigemptyset(blocked.as_mut_ptr())
        };
        if empty_result != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut blocked = unsafe {
            // SAFETY: sigemptyset initialized the complete sigset_t on success.
            blocked.assume_init()
        };
        let add_result = unsafe {
            // SAFETY: `blocked` is initialized and SIGTTOU is a valid signal.
            libc::sigaddset(&mut blocked, libc::SIGTTOU)
        };
        if add_result != 0 {
            return Err(io::Error::last_os_error());
        }

        let mut previous = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
        let mask_result = unsafe {
            // SAFETY: Both signal sets point to valid storage. pthread_sigmask
            // copies their values and does not retain either pointer.
            libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, previous.as_mut_ptr())
        };
        if mask_result != 0 {
            return Err(io::Error::from_raw_os_error(mask_result));
        }
        let previous = unsafe {
            // SAFETY: pthread_sigmask initialized the previous mask on success.
            previous.assume_init()
        };
        Ok(Self {
            previous,
            armed: true,
        })
    }

    fn restore(mut self) -> io::Result<()> {
        self.restore_inner()
    }

    fn restore_inner(&mut self) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        let result = unsafe {
            // SAFETY: `previous` was returned by pthread_sigmask for this
            // thread and is restored without retaining its address.
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous, std::ptr::null_mut())
        };
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result));
        }
        self.armed = false;
        Ok(())
    }
}

#[cfg(not(any(
    target_os = "cygwin",
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
impl Drop for SigttouMaskGuard {
    fn drop(&mut self) {
        let _ = self.restore_inner();
    }
}
