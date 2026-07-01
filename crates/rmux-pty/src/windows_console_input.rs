use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::Mutex;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ACCESS_DENIED, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    AttachConsole, FreeConsole, GenerateConsoleCtrlEvent, GetConsoleMode, SetConsoleCtrlHandler,
    WriteConsoleInputW, COORD, CTRL_C_EVENT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
    FROM_LEFT_1ST_BUTTON_PRESSED, INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT, KEY_EVENT_RECORD,
    KEY_EVENT_RECORD_0, MOUSE_EVENT, MOUSE_EVENT_RECORD, MOUSE_MOVED, NUMLOCK_ON,
};

use crate::ProcessId;

static CONSOLE_ATTACH_LOCK: Mutex<()> = Mutex::new(());
const LEFT_CTRL_PRESSED: u32 = 0x0008;

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

    /// Creates a Ctrl-C keyboard event.
    #[must_use]
    pub const fn ctrl_c() -> Self {
        Self::new(b'C' as u16, 0x2e, 0x03, LEFT_CTRL_PRESSED, 1)
    }

    /// Creates a Ctrl-D keyboard event.
    #[must_use]
    pub const fn ctrl_d() -> Self {
        Self::new(b'D' as u16, 0, 0x04, LEFT_CTRL_PRESSED | NUMLOCK_ON, 1)
    }

    /// Creates a Ctrl-D event carrying the POSIX EOT byte.
    #[must_use]
    pub const fn ctrl_d_eot() -> Self {
        Self::new(b'D' as u16, 0x20, 0x04, LEFT_CTRL_PRESSED, 1)
    }

    /// Creates a Ctrl-Z keyboard event.
    #[must_use]
    pub const fn ctrl_z() -> Self {
        Self::new(b'Z' as u16, 0x2c, 0x1a, LEFT_CTRL_PRESSED, 1)
    }

    /// Creates a Ctrl-letter keyboard event for an ASCII letter.
    #[must_use]
    pub const fn ctrl_letter(letter: u8) -> Option<Self> {
        if letter >= b'A' && letter <= b'Z' {
            Some(Self::new(
                letter as u16,
                ctrl_letter_scan_code(letter),
                (letter - b'A' + 1) as u16,
                LEFT_CTRL_PRESSED,
                1,
            ))
        } else {
            None
        }
    }

    /// Returns the same key event with an adjusted repeat count.
    #[must_use]
    pub const fn with_repeat_count(self, repeat_count: u16) -> Self {
        Self {
            repeat_count,
            ..self
        }
    }

    /// Returns the Windows virtual-key code.
    #[must_use]
    pub const fn virtual_key_code(self) -> u16 {
        self.virtual_key_code
    }

    /// Returns the Windows virtual scan code.
    #[must_use]
    pub const fn virtual_scan_code(self) -> u16 {
        self.virtual_scan_code
    }

    /// Returns the UTF-16 character reported by the key event.
    #[must_use]
    pub const fn unicode_char(self) -> u16 {
        self.unicode_char
    }

    /// Returns the Windows control-key-state bitset.
    #[must_use]
    pub const fn control_key_state(self) -> u32 {
        self.control_key_state
    }

    /// Returns the Windows key repeat count.
    #[must_use]
    pub const fn repeat_count(self) -> u16 {
        self.repeat_count
    }
}

