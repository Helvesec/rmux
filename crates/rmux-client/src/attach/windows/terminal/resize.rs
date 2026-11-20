//! Console window size queries and the resize polling watcher.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rmux_proto::TerminalSize;
use tokio::sync::mpsc;
use windows_sys::Win32::System::Console::{
    GetConsoleScreenBufferInfo, CONSOLE_SCREEN_BUFFER_INFO, STD_OUTPUT_HANDLE,
};

use super::console_api::std_handle;

pub(in crate::attach::windows) fn current_size() -> Option<TerminalSize> {
    let handle = std_handle(STD_OUTPUT_HANDLE).ok().flatten()?;
    let mut info = std::mem::MaybeUninit::<CONSOLE_SCREEN_BUFFER_INFO>::zeroed();
    let ok = unsafe {
        // SAFETY: info is writable for the Win32 structure expected by this API.
        GetConsoleScreenBufferInfo(handle, info.as_mut_ptr())
    };
    if ok == 0 {
        return None;
    }

    let info = unsafe {
        // SAFETY: Win32 reported that it initialized the structure.
        info.assume_init()
    };
    let width = info.srWindow.Right - info.srWindow.Left + 1;
    let height = info.srWindow.Bottom - info.srWindow.Top + 1;
    let cols = u16::try_from(width).ok()?;
    let rows = u16::try_from(height).ok()?;
    (cols > 0 && rows > 0).then_some(TerminalSize { cols, rows })
}

#[derive(Debug)]
pub(in crate::attach::windows) struct ResizeWatcher {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ResizeWatcher {
    pub(in crate::attach::windows) fn spawn(
        initial_size: Option<TerminalSize>,
        resize_tx: mpsc::UnboundedSender<TerminalSize>,
    ) -> Option<Self> {
        let initial_size = initial_size?;

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut deduper = ResizeDeduper::new(Some(initial_size));
            while !thread_stop.load(Ordering::SeqCst) && !resize_tx.is_closed() {
                thread::sleep(Duration::from_millis(100));
                if let Some(size) = deduper.observe(current_size()) {
                    if resize_tx.send(size).is_err() {
                        return;
                    }
                }
            }
        });

        Some(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for ResizeWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Debug)]
pub(super) struct ResizeDeduper {
    last: Option<TerminalSize>,
}

impl ResizeDeduper {
    pub(super) const fn new(initial: Option<TerminalSize>) -> Self {
        Self { last: initial }
    }

    pub(super) fn observe(&mut self, size: Option<TerminalSize>) -> Option<TerminalSize> {
        if size.is_some() && size != self.last {
            self.last = size;
            return size;
        }
        None
    }
}
