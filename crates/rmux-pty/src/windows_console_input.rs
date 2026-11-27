mod attach;
mod key_event;

use std::io;
use std::os::windows::io::AsRawHandle;
use std::sync::Mutex;
use std::time::Duration;

use windows_sys::Win32::Foundation::{GetLastError, HANDLE};
use windows_sys::Win32::System::Console::{
    GenerateConsoleCtrlEvent, GetConsoleMode, WriteConsoleInputW, COORD, CTRL_C_EVENT,
    ENABLE_PROCESSED_INPUT, FROM_LEFT_1ST_BUTTON_PRESSED, INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT,
    KEY_EVENT_RECORD, KEY_EVENT_RECORD_0, MOUSE_EVENT, MOUSE_EVENT_RECORD, MOUSE_MOVED,
};

use crate::ProcessId;
use attach::{attach_to_process_console, open_console_input};
pub use key_event::WindowsConsoleKeyEvent;

static CONSOLE_ATTACH_LOCK: Mutex<()> = Mutex::new(());
const LEFT_CTRL_PRESSED: u32 = 0x0008;
const CONSOLE_TEXT_KEY_BATCH: usize = 2048;
const VK_PACKET: u16 = 0x00e7;

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

/// Writes one atomic batch of Windows console key press/release pairs into a
/// ConPTY child console.
///
/// Keeping the records in one `WriteConsoleInputW` call is useful when the
/// consumer intentionally classifies a console-input batch as a unit. Cooked
/// Ctrl-D events receive the same suppression policy as the single-key API.
pub fn write_windows_console_key_batch(
    process_id: ProcessId,
    keys: &[WindowsConsoleKeyEvent],
) -> io::Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    let _guard = CONSOLE_ATTACH_LOCK
        .lock()
        .map_err(|_| io::Error::other("Windows console attach lock poisoned"))?;
    let _attachment = attach_to_process_console(process_id)?;
    let handle = open_console_input()?;
    let handle = handle.as_raw_handle() as HANDLE;
    let mut records = Vec::with_capacity(keys.len().saturating_mul(2));
    for key in keys {
        trace_windows_key_injection(process_id, *key);
        if should_suppress_cooked_ctrl_d(handle, *key)? {
            continue;
        }
        records.extend(key_event_records(*key));
    }
    write_windows_console_records_to_handle(handle, &records)
}

/// Injects UTF-8 text as Unicode console key records into a ConPTY child.
///
/// This preserves terminal control sequences on older ConPTY builds which
/// parse and consume bracketed-paste delimiters written through the raw input
/// pipe. The console attachment remains locked across all bounded record
/// batches so concurrent key injection cannot interleave with the text.
pub fn write_windows_console_utf8(process_id: ProcessId, bytes: &[u8]) -> io::Result<()> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Windows console text is not valid UTF-8: {error}"),
        )
    })?;
    if text.is_empty() {
        return Ok(());
    }

    let _guard = CONSOLE_ATTACH_LOCK
        .lock()
        .map_err(|_| io::Error::other("Windows console attach lock poisoned"))?;
    let _attachment = attach_to_process_console(process_id)?;
    let handle = open_console_input()?;
    let handle = handle.as_raw_handle() as HANDLE;
    write_console_utf8_batches(text, |keys| write_console_key_batch_to_handle(handle, keys))
}

