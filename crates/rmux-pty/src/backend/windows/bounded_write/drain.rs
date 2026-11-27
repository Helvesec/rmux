use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
use windows_sys::Win32::Storage::FileSystem::FlushFileBuffers;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread};
use windows_sys::Win32::System::IO::CancelSynchronousIo;

use super::{drain_timeout, last_os_error};

pub(super) fn flush_with_timeout(handle: HANDLE, timeout: Duration) -> io::Result<()> {
    if timeout.is_zero() {
        return Err(drain_timeout(timeout));
    }
    let current_thread = duplicate_current_thread()?;
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let started = Instant::now();
    let watchdog = thread::spawn(move || {
        if done_rx.recv_timeout(timeout).is_ok() {
            return;
        }
        loop {
            unsafe {
                // SAFETY: `current_thread` is a real duplicated handle for the
                // thread executing `FlushFileBuffers`. Repeating cancellation
                // closes the narrow race in which the deadline expires just
                // before that synchronous request enters the kernel.
                CancelSynchronousIo(current_thread.as_raw_handle() as HANDLE);
            }
            match done_rx.recv_timeout(Duration::from_millis(1)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    });

    let flushed = unsafe {
        // SAFETY: `handle` is the live ConPTY input pipe write handle. The
        // caller holds the per-writer operation mutex across this request.
        FlushFileBuffers(handle)
    };
    let flush_error = (flushed == 0).then(last_os_error);
    let timed_out = started.elapsed() >= timeout;
    let _ = done_tx.send(());
    watchdog
        .join()
        .map_err(|_| io::Error::other("ConPTY input drain watchdog panicked"))?;

    if timed_out {
        return Err(drain_timeout(timeout));
    }
    if let Some(error) = flush_error {
        return Err(error);
    }
    Ok(())
}

fn duplicate_current_thread() -> io::Result<OwnedHandle> {
    let process = unsafe {
        // SAFETY: pseudo handles returned for the current process are always
        // valid in the calling process and must not be closed.
        GetCurrentProcess()
    };
    let thread = unsafe {
        // SAFETY: this returns the current thread's pseudo handle.
        GetCurrentThread()
    };
    let mut duplicated = std::ptr::null_mut();
    let ok = unsafe {
        // SAFETY: the source pseudo handle belongs to `process`; the output is
        // initialized on success and receives identical access rights.
        DuplicateHandle(
            process,
            thread,
            process,
            &mut duplicated,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(last_os_error());
    }
    // SAFETY: `DuplicateHandle` returned a uniquely owned real handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(duplicated as _) })
}
