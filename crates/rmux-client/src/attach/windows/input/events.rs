use windows_sys::Win32::System::Console::{
    FOCUS_EVENT, INPUT_RECORD, KEY_EVENT, KEY_EVENT_RECORD, MOUSE_EVENT, MOUSE_EVENT_RECORD,
};

use super::{
    encode_key_event, encode_mouse_event, trace_console_key_event, trace_windows_console_key,
    windows_console_key_for_event, AttachInput,
};

pub(super) enum BatchEvent {
    Key(ConsoleKeyEvent),
    Mouse(MOUSE_EVENT_RECORD),
    Focus(bool),
}

pub(super) fn append_batch_events(records: &[INPUT_RECORD], events: &mut Vec<BatchEvent>) {
    for record in records {
        match u32::from(record.EventType) {
            KEY_EVENT => {
                let event = unsafe {
                    // SAFETY: EventType says this union currently contains a KEY_EVENT_RECORD.
                    record.Event.KeyEvent
                };
                let key = ConsoleKeyEvent::from_win32(event);
                trace_console_key_event(key);
                events.push(BatchEvent::Key(key));
            }
            MOUSE_EVENT => {
                let event = unsafe {
                    // SAFETY: EventType says this union currently holds a MOUSE_EVENT_RECORD.
                    record.Event.MouseEvent
                };
                events.push(BatchEvent::Mouse(event));
            }
            FOCUS_EVENT => {
                let event = unsafe {
                    // SAFETY: EventType identifies the active FOCUS_EVENT_RECORD union member.
                    record.Event.FocusEvent
                };
                events.push(BatchEvent::Focus(event.bSetFocus != 0));
            }
            _ => {}
        }
    }
}

pub(super) fn encode_live_input_batch(
    events: &[BatchEvent],
    pending_high_surrogate: &mut Option<u16>,
    last_mouse_button_state: &mut u32,
) -> Vec<AttachInput> {
    let mut inputs = Vec::new();
    for event in events {
        match event {
            BatchEvent::Key(key) => {
                let logical_key = ConsoleKeyEvent {
                    repeat_count: 1,
                    ..*key
                };
                let bytes = encode_key_event(logical_key, pending_high_surrogate);
                if bytes.is_empty() {
                    continue;
                }
                let input = windows_console_key_for_event(*key, &bytes).map_or_else(
                    || AttachInput::repeated_bytes(bytes.clone(), key.repeat_count),
                    |console_key| {
                        trace_windows_console_key(console_key, &bytes);
                        AttachInput::with_windows_console_key(bytes.clone(), console_key)
                    },
                );
                inputs.push(input);
            }
            BatchEvent::Focus(focused) => {
                inputs.push(AttachInput::bytes(focus_bytes(*focused).to_vec()));
            }
            BatchEvent::Mouse(mouse) => {
                for bytes in encode_mouse_event(*mouse, last_mouse_button_state) {
                    inputs.push(AttachInput::bytes(bytes));
                }
            }
        }
    }
    inputs
}

pub(super) const fn focus_bytes(focused: bool) -> &'static [u8] {
    if focused {
        b"\x1b[I"
    } else {
        b"\x1b[O"
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ConsoleKeyEvent {
    pub(super) key_down: bool,
    pub(super) repeat_count: u16,
    pub(super) virtual_key_code: u16,
    pub(super) virtual_scan_code: u16,
    pub(super) unicode_char: u16,
    pub(super) control_key_state: u32,
}

impl ConsoleKeyEvent {
    fn from_win32(event: KEY_EVENT_RECORD) -> Self {
        let unicode_char = unsafe {
            // SAFETY: Reading the UnicodeChar arm is valid for KEY_EVENT_RECORD values returned
            // by ReadConsoleInputW.
            event.uChar.UnicodeChar
        };
        Self {
            key_down: event.bKeyDown != 0,
            repeat_count: event.wRepeatCount,
            virtual_key_code: event.wVirtualKeyCode,
            virtual_scan_code: event.wVirtualScanCode,
            unicode_char,
            control_key_state: event.dwControlKeyState,
        }
    }
}