/// Splits `text` into bounded literal-key batches and hands each one to
/// `write_batch`.
///
/// The record writer is a parameter purely so a test can observe the batch
/// boundaries and the emitted code units: `WriteConsoleInputW` needs a real
/// attached ConPTY child console, which a unit test cannot provide, and the
/// bound itself is what keeps a large paste from being submitted as one
/// unbounded record array.
///
/// A surrogate pair is never divided between two calls. Splitting purely on the
/// bound would end a batch on a high surrogate whenever a supplementary
/// character straddles it, submitting each half in a separate
/// `WriteConsoleInputW` call; a reader that decodes what one call delivered
/// then sees an unpaired half. Closing the batch one unit early keeps the pair
/// in a single call and still respects the bound.
fn write_console_utf8_batches<W>(text: &str, mut write_batch: W) -> io::Result<()>
where
    W: FnMut(&[WindowsConsoleKeyEvent]) -> io::Result<()>,
{
    let mut keys = Vec::with_capacity(CONSOLE_TEXT_KEY_BATCH);
    for code_unit in text.encode_utf16() {
        if keys.len() == CONSOLE_TEXT_KEY_BATCH - 1 && is_high_surrogate(code_unit) {
            write_batch(&keys)?;
            keys.clear();
        }
        keys.push(literal_console_key(code_unit));
        if keys.len() == CONSOLE_TEXT_KEY_BATCH {
            write_batch(&keys)?;
            keys.clear();
        }
    }
    write_batch(&keys)
}

const fn is_high_surrogate(code_unit: u16) -> bool {
    matches!(code_unit, 0xd800..=0xdbff)
}

fn write_console_key_batch_to_handle(
    handle: HANDLE,
    keys: &[WindowsConsoleKeyEvent],
) -> io::Result<()> {
    let mut records = Vec::with_capacity(keys.len().saturating_mul(2));
    for key in keys {
        records.extend(key_event_records(*key));
    }
    write_windows_console_records_to_handle(handle, &records)
}

