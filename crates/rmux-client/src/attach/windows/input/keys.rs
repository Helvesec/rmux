use rmux_proto::AttachedWindowsConsoleKey;
use windows_sys::Win32::System::Console::{
    LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED, RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED, SHIFT_PRESSED,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F10, VK_F11, VK_F12, VK_F2, VK_F3,
    VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_HOME, VK_INSERT, VK_LEFT, VK_NEXT, VK_PRIOR,
    VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
};

use super::events::ConsoleKeyEvent;
use super::paste::is_synthesized_paste_lf;

const HIGH_SURROGATE_START: u16 = 0xd800;
const HIGH_SURROGATE_END: u16 = 0xdbff;
const LOW_SURROGATE_START: u16 = 0xdc00;
const LOW_SURROGATE_END: u16 = 0xdfff;

pub(super) fn windows_console_key_for_event(
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

    Some(AttachedWindowsConsoleKey::new(
        event.virtual_key_code,
        event.virtual_scan_code,
        event.unicode_char,
        event.control_key_state,
        event.repeat_count.max(1),
    ))
}

pub(super) fn encode_paste_body_key_event(
    event: ConsoleKeyEvent,
    pending_high_surrogate: &mut Option<u16>,
) -> Vec<u8> {
    if is_synthesized_paste_lf(&event) {
        // Live Ctrl+Enter remains a modified console key, but inside a burst
        // independently identified as paste this record represents the literal
        // U+000A clipboard byte. Encoding it through VK_RETURN would turn it
        // into CSI 13;5u and corrupt the paste body. A UTF-16 pair being
        // decoded across this record is unrelated state: like the Ctrl+J
        // control-byte path this branch generalizes, it returns its byte
        // without disturbing a pending high surrogate, so the low half still
        // finds its partner after the newline.
        return vec![b'\n'; usize::from(event.repeat_count.max(1))];
    }
    encode_key_event(event, pending_high_surrogate)
}

pub(super) fn encode_key_event(
    event: ConsoleKeyEvent,
    pending_high_surrogate: &mut Option<u16>,
) -> Vec<u8> {
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

pub(super) fn ctrl_pressed(state: u32) -> bool {
    state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0
}

pub(super) fn meta_pressed(state: u32) -> bool {
    state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0 && !alt_gr_pressed(state)
}

pub(super) fn shift_pressed(state: u32) -> bool {
    state & SHIFT_PRESSED != 0
}

pub(super) fn alt_gr_pressed(state: u32) -> bool {
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
