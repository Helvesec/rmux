use std::io;
use std::os::windows::io::RawHandle;

use rmux_proto::AttachedWindowsConsoleKey;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{
    GetConsoleMode, ReadConsoleInputW, FROM_LEFT_1ST_BUTTON_PRESSED, FROM_LEFT_2ND_BUTTON_PRESSED,
    FROM_LEFT_3RD_BUTTON_PRESSED, INPUT_RECORD, KEY_EVENT, KEY_EVENT_RECORD, LEFT_ALT_PRESSED,
    LEFT_CTRL_PRESSED, MOUSE_EVENT, MOUSE_EVENT_RECORD, MOUSE_HWHEELED, MOUSE_MOVED, MOUSE_WHEELED,
    RIGHTMOST_BUTTON_PRESSED, RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED, SHIFT_PRESSED,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F10, VK_F11, VK_F12, VK_F2, VK_F3,
    VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_HOME, VK_INSERT, VK_LEFT, VK_NEXT, VK_PRIOR,
    VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
};

const ATTACH_INPUT_CHUNK_LIMIT: usize = 4096;
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";
const CONSOLE_INPUT_RECORD_BATCH: usize = 32;
const HIGH_SURROGATE_START: u16 = 0xd800;
const HIGH_SURROGATE_END: u16 = 0xdbff;
const LOW_SURROGATE_START: u16 = 0xdc00;
const LOW_SURROGATE_END: u16 = 0xdfff;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AttachInput {
    bytes: Vec<u8>,
    windows_console_key: Option<AttachedWindowsConsoleKey>,
}

impl AttachInput {
    pub(super) fn bytes(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            windows_console_key: None,
        }
    }

    pub(super) fn with_windows_console_key(bytes: Vec<u8>, key: AttachedWindowsConsoleKey) -> Self {
        Self {
            bytes,
            windows_console_key: Some(key),
        }
    }

    pub(super) fn payload(&self) -> &[u8] {
        &self.bytes
    }

    pub(super) fn windows_console_key(&self) -> Option<AttachedWindowsConsoleKey> {
        self.windows_console_key
    }
}

pub(super) struct ConsoleInputReader {
    handle: HANDLE,
    pending_high_surrogate: Option<u16>,
    last_mouse_button_state: u32,
}

impl ConsoleInputReader {
    pub(super) fn from_handle(handle: RawHandle) -> Option<Self> {
        let handle = handle as HANDLE;
        let mut mode = 0;
        let ok = unsafe {
            // SAFETY: `mode` is writable and `handle` is only borrowed for this capability probe.
            GetConsoleMode(handle, &mut mode)
        };
        (ok != 0).then_some(Self {
            handle,
            pending_high_surrogate: None,
            last_mouse_button_state: 0,
        })
    }

