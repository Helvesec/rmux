use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use rmux_proto::TerminalSize;

use super::terminal_size_from_fd;
use crate::attach::AttachError;
use crate::ClientError;

#[derive(Debug)]
pub(in crate::attach) struct SignalMaskGuard {
    previous: libc::sigset_t,
}

impl SignalMaskGuard {
    pub(in crate::attach) fn block_winch() -> super::Result<Self> {
        let signals = winch_signal_set().map_err(AttachError::Io)?;
        let mut previous = MaybeUninit::<libc::sigset_t>::uninit();

        // SAFETY: Both signal sets are valid pointers. Blocking SIGWINCH lets
        // the watcher thread consume resize notifications with `sigwait`.
        let result =
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &signals, previous.as_mut_ptr()) };
        if result != 0 {
            return Err(AttachError::Io(io::Error::from_raw_os_error(result)));
        }

        // SAFETY: `pthread_sigmask` returned success and initialized previous.
        let previous = unsafe { previous.assume_init() };
        Ok(Self { previous })
    }
}

impl Drop for SignalMaskGuard {
    fn drop(&mut self) {
        // SAFETY: This restores the exact mask returned by the earlier
        // successful `pthread_sigmask` call.
        let _ = unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous, std::ptr::null_mut())
        };
    }
}

#[derive(Debug)]
pub(in crate::attach) struct ResizeWatcher {
    stop: Arc<AtomicBool>,
    thread_id: libc::pthread_t,
    thread: Option<thread::JoinHandle<()>>,
}

impl ResizeWatcher {
    pub(in crate::attach) fn spawn(
        terminal_fd: OwnedFd,
        resize_tx: mpsc::Sender<TerminalSize>,
    ) -> std::result::Result<Self, ClientError> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let (thread_id_tx, thread_id_rx) = mpsc::channel();

        let thread = thread::spawn(move || {
            // SAFETY: `pthread_self` returns the current thread identifier.
            let _ = thread_id_tx.send(unsafe { libc::pthread_self() });
            let signals = match winch_signal_set() {
                Ok(signals) => signals,
                Err(_) => return,
            };

            loop {
                let mut signal = 0;
                // SAFETY: The signal set is initialized and the signal out
                // pointer is valid for this call.
                let result = unsafe { libc::sigwait(&signals, &mut signal) };
                if result != 0 {
                    return;
                }

                if stop_flag.load(Ordering::SeqCst) {
                    return;
                }

                if signal == libc::SIGWINCH {
                    let size = match terminal_size_from_fd(&terminal_fd) {
                        Ok(size) => size,
                        Err(_) => return,
                    };

                    if resize_tx.send(size).is_err() {
                        return;
                    }
                }
            }
        });

        let thread_id = thread_id_rx
            .recv()
            .map_err(|_| ClientError::Io(io::Error::other("resize watcher failed to start")))?;

        Ok(Self {
            stop,
            thread_id,
            thread: Some(thread),
        })
    }

    #[cfg(test)]
    pub(in crate::attach) fn notify_for_test(&self) -> io::Result<()> {
        self.notify()
    }

    fn notify(&self) -> io::Result<()> {
        // SAFETY: `thread_id` identifies the watcher thread created above.
        let result = unsafe { libc::pthread_kill(self.thread_id, libc::SIGWINCH) };
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
        let _ = self.notify();

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn winch_signal_set() -> io::Result<libc::sigset_t> {
    let mut signals = MaybeUninit::<libc::sigset_t>::uninit();

    // SAFETY: `sigemptyset` initializes the provided signal set on success.
    let result = unsafe { libc::sigemptyset(signals.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: The previous call initialized `signals`.
    let mut signals = unsafe { signals.assume_init() };
    // SAFETY: `signals` is initialized and SIGWINCH is a valid signal number.
    let result = unsafe { libc::sigaddset(&mut signals, libc::SIGWINCH) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(signals)
}