const fn ctrl_letter_scan_code(letter: u8) -> u16 {
    match letter {
        b'A' => 0x1e,
        b'B' => 0x30,
        b'C' => 0x2e,
        b'D' => 0x20,
        b'E' => 0x12,
        b'F' => 0x21,
        b'G' => 0x22,
        b'H' => 0x23,
        b'I' => 0x17,
        b'J' => 0x24,
        b'K' => 0x25,
        b'L' => 0x26,
        b'M' => 0x32,
        b'N' => 0x31,
        b'O' => 0x18,
        b'P' => 0x19,
        b'Q' => 0x10,
        b'R' => 0x13,
        b'S' => 0x1f,
        b'T' => 0x14,
        b'U' => 0x16,
        b'V' => 0x2f,
        b'W' => 0x11,
        b'X' => 0x2d,
        b'Y' => 0x15,
        b'Z' => 0x2c,
        _ => 0,
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
    trace_windows_key_injection(process_id, key);
    write_windows_console_key_to_attached_console(key)
}

/// Writes a left-button mouse drag into a ConPTY child console.
///
/// Coordinates are zero-based console-cell positions. This mirrors a real
/// Windows Terminal mouse drag more closely than writing xterm SGR bytes into
/// ConPTY input, because RMUX's Windows attach loop reads Win32 console input
/// records and encodes them into SGR before forwarding them to the server.
pub fn write_windows_console_mouse_drag(
    process_id: ProcessId,
    start_x: i16,
    start_y: i16,
    end_x: i16,
    end_y: i16,
) -> io::Result<()> {
    let _guard = CONSOLE_ATTACH_LOCK
        .lock()
        .map_err(|_| io::Error::other("Windows console attach lock poisoned"))?;
    let _attachment = attach_to_process_console(process_id)?;
    trace_windows_mouse_drag(process_id, start_x, start_y, end_x, end_y);
    let handle = open_console_input()?;
    write_windows_console_mouse_drag_to_handle(
        handle.as_raw_handle() as HANDLE,
        start_x,
        start_y,
        end_x,
        end_y,
    )
}

/// Writes a Windows console key and, only when the pane console is in processed
/// input mode, follows it with a scoped console interrupt.
///
/// Raw console/TUI applications commonly disable processed input and expect to
/// receive Ctrl-C as a character. Cooked shells keep processed input enabled and
/// expect Ctrl-C to interrupt the foreground program. This mirrors that native
/// split instead of hard-coding one behavior for every Windows pane.
pub fn write_windows_console_key_then_interrupt_if_processed(
    process_id: ProcessId,
    key: WindowsConsoleKeyEvent,
) -> io::Result<()> {
    let _guard = CONSOLE_ATTACH_LOCK
        .lock()
        .map_err(|_| io::Error::other("Windows console attach lock poisoned"))?;
    let _attachment = attach_to_process_console(process_id)?;
    let handle = open_console_input()?;
    let mode = console_input_mode(handle.as_raw_handle() as HANDLE)?;
    if mode & ENABLE_PROCESSED_INPUT != 0 {
        // ConPTY foreground handoff can lag the first visible prompt/program
        // output. A short second event matches repeated terminal Ctrl-C well
        // enough to close that race without breaking raw-mode Ctrl-C delivery.
        send_windows_console_interrupt_attached(process_id)?;
        send_windows_console_interrupt_attached(process_id)?;
    }
    trace_windows_key_injection(process_id, key);
    write_windows_console_key_to_handle(handle.as_raw_handle() as HANDLE, key)?;
    Ok(())
}

/// Sends a native Ctrl-C interrupt to a Windows ConPTY child console.
///
/// `WriteConsoleInputW` inserts a key record, but it is not the same oracle as
/// a real terminal Ctrl-C: foreground console programs such as Python or
/// `ping.exe` expect a console control event. `CTRL_C_EVENT` cannot be scoped
/// to a process group, so emit it console-wide after attaching to the pane's
/// own ConPTY console and temporarily ignoring Ctrl-C in RMUX itself. Sibling
/// panes have separate ConPTY consoles and do not receive this event.
pub fn send_windows_console_interrupt(process_id: ProcessId) -> io::Result<()> {
    let _guard = CONSOLE_ATTACH_LOCK
        .lock()
        .map_err(|_| io::Error::other("Windows console attach lock poisoned"))?;
    let _attachment = attach_to_process_console(process_id)?;
    send_windows_console_interrupt_attached(process_id)
}

fn send_windows_console_interrupt_attached(process_id: ProcessId) -> io::Result<()> {
    let _ignore_console_control = ConsoleControlIgnoreGuard::install()?;
    trace_windows_console_interrupt(process_id);
    let ok = unsafe {
        // SAFETY: The current process is attached to the target pane console
        // for the duration of this call. `CTRL_C_EVENT` uses process group 0
        // to match a real terminal Ctrl-C in this console; the ignore guard
        // prevents RMUX from handling the event while attached.
        GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)
    };
    if ok == 0 {
        return Err(last_os_error());
    }
    std::thread::sleep(Duration::from_millis(50));
    Ok(())
}

fn trace_windows_key_injection(process_id: ProcessId, key: WindowsConsoleKeyEvent) {
    if std::env::var_os("RMUX_TRACE_WINDOWS_KEYS").is_none() {
        return;
    }
    tracing::debug!(
        target: "rmux::windows_keys",
        pid = process_id.as_u32(),
        virtual_key_code = key.virtual_key_code(),
        virtual_scan_code = key.virtual_scan_code(),
        unicode_char = key.unicode_char(),
        control_key_state = key.control_key_state(),
        repeat_count = key.repeat_count(),
        "inject Windows console key"
    );
}

fn trace_windows_console_interrupt(process_id: ProcessId) {
    if std::env::var_os("RMUX_TRACE_WINDOWS_KEYS").is_none() {
        return;
    }
    tracing::debug!(
        target: "rmux::windows_keys",
        pid = process_id.as_u32(),
        event = "CTRL_C_EVENT",
        "generate Windows console interrupt"
    );
}

