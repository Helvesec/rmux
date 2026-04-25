use std::io;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use rmux_proto::TerminalSize;
use rustix::process::{Pid, Signal};
use rustix::runtime::{kernel_sigprocmask, kernel_sigwait, tkill, How, KernelSigSet};
use rustix::thread::gettid;

use super::terminal_size_from_fd;
use crate::ClientError;

#[derive(Debug)]
pub(in crate::attach) struct SignalMaskGuard {
    previous: KernelSigSet,
}

impl SignalMaskGuard {
    pub(in crate::attach) fn block_winch() -> super::Result<Self> {
        let mut signals = KernelSigSet::empty();
        signals.insert(Signal::WINCH);

        // SAFETY: Only SIGWINCH is added to the mask, which is not a libc-reserved signal.
        let previous = unsafe { kernel_sigprocmask(How::BLOCK, Some(&signals)) }?;
        Ok(Self { previous })
    }
}

impl Drop for SignalMaskGuard {
    fn drop(&mut self) {
        // SAFETY: This restores the exact mask returned by the earlier successful call.
        let _ = unsafe { kernel_sigprocmask(How::SETMASK, Some(&self.previous)) };
    }
}

#[derive(Debug)]
pub(in crate::attach) struct ResizeWatcher {
    stop: Arc<AtomicBool>,
    tid: Pid,
    thread: Option<thread::JoinHandle<()>>,
}

impl ResizeWatcher {
    pub(in crate::attach) fn spawn(
        terminal_fd: OwnedFd,
        resize_tx: mpsc::Sender<TerminalSize>,
    ) -> std::result::Result<Self, ClientError> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let (tid_tx, tid_rx) = mpsc::channel();

        let thread = thread::spawn(move || {
            let _ = tid_tx.send(gettid());
            let mut signals = KernelSigSet::empty();
            signals.insert(Signal::WINCH);

            loop {
                // SAFETY: Only SIGWINCH is waited on, and this thread inherits a blocked mask for it.
                let signal = match unsafe { kernel_sigwait(&signals) } {
                    Ok(signal) => signal,
                    Err(_) => return,
                };

                if stop_flag.load(Ordering::SeqCst) {
                    return;
                }

                if signal == Signal::WINCH {
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

        let tid = tid_rx
            .recv()
            .map_err(|_| ClientError::Io(io::Error::other("resize watcher failed to start")))?;
        Ok(Self {
            stop,
            tid,
            thread: Some(thread),
        })
    }

    #[cfg(test)]
    pub(in crate::attach) fn notify_for_test(&self) -> rustix::io::Result<()> {
        // SAFETY: `self.tid` identifies the watcher thread created above and
        // SIGWINCH is the signal it waits on.
        unsafe { tkill(self.tid, Signal::WINCH) }
    }
}

impl Drop for ResizeWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // SAFETY: `self.tid` identifies the watcher thread created above and
        // SIGWINCH is the signal it waits on.
        let _ = unsafe { tkill(self.tid, Signal::WINCH) };

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