const fn literal_console_key(code_unit: u16) -> WindowsConsoleKeyEvent {
    let (virtual_key_code, virtual_scan_code, control_key_state) = match code_unit {
        0 => (b'@' as u16, 0, LEFT_CTRL_PRESSED),
        0x08 => (0x08, 0x0e, 0),
        0x09 => (0x09, 0x0f, 0),
        0x0a | 0x0d => (0x0d, 0x1c, 0),
        0x1b => (0x1b, 0x01, 0),
        _ => (VK_PACKET, 0, 0),
    };
    WindowsConsoleKeyEvent::new(
        virtual_key_code,
        virtual_scan_code,
        code_unit,
        control_key_state,
        1,
    )
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

/// Writes a Windows console key and reports whether the pane console is in
/// processed input mode, i.e. whether the caller must follow the key record
/// with [`send_windows_console_interrupt`].
///
/// Raw console/TUI applications commonly disable processed input and expect to
/// receive Ctrl-C as a character. Cooked shells keep processed input enabled and
/// expect Ctrl-C to interrupt the foreground program. This mirrors that native
/// split instead of hard-coding one behavior for every Windows pane.
///
/// The interrupt is left to the caller because the key record and the interrupt
/// are two separate observable effects: a caller that retries a transient
/// console failure must retry them independently. One physical Ctrl-C must
/// produce one key record and one console interrupt — replaying the pair is
/// observable by handlers that intentionally survive the first interrupt and
/// can turn a single keystroke into a forced exit.
pub fn write_windows_console_key_reporting_processed_input(
    process_id: ProcessId,
    key: WindowsConsoleKeyEvent,
) -> io::Result<bool> {
    let _guard = CONSOLE_ATTACH_LOCK
        .lock()
        .map_err(|_| io::Error::other("Windows console attach lock poisoned"))?;
    let _attachment = attach_to_process_console(process_id)?;
    let handle = open_console_input()?;
    let mode = console_input_mode(handle.as_raw_handle() as HANDLE)?;
    trace_windows_key_injection(process_id, key);
    write_windows_console_key_to_handle(handle.as_raw_handle() as HANDLE, key)?;
    Ok(mode & ENABLE_PROCESSED_INPUT != 0)
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
    trace_windows_console_interrupt(process_id);
    let ok = unsafe {
        // SAFETY: The current process is attached to the target pane console
        // for the duration of this call. `CTRL_C_EVENT` uses process group 0
        // to match a real terminal Ctrl-C in this console; the attachment's
        // ignore guard prevents RMUX from handling the event while attached.
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
    write_windows_console_records_to_handle(handle, &records)
}

fn write_windows_console_records_to_handle(
    handle: HANDLE,
    records: &[INPUT_RECORD],
) -> io::Result<()> {
    if records.is_empty() {
        return Ok(());
    }
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
    if key.virtual_key_code != b'D' as u16
        || key.control_key_state & LEFT_CTRL_PRESSED == 0
        || key.virtual_scan_code != 0
    {
        return Ok(false);
    }
    let mode = console_input_mode(handle)?;
    let suppress = should_suppress_typed_ctrl_d_for_mode(mode, key);
    trace_windows_ctrl_d_mode(mode, suppress);
    Ok(suppress)
}

const fn should_suppress_typed_ctrl_d_for_mode(mode: u32, key: WindowsConsoleKeyEvent) -> bool {
    key.virtual_key_code == b'D' as u16
        && key.control_key_state & LEFT_CTRL_PRESSED != 0
        && key.virtual_scan_code == 0
        && mode & ENABLE_PROCESSED_INPUT != 0
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

fn last_os_error() -> io::Error {
    let code = unsafe {
        // SAFETY: GetLastError reads the calling thread's last-error slot and
        // has no preconditions.
        GetLastError()
    };
    io::Error::from_raw_os_error(code as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Console::ENABLE_LINE_INPUT;

    #[test]
    fn literal_console_keys_preserve_bracketed_utf16_text() {
        let text = "\u{1b}[200~alpha\r\nβ😀\u{1b}[201~";
        let keys = text
            .encode_utf16()
            .map(literal_console_key)
            .collect::<Vec<_>>();
        let actual = keys
            .iter()
            .map(|key| key.unicode_char())
            .collect::<Vec<_>>();

        assert_eq!(actual, text.encode_utf16().collect::<Vec<_>>());
        assert_eq!(keys[0].virtual_key_code(), 0x1b);
        assert_eq!(keys[0].virtual_scan_code(), 0x01);
        let lf = keys
            .iter()
            .find(|key| key.unicode_char() == b'\n' as u16)
            .expect("LF key");
        assert_eq!(lf.virtual_key_code(), 0x0d);
        assert_eq!(lf.virtual_scan_code(), 0x1c);
        assert_eq!(lf.control_key_state(), 0);

        for literal in ['A', '-', '[', '~', 'é', '🦀'] {
            let mut encoded = [0_u16; 2];
            for code_unit in literal.encode_utf16(&mut encoded) {
                let key = literal_console_key(*code_unit);
                assert_eq!(
                    key.virtual_key_code(),
                    VK_PACKET,
                    "U+{code_unit:04X} must not alias a navigation or modifier key"
                );
                assert_eq!(key.virtual_scan_code(), 0);
            }
        }
    }

    /// The bound is pinned as a literal, not read from
    /// [`CONSOLE_TEXT_KEY_BATCH`]: a test that derives its payload from the
    /// constant follows the constant, so raising or removing the bound would
    /// leave it green while an arbitrarily large record array reached
    /// `WriteConsoleInputW` in a single call.
    const PINNED_BATCH: usize = 2048;

    /// Collects the code units of every batch the writer seam emits.
    fn batched_code_units(text: &str) -> Vec<Vec<u16>> {
        let mut batches = Vec::new();
        write_console_utf8_batches(text, |keys| {
            batches.push(keys.iter().map(|key| key.unicode_char()).collect());
            Ok(())
        })
        .expect("the recording writer never fails");
        batches
    }

    /// One code unit past the bound, with the surrogate pair away from it.
    fn text_crossing_the_batch_bound() -> String {
        let mut text = String::from("😀");
        while text.encode_utf16().count() < PINNED_BATCH + 1 {
            text.push('a');
        }
        text
    }

    /// No batch may exceed the bound, and none may end on a high surrogate:
    /// that half's pair would be submitted in a separate call.
    fn assert_batches_are_bounded_and_whole(batches: &[Vec<u16>]) {
        for (index, batch) in batches.iter().enumerate() {
            assert!(
                batch.len() <= PINNED_BATCH,
                "batch {index} holds {} code units, past the 2048 bound",
                batch.len()
            );
            assert!(
                !batch.last().copied().is_some_and(is_high_surrogate),
                "batch {index} ends on a high surrogate, dividing a pair across \
                 two WriteConsoleInputW calls"
            );
        }
    }

    #[test]
    fn console_text_is_written_in_bounded_batches_without_losing_code_units() {
        assert_eq!(
            CONSOLE_TEXT_KEY_BATCH, PINNED_BATCH,
            "the console record batch bound is a contract, not an implementation detail"
        );
        let text = text_crossing_the_batch_bound();
        let expected = text.encode_utf16().collect::<Vec<_>>();
        assert_eq!(expected.len(), PINNED_BATCH + 1);

        let batches = batched_code_units(&text);

        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![PINNED_BATCH, 1],
            "a payload one unit past the bound must produce exactly 2048 + 1"
        );
        assert_eq!(
            batches.concat(),
            expected,
            "batching must not truncate or reorder the paste"
        );
        assert_batches_are_bounded_and_whole(&batches);
    }

    #[test]
    fn a_surrogate_pair_astride_the_batch_bound_is_never_divided_between_writer_calls() {
        // 2047 ASCII units then the pair, so splitting on the bound alone would
        // end batch one on the high half.
        let mut text = "a".repeat(PINNED_BATCH - 1);
        text.push('😀');
        let expected = text.encode_utf16().collect::<Vec<_>>();
        assert_eq!(expected.len(), PINNED_BATCH + 1);

        let batches = batched_code_units(&text);

        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![PINNED_BATCH - 1, 2],
            "the batch must close one unit early so the pair stays in one call"
        );
        assert_eq!(
            batches.concat(),
            expected,
            "closing early must not truncate or reorder the paste"
        );
        assert_eq!(
            batches[1],
            vec![0xd83d, 0xde00],
            "both halves must reach the same WriteConsoleInputW call, in order"
        );
        assert_batches_are_bounded_and_whole(&batches);
    }

    /// The early close is for pairs only: an ordinary BMP payload must still
    /// fill every batch to the bound.
    #[test]
    fn a_bmp_payload_still_fills_each_batch_to_the_bound() {
        let text = "β".repeat(PINNED_BATCH * 2 + 5);

        let batches = batched_code_units(&text);

        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![PINNED_BATCH, PINNED_BATCH, 5],
            "only a straddling surrogate pair may close a batch early"
        );
        assert_eq!(batches.concat(), text.encode_utf16().collect::<Vec<_>>());
        assert_batches_are_bounded_and_whole(&batches);
    }

    #[test]
    fn ctrl_d_eot_is_suppressible_cooked_event_without_scan_code() {
        let key = WindowsConsoleKeyEvent::ctrl_d_eot();

        assert_eq!(key.virtual_key_code, b'D' as u16);
        assert_eq!(key.virtual_scan_code, 0);
        assert_eq!(key.unicode_char, 0x04);
        assert_eq!(key.control_key_state & LEFT_CTRL_PRESSED, LEFT_CTRL_PRESSED);
    }

    #[test]
    fn ctrl_d_keeps_native_scan_code_for_cmd_console_keys() {
        let key = WindowsConsoleKeyEvent::ctrl_d();

        assert_eq!(key.virtual_key_code, b'D' as u16);
        assert_eq!(key.virtual_scan_code, 0x20);
        assert_eq!(key.unicode_char, 0x04);
        assert_eq!(key.control_key_state & LEFT_CTRL_PRESSED, LEFT_CTRL_PRESSED);
    }

    #[test]
    fn typed_ctrl_d_is_suppressed_only_in_processed_input_mode() {
        let typed = WindowsConsoleKeyEvent::ctrl_d_eot();
        assert!(should_suppress_typed_ctrl_d_for_mode(
            ENABLE_PROCESSED_INPUT,
            typed
        ));
        assert!(!should_suppress_typed_ctrl_d_for_mode(
            ENABLE_LINE_INPUT,
            typed
        ));
        assert!(!should_suppress_typed_ctrl_d_for_mode(
            ENABLE_PROCESSED_INPUT,
            WindowsConsoleKeyEvent::ctrl_d()
        ));
    }
}
