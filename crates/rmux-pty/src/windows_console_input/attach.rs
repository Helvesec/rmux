use std::io;
use std::os::windows::io::{FromRawHandle, OwnedHandle};

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{AttachConsole, FreeConsole, SetConsoleCtrlHandler};

use super::last_os_error;
use crate::ProcessId;

pub(super) fn attach_to_process_console(process_id: ProcessId) -> io::Result<ConsoleAttachment> {
    if try_attach_console(process_id.as_u32()) {
        return ConsoleAttachment::protect_current_process();
    }
    let first_error = last_os_error();
    if first_error.raw_os_error() != Some(ERROR_ACCESS_DENIED as i32) {
        return Err(first_error);
    }

    let _ = unsafe {
        // SAFETY: FreeConsole only affects the current process console
        // attachment. It is required before attaching to a different console.
        FreeConsole()
    };
    if try_attach_console(process_id.as_u32()) {
        return ConsoleAttachment::protect_current_process();
    }
    Err(last_os_error())
}

fn try_attach_console(process_id: u32) -> bool {
    let ok = unsafe {
        // SAFETY: AttachConsole validates the process id. On success, the
        // current process is attached until FreeConsole is called.
        AttachConsole(process_id)
    };
    ok != 0
}

pub(super) fn open_console_input() -> io::Result<OwnedHandle> {
    const CONIN: [u16; 7] = [
        b'C' as u16,
        b'O' as u16,
        b'N' as u16,
        b'I' as u16,
        b'N' as u16,
        b'$' as u16,
        0,
    ];
    let handle = unsafe {
        // SAFETY: `CONIN` is a NUL-terminated UTF-16 device name and all other
        // pointer arguments are null by design.
        CreateFileW(
            CONIN.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(last_os_error());
    }
    let handle = unsafe {
        // SAFETY: CreateFileW returned a non-null owned handle that is
        // transferred exactly once into OwnedHandle.
        OwnedHandle::from_raw_handle(handle as _)
    };
    Ok(handle)
}

pub(super) struct ConsoleAttachment {
    // This field is deliberately retained until after `FreeConsole` runs in
    // `Drop`. Console-control delivery is asynchronous, so removing the ignore
    // handler while RMUX is still attached creates a window where the pane's
    // CTRL_C_EVENT can terminate the daemon itself.
    _ignore_console_control: ConsoleControlIgnoreGuard,
}

impl ConsoleAttachment {
    fn protect_current_process() -> io::Result<Self> {
        match ConsoleControlIgnoreGuard::install() {
            Ok(ignore_console_control) => Ok(Self {
                _ignore_console_control: ignore_console_control,
            }),
            Err(error) => {
                let _ = unsafe {
                    // SAFETY: AttachConsole just succeeded, so this rolls back
                    // that process-wide attachment before returning the error.
                    FreeConsole()
                };
                Err(error)
            }
        }
    }
}

impl Drop for ConsoleAttachment {
    fn drop(&mut self) {
        let _ = unsafe {
            // SAFETY: This releases any console attachment owned by the current process.
            FreeConsole()
        };
        // Struct fields are dropped after this method returns, so the ignore
        // guard remains installed through the detach above.
    }
}

struct ConsoleControlIgnoreGuard;

impl ConsoleControlIgnoreGuard {
    fn install() -> io::Result<Self> {
        let ok = unsafe {
            // SAFETY: The handler is a static function with the required ABI.
            // The lock above serializes the short process-wide window.
            SetConsoleCtrlHandler(Some(ignore_console_control_event), 1)
        };
        if ok == 0 {
            return Err(last_os_error());
        }
        Ok(Self)
    }
}

impl Drop for ConsoleControlIgnoreGuard {
    fn drop(&mut self) {
        let _ = unsafe {
            // SAFETY: This removes the process control handler installed by
            // `install`; failure during drop is not recoverable here.
            SetConsoleCtrlHandler(Some(ignore_console_control_event), 0)
        };
    }
}

unsafe extern "system" fn ignore_console_control_event(_control_type: u32) -> i32 {
    1
}
