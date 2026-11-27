#![cfg(windows)]

use std::io;

const WINDOWS_CONSOLE_TEST_LOCK_TIMEOUT_MS: u32 = 300_000;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_ABANDONED: u32 = 0x0000_0080;

type RawHandle = *mut std::ffi::c_void;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(
        lp_mutex_attributes: *mut std::ffi::c_void,
        b_initial_owner: i32,
        lp_name: *const u16,
    ) -> RawHandle;
    fn WaitForSingleObject(h_handle: RawHandle, dw_milliseconds: u32) -> u32;
    fn ReleaseMutex(h_mutex: RawHandle) -> i32;
    fn CloseHandle(h_object: RawHandle) -> i32;
}

pub(crate) struct WindowsCliSerialGuard {
    _console_mutex: NamedMutexGuard,
}

pub(crate) fn acquire(label: &str) -> io::Result<WindowsCliSerialGuard> {
    // The named mutex already serializes every CLI test with the console tests.
    // A second create/delete file lock adds a Windows delete-pending race:
    // CreateFile can return ACCESS_DENIED while the previous file is closing.
    let console_mutex = NamedMutexGuard::acquire("Local\\RMUXWindowsConsoleTestLock")
        .map_err(|error| io::Error::new(error.kind(), format!("{label}: {error}")))?;
    Ok(WindowsCliSerialGuard {
        _console_mutex: console_mutex,
    })
}

struct NamedMutexGuard {
    handle: RawHandle,
}

impl NamedMutexGuard {
    fn acquire(name: &str) -> io::Result<Self> {
        let wide_name = name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let handle = unsafe {
            // SAFETY: The mutex name is a NUL-terminated UTF-16 string and the
            // default security attributes are intentionally null for a per-user
            // test mutex shared with windows_attach_exit.rs.
            CreateMutexW(std::ptr::null_mut(), 0, wide_name.as_ptr())
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let wait = unsafe {
            // SAFETY: `handle` is a valid mutex handle returned by
            // CreateMutexW.
            WaitForSingleObject(handle, WINDOWS_CONSOLE_TEST_LOCK_TIMEOUT_MS)
        };
        if matches!(wait, WAIT_OBJECT_0 | WAIT_ABANDONED) {
            return Ok(Self { handle });
        }
        unsafe {
            // SAFETY: The handle was returned by CreateMutexW and has not yet
            // been closed.
            let _ = CloseHandle(handle);
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("timed out waiting for Windows console test mutex {name:?}: wait={wait}"),
        ))
    }
}

impl Drop for NamedMutexGuard {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: The guard is only constructed after WaitForSingleObject
            // reports ownership of a valid mutex handle.
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}
