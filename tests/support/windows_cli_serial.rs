#![cfg(windows)]

use std::fs::{remove_file, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const LOCK_STALE_AFTER: Duration = Duration::from_secs(10 * 60);
const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(300);
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
    path: PathBuf,
    _console_mutex: NamedMutexGuard,
}

pub(crate) fn acquire(label: &str) -> io::Result<WindowsCliSerialGuard> {
    let path = std::env::temp_dir().join("rmux-windows-cli-integration.lock");
    let deadline = Instant::now() + LOCK_WAIT_TIMEOUT;
    loop {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                use std::io::Write as _;
                writeln!(&file, "pid={} label={label}", std::process::id())?;
                let console_mutex = NamedMutexGuard::acquire("Local\\RMUXWindowsConsoleTestLock")?;
                return Ok(WindowsCliSerialGuard {
                    path,
                    _console_mutex: console_mutex,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                remove_stale_lock(&path);
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "timed out waiting for Windows CLI integration lock '{}'",
                            path.display()
                        ),
                    ));
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error),
        }
    }
}

fn remove_stale_lock(path: &Path) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    let Ok(modified) = metadata.modified() else {
        return;
    };
    let Ok(age) = SystemTime::now().duration_since(modified) else {
        return;
    };
    if age >= LOCK_STALE_AFTER {
        let _ = remove_file(path);
    }
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

impl Drop for WindowsCliSerialGuard {
    fn drop(&mut self) {
        let _ = remove_file(&self.path);
    }
}
