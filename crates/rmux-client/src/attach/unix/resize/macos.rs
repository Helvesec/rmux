#[cfg(test)]
use std::io;
use std::os::fd::OwnedFd;
use std::sync::mpsc;
use std::thread;

use rmux_proto::TerminalGeometry;
use signal_hook::consts::signal::SIGWINCH;
use signal_hook::iterator::{Handle, Signals};

use super::terminal_geometry_from_fd;
use crate::ClientError;

#[derive(Debug)]
pub(in crate::attach) struct SignalMaskGuard;

impl SignalMaskGuard {
    pub(in crate::attach) fn block_winch() -> super::Result<Self> {
        Ok(Self)
    }
}

#[derive(Debug)]
pub(in crate::attach) struct ResizeWatcher {
    handle: Handle,
    thread: Option<thread::JoinHandle<()>>,
}

impl ResizeWatcher {
    pub(in crate::attach) fn spawn(
        terminal_fd: OwnedFd,
        resize_tx: mpsc::Sender<TerminalGeometry>,
    ) -> std::result::Result<Self, ClientError> {
        let mut signals = Signals::new([SIGWINCH]).map_err(ClientError::Io)?;
        let handle = signals.handle();

        let thread = thread::spawn(move || {
            for signal in signals.forever() {
                if signal == SIGWINCH {
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
            handle,
            thread: Some(thread),
        })
    }

    #[cfg(test)]
    pub(in crate::attach) fn notify_for_test(&self) -> io::Result<()> {
        self.notify()
    }

    #[cfg(test)]
    fn notify(&self) -> io::Result<()> {
        // SAFETY: Raising SIGWINCH exercises the same process-wide signal
        // path that terminal resizes use.
        let result = unsafe { libc::raise(SIGWINCH) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(result))
        }
    }
}

impl Drop for ResizeWatcher {
    fn drop(&mut self) {
        self.handle.close();

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
