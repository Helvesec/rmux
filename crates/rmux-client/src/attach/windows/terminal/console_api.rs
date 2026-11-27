use super::{vt_context, AttachError, ConsoleApi, Result, Win32Console, ATTACH_CONSOLE_IO};
use std::io;
use windows_sys::Win32::{
    Foundation::{GetLastError, ERROR_INVALID_HANDLE, HANDLE, INVALID_HANDLE_VALUE},
    System::Console::{FlushConsoleInputBuffer, GetConsoleMode, GetStdHandle, SetConsoleMode},
};

impl ConsoleApi for Win32Console {
    type Handle = HANDLE;

    fn supports_vt_input(&self) -> bool {
        vt_context::supported()
    }

    fn set_vt_input(&self, input: HANDLE, output: HANDLE, mode: u32, enabled: bool) -> Result<()> {
        vt_context::set(input, output, mode, enabled).map_err(AttachError::Io)
    }

    fn std_handle(&self, handle_id: u32) -> Result<Option<Self::Handle>> {
        std_handle(handle_id)
    }

    fn get_console_mode(&self, handle: Self::Handle) -> Result<Option<u32>> {
        ATTACH_CONSOLE_IO.synchronized(|| {
            let mut mode = 0;
            let ok = unsafe {
                // SAFETY: handle is a valid std handle and mode points to
                // writable storage.
                GetConsoleMode(handle, &mut mode)
            };
            if ok == 0 {
                return console_mode_absent_or_error();
            }
            Ok(Some(mode))
        })?
    }

    fn set_console_mode(&self, handle: Self::Handle, mode: u32) -> Result<()> {
        ATTACH_CONSOLE_IO.synchronized(|| {
            let ok = unsafe {
                // SAFETY: handle is a console handle and mode is a bitmask
                // accepted by Win32.
                SetConsoleMode(handle, mode)
            };
            if ok == 0 {
                return Err(AttachError::Io(io::Error::last_os_error()));
            }
            Ok(())
        })?
    }

    fn flush_console_input(&self, handle: Self::Handle) -> Result<()> {
        ATTACH_CONSOLE_IO.synchronized(|| {
            let ok = unsafe {
                // SAFETY: handle is a valid console input handle captured by
                // ConsoleMode.
                FlushConsoleInputBuffer(handle)
            };
            if ok == 0 {
                return Err(AttachError::Io(io::Error::last_os_error()));
            }
            Ok(())
        })?
    }
}

pub(super) fn std_handle(handle_id: u32) -> Result<Option<HANDLE>> {
    let handle = unsafe {
        // SAFETY: GetStdHandle accepts the documented STD_* constants.
        GetStdHandle(handle_id)
    };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Ok(None);
    }
    Ok(Some(handle))
}

fn console_mode_absent_or_error<T>() -> Result<Option<T>> {
    let error = unsafe {
        // SAFETY: GetLastError reads the calling thread's last Win32 error.
        GetLastError()
    };
    if error == ERROR_INVALID_HANDLE {
        return Ok(None);
    }
    Err(AttachError::Io(io::Error::from_raw_os_error(
        i32::try_from(error).unwrap_or(i32::MAX),
    )))
}
