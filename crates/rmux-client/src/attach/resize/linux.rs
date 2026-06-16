use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;

use rmux_proto::TerminalGeometry;
use rustix::process::{Pid, Signal};
use rustix::runtime::{kernel_sigprocmask, kernel_sigwait, tkill, How, KernelSigSet};
use rustix::thread::gettid;

use super::terminal_geometry_from_fd;
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
    tid: Arc<(Mutex<Option<Pid>>, Condvar)>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ResizeWatcher {
    pub(in crate::attach) fn spawn(
        terminal_fd: OwnedFd,
        resize_tx: mpsc::Sender<TerminalGeometry>,
    ) -> std::result::Result<Self, ClientError> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let tid = Arc::new((Mutex::new(None), Condvar::new()));
        let thread_tid = Arc::clone(&tid);

        let thread = thread::spawn(move || {
            {
                let (tid_lock, tid_ready) = &*thread_tid;
                if let Ok(mut tid) = tid_lock.lock() {
                    *tid = Some(gettid());
                    tid_ready.notify_all();
                }
            }
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
            tid,
            thread: Some(thread),
        })
    }

    #[cfg(test)]
    pub(in crate::attach) fn notify_for_test(&self) -> rustix::io::Result<()> {
        // SAFETY: `self.tid` identifies the watcher thread created above and
        // SIGWINCH is the signal it waits on.
        let Some(tid) = self.wait_for_tid() else {
            return Ok(());
        };
        unsafe { tkill(tid, Signal::WINCH) }
    }

    fn wait_for_tid(&self) -> Option<Pid> {
        let (tid_lock, tid_ready) = &*self.tid;
        let Ok(mut tid) = tid_lock.lock() else {
            return None;
        };
        while tid.is_none() {
            tid = tid_ready.wait(tid).ok()?;
        }
        *tid
    }
}

impl Drop for ResizeWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(tid) = self.wait_for_tid() {
            // SAFETY: `tid` identifies the watcher thread created above and
            // SIGWINCH is the signal it waits on.
            let _ = unsafe { tkill(tid, Signal::WINCH) };
        }

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