fn trace_windows_mouse_drag(
    process_id: ProcessId,
    start_x: i16,
    start_y: i16,
    end_x: i16,
    end_y: i16,
) {
    if std::env::var_os("RMUX_TRACE_WINDOWS_KEYS").is_none() {
        return;
    }
    tracing::debug!(
        target: "rmux::windows_keys",
        pid = process_id.as_u32(),
        start_x,
        start_y,
        end_x,
        end_y,
        "inject Windows console mouse drag"
    );
}

fn write_windows_console_key_to_attached_console(key: WindowsConsoleKeyEvent) -> io::Result<()> {
    let handle = open_console_input()?;
    if should_suppress_cooked_ctrl_d(handle.as_raw_handle() as HANDLE, key)? {
        return Ok(());
    }
    write_windows_console_key_to_handle(handle.as_raw_handle() as HANDLE, key)
}

fn write_windows_console_key_to_handle(
    handle: HANDLE,
    key: WindowsConsoleKeyEvent,
) -> io::Result<()> {
    let records = key_event_records(key);
    let mut written = 0_u32;
    let ok = unsafe {
        // SAFETY: `handle` is the input handle of the currently attached console,
        // `records` points to initialized INPUT_RECORD values, and `written` is
        // valid writable storage for the duration of the call.
        WriteConsoleInputW(handle, records.as_ptr(), records.len() as u32, &mut written)
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

fn write_windows_console_mouse_drag_to_handle(
    handle: HANDLE,
    start_x: i16,
    start_y: i16,
    end_x: i16,
    end_y: i16,
) -> io::Result<()> {
    let records = mouse_drag_records(start_x, start_y, end_x, end_y);
    let mut written = 0_u32;
    let ok = unsafe {
        // SAFETY: `handle` is the input handle of the currently attached console,
        // `records` points to initialized INPUT_RECORD values, and `written` is
        // valid writable storage for the duration of the call.
        WriteConsoleInputW(handle, records.as_ptr(), records.len() as u32, &mut written)
    };
    if ok == 0 {
        return Err(last_os_error());
    }
    if written != records.len() as u32 {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            format!(
                "WriteConsoleInputW wrote {written} of {} mouse records",
                records.len()
            ),
        ));
    }
    Ok(())
}

fn should_suppress_cooked_ctrl_d(handle: HANDLE, key: WindowsConsoleKeyEvent) -> io::Result<bool> {
    if key.virtual_key_code != b'D' as u16 || key.control_key_state & LEFT_CTRL_PRESSED == 0 {
        return Ok(false);
    }
    let mode = console_input_mode(handle)?;
    let suppress = mode & (ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT) != 0;
    trace_windows_ctrl_d_mode(mode, suppress);
    Ok(suppress)
}

fn console_input_mode(handle: HANDLE) -> io::Result<u32> {
    let mut mode = 0_u32;
    let ok = unsafe {
        // SAFETY: `handle` is an open CONIN$ handle and `mode` is writable.
        GetConsoleMode(handle, &mut mode)
    };
    if ok == 0 {
        return Err(last_os_error());
    }
    Ok(mode)
}

fn trace_windows_ctrl_d_mode(mode: u32, suppress: bool) {
    if std::env::var_os("RMUX_TRACE_WINDOWS_KEYS").is_none() {
        return;
    }
    tracing::debug!(
        target: "rmux::windows_keys",
        mode,
        suppress,
        "inspect Windows console Ctrl-D mode"
    );
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

fn mouse_drag_records(start_x: i16, start_y: i16, end_x: i16, end_y: i16) -> [INPUT_RECORD; 3] {
    [
        mouse_input_record(start_x, start_y, FROM_LEFT_1ST_BUTTON_PRESSED, 0),
        mouse_input_record(end_x, end_y, FROM_LEFT_1ST_BUTTON_PRESSED, MOUSE_MOVED),
        mouse_input_record(end_x, end_y, 0, 0),
    ]
}

fn mouse_input_record(x: i16, y: i16, button_state: u32, event_flags: u32) -> INPUT_RECORD {
    INPUT_RECORD {
        EventType: MOUSE_EVENT as u16,
        Event: INPUT_RECORD_0 {
            MouseEvent: MOUSE_EVENT_RECORD {
                dwMousePosition: COORD { X: x, Y: y },
                dwButtonState: button_state,
                dwControlKeyState: 0,
                dwEventFlags: event_flags,
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

fn last_os_error() -> io::Error {
    let code = unsafe {
        // SAFETY: GetLastError reads the calling thread's last-error slot and
        // has no preconditions.
        GetLastError()
    };
    io::Error::from_raw_os_error(code as i32)
}
