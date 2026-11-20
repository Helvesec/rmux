use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::thread::JoinHandleExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use rmux_proto::TerminalGeometry;

use super::terminal_geometry_from_fd;
use crate::ClientError;

#[derive(Debug)]
pub(in crate::attach) struct SignalMaskGuard {
    previous: libc::sigset_t,
}

impl SignalMaskGuard {
    pub(in crate::attach) fn block_winch() -> super::Result<Self> {
        let signals = winch_signal_set();
        let mut previous = MaybeUninit::<libc::sigset_t>::zeroed();

        // SAFETY: Only SIGWINCH is added to the calling thread's mask, which is
        // not a libc-reserved signal, and `previous` is a valid output buffer.
        let result =
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &signals, previous.as_mut_ptr()) };
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result).into());
        }

        // SAFETY: `previous` was fully zeroed before the call; glibc writes only
        // the kernel-sized prefix of the mask, so every byte is initialized.
        let previous = unsafe { previous.assume_init() };
        Ok(Self { previous })
    }
}

impl Drop for SignalMaskGuard {
    fn drop(&mut self) {
        // SAFETY: This restores the exact mask returned by the earlier successful call.
        let _ = unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous, std::ptr::null_mut())
        };
    }
}

#[derive(Debug)]
pub(in crate::attach) struct ResizeWatcher {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ResizeWatcher {
    pub(in crate::attach) fn spawn(
        terminal_fd: OwnedFd,
        resize_tx: mpsc::Sender<TerminalGeometry>,
    ) -> std::result::Result<Self, ClientError> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);

        let thread = thread::spawn(move || {
            let signals = winch_signal_set();

            loop {
                let mut signal: libc::c_int = 0;
                // SAFETY: Only SIGWINCH is waited on, and this thread inherits a blocked mask for it.
                if unsafe { libc::sigwait(&signals, &mut signal) } != 0 {
                    return;
                }

                if stop_flag.load(Ordering::SeqCst) {
                    return;
                }

                if signal == libc::SIGWINCH {
                    let geometry = match terminal_geometry_from_fd(&terminal_fd) {
                        Ok(Some(geometry)) => geometry,
                        Ok(None) => continue,
                        Err(_) => return,
                    };

                    if resize_tx.send(geometry).is_err() {
                        return;
                    }
                }
            }
        });

        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    #[cfg(test)]
    pub(in crate::attach) fn notify_for_test(&self) -> io::Result<()> {
        self.signal_watcher()
    }

    fn signal_watcher(&self) -> io::Result<()> {
        let Some(thread) = &self.thread else {
            return Ok(());
        };

        // SAFETY: The watcher thread has not been joined, so its pthread_t is
        // still valid, and SIGWINCH is the signal it waits on.
        let result =
            unsafe { libc::pthread_kill(thread.as_pthread_t() as libc::pthread_t, libc::SIGWINCH) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(result))
        }
    }
}

impl Drop for ResizeWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.signal_watcher();

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn winch_signal_set() -> libc::sigset_t {
    let mut signals = MaybeUninit::<libc::sigset_t>::zeroed();
    // SAFETY: `signals` is fully zeroed, and glibc sigemptyset/sigaddset write
    // only its kernel-sized prefix, so every byte is initialized at
    // assume_init. SIGWINCH is a valid signal number, so neither call can fail.
    unsafe {
        libc::sigemptyset(signals.as_mut_ptr());
        libc::sigaddset(signals.as_mut_ptr(), libc::SIGWINCH);
        signals.assume_init()
    }
}

#[cfg(test)]
mod tests {
    use std::mem::{size_of, MaybeUninit};

    use super::{winch_signal_set, SignalMaskGuard};

    const STACK_POISON: u8 = 0xA5;

    /// Fills the stack region that the next sibling call will reuse with a
    /// non-zero pattern, so storage left uninitialized there is observable.
    #[inline(never)]
    fn poison_stack() {
        let mut poison = [STACK_POISON; 64 * 1024];
        std::hint::black_box(&mut poison);
    }

    #[inline(never)]
    fn winch_signal_set_after_poison() -> libc::sigset_t {
        std::hint::black_box(winch_signal_set())
    }

    #[inline(never)]
    fn block_winch_after_poison() -> super::super::Result<SignalMaskGuard> {
        std::hint::black_box(SignalMaskGuard::block_winch())
    }

    fn sigset_bytes(set: &libc::sigset_t) -> [u8; size_of::<libc::sigset_t>()] {
        // SAFETY: libc::sigset_t is a plain integer array on Linux, and every
        // byte compared here is expected to have been initialized.
        unsafe { std::mem::transmute_copy(set) }
    }

    fn zeroed_sigset() -> libc::sigset_t {
        // SAFETY: The all-zero bit pattern is the empty signal set.
        unsafe { MaybeUninit::<libc::sigset_t>::zeroed().assume_init() }
    }

    fn current_thread_mask() -> libc::sigset_t {
        let mut current = zeroed_sigset();
        // SAFETY: A null new set only queries the mask into fully initialized storage.
        let result =
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut current) };
        assert_eq!(result, 0);
        current
    }

    #[test]
    fn winch_signal_set_initializes_the_whole_sigset() {
        let mut expected = zeroed_sigset();
        // SAFETY: `expected` is initialized storage and SIGWINCH is a valid signal.
        assert_eq!(unsafe { libc::sigaddset(&mut expected, libc::SIGWINCH) }, 0);

        poison_stack();
        let signals = winch_signal_set_after_poison();

        assert_eq!(sigset_bytes(&signals), sigset_bytes(&expected));
    }

    #[test]
    fn signal_mask_guard_initializes_the_whole_previous_mask() {
        std::thread::spawn(|| {
            let expected = current_thread_mask();

            poison_stack();
            let guard = block_winch_after_poison().expect("block SIGWINCH");

            assert_eq!(sigset_bytes(&guard.previous), sigset_bytes(&expected));
        })
        .join()
        .expect("signal mask probe thread");
    }
}