    pub(super) fn read_key_inputs(&mut self) -> io::Result<Vec<AttachInput>> {
        let mut records = [INPUT_RECORD::default(); CONSOLE_INPUT_RECORD_BATCH];
        let mut records_read = 0;
        let ok = unsafe {
            // SAFETY: `records` points to writable INPUT_RECORD storage and `records_read` is a
            // valid output pointer. The console handle is borrowed for the duration of the call.
            ReadConsoleInputW(
                self.handle,
                records.as_mut_ptr(),
                records.len() as u32,
                &mut records_read,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut inputs = Vec::new();
        for record in &records[..records_read as usize] {
            match u32::from(record.EventType) {
                KEY_EVENT => {
                    let event = unsafe {
                        // SAFETY: EventType says this union currently contains a KEY_EVENT_RECORD.
                        record.Event.KeyEvent
                    };
                    let event = ConsoleKeyEvent::from_win32(event);
                    let bytes = encode_key_event(event, &mut self.pending_high_surrogate);
                    if bytes.is_empty() {
                        continue;
                    }
                    let input = windows_console_key_for_event(event, &bytes).map_or_else(
                        || AttachInput::bytes(bytes.clone()),
                        |key| AttachInput::with_windows_console_key(bytes.clone(), key),
                    );
                    inputs.push(input);
                }
                MOUSE_EVENT => {
                    let event = unsafe {
                        // SAFETY: EventType says this union currently holds a MOUSE_EVENT_RECORD.
                        record.Event.MouseEvent
                    };
                    if let Some(bytes) =
                        encode_mouse_event(event, &mut self.last_mouse_button_state)
                    {
                        inputs.push(AttachInput::bytes(bytes));
                    }
                }
                _ => {}
            }
        }
        Ok(inputs)
    }
}

// SGR mouse button codes (xterm). Modifiers are OR-ed in; drag adds 32; wheel uses 64/65.
const SGR_BTN_LEFT: u16 = 0;
const SGR_BTN_MIDDLE: u16 = 1;
const SGR_BTN_RIGHT: u16 = 2;
const SGR_WHEEL_UP: u16 = 64;
const SGR_WHEEL_DOWN: u16 = 65;
const SGR_MOD_SHIFT: u16 = 4;
const SGR_MOD_ALT: u16 = 8;
const SGR_MOD_CTRL: u16 = 16;
const SGR_DRAG_FLAG: u16 = 32;

/// Encode a Win32 console `MOUSE_EVENT_RECORD` into an SGR mouse sequence
/// (`\x1b[<b;x;yM` press/wheel, `\x1b[<b;x;ym` release). The rmux server gates these
/// against the active mouse mode, so we always emit and let the server decide.
fn encode_mouse_event(event: MOUSE_EVENT_RECORD, last_button_state: &mut u32) -> Option<Vec<u8>> {
    let x = u16::try_from(event.dwMousePosition.X.max(0)).unwrap_or(0) + 1;
    let y = u16::try_from(event.dwMousePosition.Y.max(0)).unwrap_or(0) + 1;
    let buttons = event.dwButtonState;
    let modifiers = sgr_modifier_bits(event.dwControlKeyState);

    if event.dwEventFlags & MOUSE_WHEELED != 0 {
        // High word of dwButtonState is the signed wheel delta; positive = up.
        let delta = (buttons >> 16) as i16;
        let base = if delta >= 0 {
            SGR_WHEEL_UP
        } else {
            SGR_WHEEL_DOWN
        };
        return Some(format_sgr_mouse(base | modifiers, x, y, 'M'));
    }
    if event.dwEventFlags & MOUSE_HWHEELED != 0 {
        return None;
    }

    // Ignore bare double-click flags; movement is handled below.
    let previous = *last_button_state;
    *last_button_state = buttons;
    let pressed = buttons & !previous;
    let released = previous & !buttons;

    let is_move = event.dwEventFlags & MOUSE_MOVED != 0;

    if pressed == 0 && released == 0 {
        if is_move {
            // Drag (motion with a button held) or bare hover motion.
            if let Some(button) = sgr_button_for_state(buttons) {
                return Some(format_sgr_mouse(
                    button | SGR_DRAG_FLAG | modifiers,
                    x,
                    y,
                    'M',
                ));
            }
        }
        return None;
    }

    if let Some(button) = sgr_button_for_state(pressed) {
        Some(format_sgr_mouse(button | modifiers, x, y, 'M'))
    } else {
        sgr_button_for_state(released).map(|button| format_sgr_mouse(button | modifiers, x, y, 'm'))
    }
}

fn sgr_button_for_state(buttons: u32) -> Option<u16> {
    if buttons & FROM_LEFT_1ST_BUTTON_PRESSED != 0 {
        Some(SGR_BTN_LEFT)
    } else if buttons & RIGHTMOST_BUTTON_PRESSED != 0 {
        Some(SGR_BTN_RIGHT)
    } else if buttons & (FROM_LEFT_2ND_BUTTON_PRESSED | FROM_LEFT_3RD_BUTTON_PRESSED) != 0 {
        Some(SGR_BTN_MIDDLE)
    } else {
        None
    }
}

fn sgr_modifier_bits(control_key_state: u32) -> u16 {
    let mut bits = 0;
    if control_key_state & SHIFT_PRESSED != 0 {
        bits |= SGR_MOD_SHIFT;
    }
    if control_key_state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0 {
        bits |= SGR_MOD_ALT;
    }
    if control_key_state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0 {
        bits |= SGR_MOD_CTRL;
    }
    bits
}

fn format_sgr_mouse(button: u16, x: u16, y: u16, terminator: char) -> Vec<u8> {
    format!("\x1b[<{button};{x};{y}{terminator}").into_bytes()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConsoleKeyEvent {
    key_down: bool,
    repeat_count: u16,
    virtual_key_code: u16,
    virtual_scan_code: u16,
    unicode_char: u16,
    control_key_state: u32,
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

fn windows_console_key_for_event(
    event: ConsoleKeyEvent,
    encoded_bytes: &[u8],
) -> Option<AttachedWindowsConsoleKey> {
    if !event.key_down
        || encoded_bytes.is_empty()
        || !ctrl_pressed(event.control_key_state)
        || alt_gr_pressed(event.control_key_state)
        || meta_pressed(event.control_key_state)
    {
        return None;
    }

    let unicode_char = if event.unicode_char == 0
        && encoded_bytes.len() == 1
        && is_windows_ctrl_key_byte(encoded_bytes[0])
    {
        u16::from(encoded_bytes[0])
    } else {
        event.unicode_char
    };
    Some(AttachedWindowsConsoleKey::new(
        event.virtual_key_code,
        event.virtual_scan_code,
        unicode_char,
        event.control_key_state,
        event.repeat_count.max(1),
    ))
}

fn is_windows_ctrl_key_byte(byte: u8) -> bool {
    matches!(byte, 0x00..=0x1a | 0x1c..=0x1f | 0x7f)
}

fn encode_key_event(event: ConsoleKeyEvent, pending_high_surrogate: &mut Option<u16>) -> Vec<u8> {
    if !event.key_down {
        return Vec::new();
    }

    let repeat_count = usize::from(event.repeat_count.max(1));
    let mut once = if event.unicode_char != 0 && !virtual_key_requires_modifier_mapping(event) {
        encode_unicode_key_event(event, pending_high_surrogate)
    } else {
        pending_high_surrogate.take();
        encode_virtual_key_event(event)
    };

    if once.is_empty() || repeat_count == 1 {
        return once;
    }

    let single = once.clone();
    once.reserve(single.len().saturating_mul(repeat_count.saturating_sub(1)));
    for _ in 1..repeat_count {
        once.extend_from_slice(&single);
    }
    once
}

fn virtual_key_requires_modifier_mapping(event: ConsoleKeyEvent) -> bool {
    matches!(
        event.virtual_key_code,
        VK_BACK | VK_ESCAPE | VK_RETURN | VK_TAB
    )
}

fn encode_unicode_key_event(
    event: ConsoleKeyEvent,
    pending_high_surrogate: &mut Option<u16>,
) -> Vec<u8> {
    let alt = meta_pressed(event.control_key_state);
    let ctrl = ctrl_pressed(event.control_key_state) && !alt_gr_pressed(event.control_key_state);

    if ctrl {
        if let Some(control) = control_byte_for_event(event) {
            return with_meta_prefix(alt, &[control]);
        }
    }

    let Some(character) = char_from_utf16_event(event.unicode_char, pending_high_surrogate) else {
        return Vec::new();
    };
    let mut utf8 = [0; 4];
    with_meta_prefix(alt, character.encode_utf8(&mut utf8).as_bytes())
}

fn encode_virtual_key_event(event: ConsoleKeyEvent) -> Vec<u8> {
    let state = event.control_key_state;
    let alt = meta_pressed(state);
    let modifier = xterm_modifier_parameter(state);
    let key = event.virtual_key_code;

    if key == VK_ESCAPE {
        return if alt {
            b"\x1b\x1b".to_vec()
        } else {
            b"\x1b".to_vec()
        };
    }
    if key == VK_SPACE && ctrl_pressed(state) && !alt_gr_pressed(state) {
        return with_meta_prefix(alt, &[0x00]);
    }
    if ctrl_pressed(state) && !alt_gr_pressed(state) {
        if let Some(control) = control_byte_for_virtual_key(key) {
            return with_meta_prefix(alt, &[control]);
        }
    }
    if key == VK_BACK {
        return if modifier == 1 {
            b"\x7f".to_vec()
        } else {
            csi_u_sequence(0x7f, modifier)
        };
    }
    if key == VK_TAB {
        return match modifier {
            1 => b"\t".to_vec(),
            2 => b"\x1b[Z".to_vec(),
            _ => csi_u_sequence(0x09, modifier),
        };
    }
    if key == VK_RETURN {
        return if modifier == 1 {
            b"\r".to_vec()
        } else {
            csi_u_sequence(0x0d, modifier)
        };
    }

    if let Some((normal, modified_final)) = cursor_key_sequence(key) {
        return if modifier == 1 {
            normal.to_vec()
        } else {
            format!("\x1b[1;{modifier}{}", char::from(modified_final)).into_bytes()
        };
    }
    if let Some(number) = tilde_key_number(key) {
        return if modifier == 1 {
            format!("\x1b[{number}~").into_bytes()
        } else {
            format!("\x1b[{number};{modifier}~").into_bytes()
        };
    }
    if let Some((normal, modified_final)) = function_key_sequence(key) {
        return if modifier == 1 {
            normal.to_vec()
        } else {
            format!("\x1b[1;{modifier}{}", char::from(modified_final)).into_bytes()
        };
    }

    Vec::new()
}

fn char_from_utf16_event(value: u16, pending_high_surrogate: &mut Option<u16>) -> Option<char> {
    if (HIGH_SURROGATE_START..=HIGH_SURROGATE_END).contains(&value) {
        *pending_high_surrogate = Some(value);
        return None;
    }

    if let Some(high) = pending_high_surrogate.take() {
        if (LOW_SURROGATE_START..=LOW_SURROGATE_END).contains(&value) {
            let high = u32::from(high - HIGH_SURROGATE_START);
            let low = u32::from(value - LOW_SURROGATE_START);
            return char::from_u32(0x10000 + ((high << 10) | low));
        }
    }

    char::from_u32(u32::from(value))
}

fn control_byte_for_event(event: ConsoleKeyEvent) -> Option<u8> {
    if (1..=0x1a).contains(&event.unicode_char) {
        return Some(event.unicode_char as u8);
    }
    match event.virtual_key_code {
        value if value == VK_SPACE => return Some(0x00),
        _ => {}
    }

    let character = char::from_u32(u32::from(event.unicode_char))?;
    let character = character.to_ascii_lowercase();
    match character {
        'a'..='z' => Some((character as u8 - b'a') + 1),
        ' ' | '@' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

fn control_byte_for_virtual_key(key: u16) -> Option<u8> {
    match key {
        0x41..=0x5a => Some((key as u8 - b'A') + 1),
        0x32 => Some(0x00),
        0x33 => Some(0x1b),
        0x34 => Some(0x1c),
        0x35 => Some(0x1d),
        0x36 => Some(0x1e),
        0x5f => Some(0x1f),
        _ => None,
    }
}

fn with_meta_prefix(meta: bool, bytes: &[u8]) -> Vec<u8> {
    if !meta {
        return bytes.to_vec();
    }
    let mut output = Vec::with_capacity(bytes.len() + 1);
    output.push(0x1b);
    output.extend_from_slice(bytes);
    output
}

fn ctrl_pressed(state: u32) -> bool {
    state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0
}

fn meta_pressed(state: u32) -> bool {
    state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0 && !alt_gr_pressed(state)
}

fn shift_pressed(state: u32) -> bool {
    state & SHIFT_PRESSED != 0
}

fn alt_gr_pressed(state: u32) -> bool {
    state & RIGHT_ALT_PRESSED != 0
        && state & LEFT_CTRL_PRESSED != 0
        && state & LEFT_ALT_PRESSED == 0
        && state & RIGHT_CTRL_PRESSED == 0
}

fn xterm_modifier_parameter(state: u32) -> u8 {
    let shift = shift_pressed(state);
    let meta = meta_pressed(state);
    let ctrl = ctrl_pressed(state) && !alt_gr_pressed(state);
    1 + u8::from(shift) + (u8::from(meta) * 2) + (u8::from(ctrl) * 4)
}

fn csi_u_sequence(key: u32, modifier: u8) -> Vec<u8> {
    format!("\x1b[{key};{modifier}u").into_bytes()
}

fn cursor_key_sequence(key: u16) -> Option<(&'static [u8], u8)> {
    match key {
        value if value == VK_UP => Some((b"\x1b[A", b'A')),
        value if value == VK_DOWN => Some((b"\x1b[B", b'B')),
        value if value == VK_RIGHT => Some((b"\x1b[C", b'C')),
        value if value == VK_LEFT => Some((b"\x1b[D", b'D')),
        value if value == VK_HOME => Some((b"\x1b[H", b'H')),
        value if value == VK_END => Some((b"\x1b[F", b'F')),
        _ => None,
    }
}

fn tilde_key_number(key: u16) -> Option<u8> {
    match key {
        value if value == VK_INSERT => Some(2),
        value if value == VK_DELETE => Some(3),
        value if value == VK_PRIOR => Some(5),
        value if value == VK_NEXT => Some(6),
        value if value == VK_F5 => Some(15),
        value if value == VK_F6 => Some(17),
        value if value == VK_F7 => Some(18),
        value if value == VK_F8 => Some(19),
        value if value == VK_F9 => Some(20),
        value if value == VK_F10 => Some(21),
        value if value == VK_F11 => Some(23),
        value if value == VK_F12 => Some(24),
        _ => None,
    }
}

fn function_key_sequence(key: u16) -> Option<(&'static [u8], u8)> {
    match key {
        value if value == VK_F1 => Some((b"\x1bOP", b'P')),
        value if value == VK_F2 => Some((b"\x1bOQ", b'Q')),
        value if value == VK_F3 => Some((b"\x1bOR", b'R')),
        value if value == VK_F4 => Some((b"\x1bOS", b'S')),
        _ => None,
    }
}

pub(super) fn attach_input_chunks(bytes: &[u8]) -> AttachInputChunks<'_> {
    AttachInputChunks { bytes, offset: 0 }
}

pub(super) struct AttachInputChunks<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Iterator for AttachInputChunks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.bytes.len() {
            return None;
        }

        let start = self.offset;
        let ideal_end = start
            .saturating_add(ATTACH_INPUT_CHUNK_LIMIT)
            .min(self.bytes.len());
        let end = if ideal_end == self.bytes.len() {
            ideal_end
        } else {
            bounded_chunk_end(self.bytes, start, ideal_end)
        };
        self.offset = end;
        Some(&self.bytes[start..end])
    }
}

fn bounded_chunk_end(bytes: &[u8], start: usize, ideal_end: usize) -> usize {
    let end = avoid_utf8_split(bytes, start, ideal_end);
    let end = avoid_bracketed_paste_marker_split(bytes, start, end);
    if end > start {
        end
    } else {
        ideal_end
    }
}

fn avoid_utf8_split(bytes: &[u8], start: usize, mut end: usize) -> usize {
    while end > start
        && end < bytes.len()
        && bytes
            .get(end)
            .is_some_and(|byte| is_utf8_continuation(*byte))
    {
        end -= 1;
    }
    end
}

fn is_utf8_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

fn avoid_bracketed_paste_marker_split(bytes: &[u8], start: usize, end: usize) -> usize {
    for marker in [BRACKETED_PASTE_START, BRACKETED_PASTE_END] {
        if let Some(adjusted) = marker_adjusted_end(bytes, start, end, marker) {
            return adjusted;
        }
    }
    end
}

fn marker_adjusted_end(bytes: &[u8], start: usize, end: usize, marker: &[u8]) -> Option<usize> {
    let search_start = end
        .saturating_sub(marker.len().saturating_sub(1))
        .max(start);
    for marker_start in search_start..end {
        let prefix = &bytes[marker_start..end];
        if !prefix.is_empty()
            && marker.starts_with(prefix)
            && marker_start + marker.len() <= bytes.len()
        {
            return Some(marker_start + marker.len());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        attach_input_chunks, encode_key_event, encode_mouse_event, windows_console_key_for_event,
        ConsoleKeyEvent, ATTACH_INPUT_CHUNK_LIMIT, BRACKETED_PASTE_END, BRACKETED_PASTE_START,
    };
    use windows_sys::Win32::System::Console::{
        FROM_LEFT_1ST_BUTTON_PRESSED, FROM_LEFT_2ND_BUTTON_PRESSED, LEFT_ALT_PRESSED,
        LEFT_CTRL_PRESSED, MOUSE_EVENT_RECORD, MOUSE_HWHEELED, MOUSE_MOVED, MOUSE_WHEELED,
        RIGHTMOST_BUTTON_PRESSED, RIGHT_ALT_PRESSED, SHIFT_PRESSED,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F5, VK_HOME, VK_LEFT, VK_RETURN,
        VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
    };

    #[test]
    fn paste_chunks_preserve_bracketed_paste_markers() {
        let mut input = vec![b'a'; ATTACH_INPUT_CHUNK_LIMIT - 2];
        input.extend_from_slice(BRACKETED_PASTE_START);
        input.extend_from_slice(b"line one\r\nline two");
        input.extend_from_slice(BRACKETED_PASTE_END);

        let chunks = collect_chunks(&input);

        assert_eq!(chunks.concat(), input);
        assert_eq!(
            chunks[0].len(),
            ATTACH_INPUT_CHUNK_LIMIT - 2 + BRACKETED_PASTE_START.len()
        );
    }

    #[test]
    fn paste_chunks_do_not_split_utf8_scalars() {
        let mut input = vec![b'a'; ATTACH_INPUT_CHUNK_LIMIT - 1];
        input.extend_from_slice("東".as_bytes());
        input.extend_from_slice(" tail".as_bytes());

        let chunks = collect_chunks(&input);

        assert_eq!(chunks.concat(), input);
        assert_eq!(chunks[0].len(), ATTACH_INPUT_CHUNK_LIMIT - 1);
        assert!(std::str::from_utf8(&chunks[1]).is_ok());
    }

    #[test]
    fn paste_chunks_preserve_control_bytes() {
        let mut input = Vec::from([0x02, b'w', 0x03]);
        input.extend(vec![b'x'; ATTACH_INPUT_CHUNK_LIMIT + 32]);

        let chunks = collect_chunks(&input);

        assert_eq!(chunks.concat(), input);
        assert_eq!(&chunks[0][..3], &[0x02, b'w', 0x03]);
    }

    fn collect_chunks(input: &[u8]) -> Vec<Vec<u8>> {
        attach_input_chunks(input)
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>()
    }

    #[test]
    fn console_key_events_encode_ctrl_letters_as_control_bytes() {
        for (letter, expected) in [('a', 0x01), ('c', 0x03), ('l', 0x0c), ('z', 0x1a)] {
            let event = key_event(letter as u16, letter as u16, LEFT_CTRL_PRESSED);
            assert_eq!(
                encode(&event),
                vec![expected],
                "Ctrl+{letter} should preserve the control byte"
            );
        }
    }

    #[test]
    fn console_key_events_preserve_existing_control_chars() {
        let event = key_event('l' as u16, 0x0c, LEFT_CTRL_PRESSED);

        assert_eq!(encode(&event), vec![0x0c]);
    }

    #[test]
    fn console_key_events_preserve_ctrl_d_windows_metadata() {
        let event = key_event('D' as u16, 0x04, LEFT_CTRL_PRESSED);
        let bytes = encode(&event);

        let key = windows_console_key_for_event(event, &bytes)
            .expect("Ctrl-D should preserve Windows console metadata");

        assert_eq!(bytes, vec![0x04]);
        assert_eq!(key.virtual_key_code(), 'D' as u16);
        assert_eq!(key.virtual_scan_code(), 0x20);
        assert_eq!(key.unicode_char(), 0x04);
        assert_eq!(key.control_key_state(), LEFT_CTRL_PRESSED);
        assert_eq!(key.repeat_count(), 1);
    }

    #[test]
    fn console_key_events_preserve_other_ctrl_letter_windows_metadata() {
        let event = key_event('P' as u16, 0x10, LEFT_CTRL_PRESSED);
        let bytes = encode(&event);

        let key = windows_console_key_for_event(event, &bytes)
            .expect("Ctrl-P should preserve Windows console metadata");

        assert_eq!(bytes, vec![0x10]);
        assert_eq!(key.virtual_key_code(), 'P' as u16);
        assert_eq!(key.unicode_char(), 0x10);
    }

    #[test]
    fn console_key_events_preserve_ctrl_c_windows_metadata() {
        let event = key_event('C' as u16, 0x03, LEFT_CTRL_PRESSED);
        let bytes = encode(&event);

        assert_eq!(bytes, vec![0x03]);
        assert!(windows_console_key_for_event(event, &bytes).is_some());
    }

    #[test]
    fn console_key_events_encode_ctrl_virtual_letters_without_unicode_char() {
        for (letter, expected) in [('A', 0x01), ('L', 0x0c), ('Z', 0x1a)] {
            let event = key_event(letter as u16, 0, LEFT_CTRL_PRESSED);
            assert_eq!(
                encode(&event),
                vec![expected],
                "virtual Ctrl+{letter} should preserve the control byte"
            );
        }
    }

    #[test]
    fn console_key_events_encode_ctrl_space_and_alt_ctrl_letters() {
        let ctrl_space = key_event(VK_SPACE, 0, LEFT_CTRL_PRESSED);
        assert_eq!(encode(&ctrl_space), vec![0x00]);

        let alt_ctrl_l = key_event('l' as u16, 'l' as u16, LEFT_ALT_PRESSED | LEFT_CTRL_PRESSED);
        assert_eq!(encode(&alt_ctrl_l), b"\x1b\x0c");
    }

    #[test]
    fn console_key_events_do_not_treat_alt_gr_text_as_ctrl_meta() {
        let event = key_event('e' as u16, 0x20ac, RIGHT_ALT_PRESSED | LEFT_CTRL_PRESSED);

        assert_eq!(encode(&event), "€".as_bytes());
    }

    #[test]
    fn console_key_events_encode_text_and_meta_text() {
        let plain = key_event('x' as u16, 'x' as u16, 0);
        assert_eq!(encode(&plain), b"x");

        let meta = key_event('x' as u16, 'x' as u16, LEFT_ALT_PRESSED);
        assert_eq!(encode(&meta), b"\x1bx");
    }

    #[test]
    fn console_key_events_encode_navigation_with_modifiers() {
        assert_eq!(encode(&key_event(VK_UP, 0, 0)), b"\x1b[A");
        assert_eq!(
            encode(&key_event(VK_LEFT, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[1;5D"
        );
        assert_eq!(
            encode(&key_event(VK_RIGHT, 0, SHIFT_PRESSED | LEFT_CTRL_PRESSED)),
            b"\x1b[1;6C"
        );
        assert_eq!(
            encode(&key_event(VK_HOME, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[1;5H"
        );
        assert_eq!(
            encode(&key_event(VK_END, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[1;5F"
        );
        assert_eq!(
            encode(&key_event(VK_DELETE, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[3;5~"
        );
        assert_eq!(
            encode(&key_event(VK_F5, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[15;5~"
        );
        assert_eq!(encode(&key_event(VK_DOWN, 0, 0)), b"\x1b[B");
    }

    #[test]
    fn console_key_events_encode_enter_tab_escape_and_backspace() {
        assert_eq!(encode(&key_event(VK_RETURN, 0, 0)), b"\r");
        assert_eq!(
            encode(&key_event(VK_RETURN, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[13;5u"
        );
        assert_eq!(encode(&key_event(VK_TAB, 0, SHIFT_PRESSED)), b"\x1b[Z");
        assert_eq!(
            encode(&key_event(VK_TAB, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[9;5u"
        );
        assert_eq!(encode(&key_event(VK_ESCAPE, 0, 0)), b"\x1b");
        assert_eq!(
            encode(&key_event(VK_ESCAPE, 0, LEFT_ALT_PRESSED)),
            b"\x1b\x1b"
        );
        assert_eq!(encode(&key_event(VK_BACK, 0, 0)), b"\x7f");
        assert_eq!(
            encode(&key_event(VK_BACK, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[127;5u"
        );
    }

    #[test]
    fn console_key_events_map_windows_control_unicode_through_virtual_keys() {
        assert_eq!(encode(&key_event(VK_BACK, 0x08, 0)), b"\x7f");
        assert_eq!(
            encode(&key_event(VK_BACK, 0x08, LEFT_CTRL_PRESSED)),
            b"\x1b[127;5u"
        );
        assert_eq!(encode(&key_event(VK_TAB, 0x09, SHIFT_PRESSED)), b"\x1b[Z");
        assert_eq!(
            encode(&key_event(VK_TAB, 0x09, LEFT_CTRL_PRESSED)),
            b"\x1b[9;5u"
        );
        assert_eq!(
            encode(&key_event(VK_RETURN, 0x0d, LEFT_CTRL_PRESSED)),
            b"\x1b[13;5u"
        );
        assert_eq!(
            encode(&key_event(VK_ESCAPE, 0x1b, LEFT_ALT_PRESSED)),
            b"\x1b\x1b"
        );
    }

    #[test]
    fn console_key_events_repeat_encoded_bytes() {
        let mut event = key_event('x' as u16, 'x' as u16, 0);
        event.repeat_count = 3;

        assert_eq!(encode(&event), b"xxx");
    }

    #[test]
    fn console_key_events_ignore_key_up() {
        let mut event = key_event('x' as u16, 'x' as u16, 0);
        event.key_down = false;

        assert!(encode(&event).is_empty());
    }

    fn encode(event: &ConsoleKeyEvent) -> Vec<u8> {
        encode_key_event(*event, &mut None)
    }

    fn key_event(
        virtual_key_code: u16,
        unicode_char: u16,
        control_key_state: u32,
    ) -> ConsoleKeyEvent {
        ConsoleKeyEvent {
            key_down: true,
            repeat_count: 1,
            virtual_key_code,
            virtual_scan_code: 0x20,
            unicode_char,
            control_key_state,
        }
    }

    fn mouse_event(
        button_state: u32,
        event_flags: u32,
        control_key_state: u32,
        x: i16,
        y: i16,
    ) -> MOUSE_EVENT_RECORD {
        MOUSE_EVENT_RECORD {
            dwMousePosition: windows_sys::Win32::System::Console::COORD { X: x, Y: y },
            dwButtonState: button_state,
            dwControlKeyState: control_key_state,
            dwEventFlags: event_flags,
        }
    }

    fn encode_mouse(event: MOUSE_EVENT_RECORD, last: &mut u32) -> Option<String> {
        encode_mouse_event(event, last).map(|bytes| String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn mouse_wheel_up_encodes_sgr_button_64() {
        // High word of dwButtonState carries the signed wheel delta; positive = up.
        let event = mouse_event(0x0078_0000, MOUSE_WHEELED, 0, 9, 14);
        let mut last = 0;
        // Coordinates are 0-based from the console and become 1-based in SGR.
        assert_eq!(
            encode_mouse(event, &mut last).as_deref(),
            Some("\x1b[<64;10;15M")
        );
    }

    #[test]
    fn mouse_wheel_down_encodes_sgr_button_65() {
        let event = mouse_event(0xff88_0000, MOUSE_WHEELED, 0, 0, 0);
        let mut last = 0;
        assert_eq!(
            encode_mouse(event, &mut last).as_deref(),
            Some("\x1b[<65;1;1M")
        );
    }

    #[test]
    fn mouse_left_press_then_release_encodes_press_and_sgr_release() {
        let mut last = 0;
        let press = mouse_event(FROM_LEFT_1ST_BUTTON_PRESSED, 0, 0, 4, 4);
        assert_eq!(
            encode_mouse(press, &mut last).as_deref(),
            Some("\x1b[<0;5;5M")
        );
        // SGR release uses the released button identity plus the 'm' terminator.
        let release = mouse_event(0, 0, 0, 4, 4);
        assert_eq!(
            encode_mouse(release, &mut last).as_deref(),
            Some("\x1b[<0;5;5m")
        );
    }

    #[test]
    fn mouse_middle_release_preserves_sgr_button_identity() {
        let mut last = 0;
        let press = mouse_event(FROM_LEFT_2ND_BUTTON_PRESSED, 0, 0, 8, 3);
        assert_eq!(
            encode_mouse(press, &mut last).as_deref(),
            Some("\x1b[<1;9;4M")
        );

        let release = mouse_event(0, 0, SHIFT_PRESSED, 8, 3);
        assert_eq!(
            encode_mouse(release, &mut last).as_deref(),
            Some("\x1b[<5;9;4m")
        );
    }

    #[test]
    fn mouse_left_drag_sets_drag_flag() {
        let mut last = 0;
        let _ = encode_mouse(
            mouse_event(FROM_LEFT_1ST_BUTTON_PRESSED, 0, 0, 1, 1),
            &mut last,
        );
        // Motion while the left button stays held -> drag (button 0 | 32).
        let drag = mouse_event(FROM_LEFT_1ST_BUTTON_PRESSED, MOUSE_MOVED, 0, 2, 1);
        assert_eq!(
            encode_mouse(drag, &mut last).as_deref(),
            Some("\x1b[<32;3;2M")
        );
    }

    #[test]
    fn mouse_right_press_encodes_button_2() {
        let mut last = 0;
        let press = mouse_event(RIGHTMOST_BUTTON_PRESSED, 0, 0, 0, 0);
        assert_eq!(
            encode_mouse(press, &mut last).as_deref(),
            Some("\x1b[<2;1;1M")
        );
    }

    #[test]
    fn mouse_second_button_press_uses_changed_button_not_aggregate_state() {
        let mut last = 0;
        let left = mouse_event(FROM_LEFT_1ST_BUTTON_PRESSED, 0, 0, 0, 0);
        assert_eq!(
            encode_mouse(left, &mut last).as_deref(),
            Some("\x1b[<0;1;1M")
        );

        let left_and_right = mouse_event(
            FROM_LEFT_1ST_BUTTON_PRESSED | RIGHTMOST_BUTTON_PRESSED,
            0,
            0,
            1,
            0,
        );
        assert_eq!(
            encode_mouse(left_and_right, &mut last).as_deref(),
            Some("\x1b[<2;2;1M")
        );
    }

    #[test]
    fn mouse_partial_release_uses_released_button_not_remaining_button() {
        let mut last = 0;
        let both = mouse_event(
            FROM_LEFT_1ST_BUTTON_PRESSED | RIGHTMOST_BUTTON_PRESSED,
            0,
            0,
            1,
            1,
        );
        let _ = encode_mouse(both, &mut last);

        let left_remaining = mouse_event(FROM_LEFT_1ST_BUTTON_PRESSED, 0, 0, 1, 1);
        assert_eq!(
            encode_mouse(left_remaining, &mut last).as_deref(),
            Some("\x1b[<2;2;2m")
        );
    }

    #[test]
    fn mouse_modifiers_are_or_ed_into_button() {
        let mut last = 0;
        // Ctrl+Shift wheel up -> 64 | 4 (shift) | 16 (ctrl) = 84.
        let event = mouse_event(
            0x0078_0000,
            MOUSE_WHEELED,
            SHIFT_PRESSED | LEFT_CTRL_PRESSED,
            0,
            0,
        );
        assert_eq!(
            encode_mouse(event, &mut last).as_deref(),
            Some("\x1b[<84;1;1M")
        );
    }

    #[test]
    fn mouse_horizontal_wheel_is_ignored_without_polluting_button_state() {
        let mut last = FROM_LEFT_1ST_BUTTON_PRESSED;
        let event = mouse_event(0x0078_0000, MOUSE_HWHEELED, 0, 3, 3);

        assert_eq!(encode_mouse(event, &mut last), None);
        assert_eq!(last, FROM_LEFT_1ST_BUTTON_PRESSED);
    }

    #[test]
    fn mouse_idle_move_without_button_is_ignored() {
        let mut last = 0;
        let event = mouse_event(0, MOUSE_MOVED, 0, 5, 5);
        assert_eq!(encode_mouse(event, &mut last), None);
    }
}
