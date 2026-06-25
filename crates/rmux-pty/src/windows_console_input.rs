use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ACCESS_DENIED, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    AttachConsole, FreeConsole, WriteConsoleInputW, INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT,
    KEY_EVENT_RECORD, KEY_EVENT_RECORD_0,
};

use crate::ProcessId;

static CONSOLE_ATTACH_LOCK: Mutex<()> = Mutex::new(());

/// A Windows console keyboard event that can be injected into a ConPTY child.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowsConsoleKeyEvent {
    virtual_key_code: u16,
    virtual_scan_code: u16,
    unicode_char: u16,
    control_key_state: u32,
    repeat_count: u16,
}

impl WindowsConsoleKeyEvent {
    /// Creates a key event from the fields of a Windows `KEY_EVENT_RECORD`.
    #[must_use]
    pub const fn new(
        virtual_key_code: u16,
        virtual_scan_code: u16,
        unicode_char: u16,
        control_key_state: u32,
        repeat_count: u16,
    ) -> Self {
        Self {
            virtual_key_code,
            virtual_scan_code,
            unicode_char,
            control_key_state,
            repeat_count,
        }
    }
}

/// Writes a Windows console key press/release pair into a ConPTY child console.
///
/// This is used for console-key semantics that cannot be represented by writing
/// a byte stream to ConPTY input pipes on older Windows builds.
pub fn write_windows_console_key(
    process_id: ProcessId,
    key: WindowsConsoleKeyEvent,
) -> io::Result<()> {
    let _guard = CONSOLE_ATTACH_LOCK
        .lock()
        .map_err(|_| io::Error::other("Windows console attach lock poisoned"))?;
    let _attachment = attach_to_process_console(process_id)?;
    let handle = open_console_input()?;
    let records = key_event_records(key);
    let mut written = 0_u32;
    let ok = unsafe {
        // SAFETY: `handle` is the input handle of the currently attached console,
        // `records` points to initialized INPUT_RECORD values, and `written` is
        // valid writable storage for the duration of the call.
        WriteConsoleInputW(
            handle.as_raw_handle() as HANDLE,
            records.as_ptr(),
            records.len() as u32,
            &mut written,
        )
    };
    if ok == 0 {
        return Err(last_os_error());
    }
    if written != records.len() as u32 {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            format!(
                "WriteConsoleInputW wrote {written} of {} records",
                records.len()
            ),
        ));
    }
    Ok(())
}

fn attach_to_process_console(process_id: ProcessId) -> io::Result<ConsoleAttachment> {
    if try_attach_console(process_id.as_u32()) {
        return Ok(ConsoleAttachment);
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
        return Ok(ConsoleAttachment);
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

fn open_console_input() -> io::Result<OwnedHandle> {
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

fn key_event_records(key: WindowsConsoleKeyEvent) -> [INPUT_RECORD; 2] {
    [
        key_input_record(key, true),
        key_input_record(
            WindowsConsoleKeyEvent {
                repeat_count: 1,
                ..key
            },
            false,
        ),
    ]
}

fn key_input_record(key: WindowsConsoleKeyEvent, key_down: bool) -> INPUT_RECORD {
    INPUT_RECORD {
        EventType: KEY_EVENT as u16,
        Event: INPUT_RECORD_0 {
            KeyEvent: KEY_EVENT_RECORD {
                bKeyDown: i32::from(key_down),
                wRepeatCount: key.repeat_count.max(1),
                wVirtualKeyCode: key.virtual_key_code,
                wVirtualScanCode: key.virtual_scan_code,
                uChar: KEY_EVENT_RECORD_0 {
                    UnicodeChar: key.unicode_char,
                },
                dwControlKeyState: key.control_key_state,
            },
        },
    }
}

struct ConsoleAttachment;

impl Drop for ConsoleAttachment {
    fn drop(&mut self) {
        let _ = unsafe {
            // SAFETY: This releases any console attachment owned by the current process.
            FreeConsole()
        };
    }
}

fn last_os_error() -> io::Error {
    let code = unsafe {
        // SAFETY: GetLastError reads the calling thread's last-error slot and
        // has no preconditions.
        GetLastError()
    };
    io::Error::from_raw_os_error(code as i32)
}
