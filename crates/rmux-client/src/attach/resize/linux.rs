use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::thread::JoinHandleExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

use rmux_proto::TerminalGeometry;

use super::terminal_geometry_from_fd;
use crate::ClientError;

pub(in crate::attach) struct SignalMaskGuard {
    previous: libc::sigset_t,
}

impl SignalMaskGuard {
    pub(in crate::attach) fn block_winch() -> super::Result<Self> {
        let signals = winch_signals();
        let mut previous = MaybeUninit::uninit();
        // SAFETY: Both pointers refer to valid storage, and a successful call initializes previous.
        let previous = unsafe {
            pthread_result(libc::pthread_sigmask(
                libc::SIG_BLOCK,
                &signals,
                previous.as_mut_ptr(),
            ))?;
            previous.assume_init()
        };
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

fn winch_signals() -> libc::sigset_t {
    let mut signals = MaybeUninit::uninit();
    // SAFETY: sigemptyset initializes the valid storage, and SIGWINCH is a valid signal.
    unsafe {
        libc::sigemptyset(signals.as_mut_ptr());
        libc::sigaddset(signals.as_mut_ptr(), libc::SIGWINCH);
        signals.assume_init()
    }
}

fn pthread_result(code: libc::c_int) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code))
    }
}

fn notify(thread: &thread::JoinHandle<()>) -> io::Result<()> {
    // SAFETY: The join handle still owns the native thread ID, and SIGWINCH is valid.
    pthread_result(unsafe { libc::pthread_kill(thread.as_pthread_t(), libc::SIGWINCH) })
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
            let signals = winch_signals();

            loop {
                // SAFETY: Only SIGWINCH is waited on, and this thread inherits a blocked mask for it.
                let mut signal = 0;
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
        self.thread.as_ref().map_or(Ok(()), notify)
    }
}

impl Drop for ResizeWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = notify(&thread);
            let _ = thread.join();
        }
    }
}
