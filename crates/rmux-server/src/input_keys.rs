#![allow(dead_code)]

use rmux_core::{
    input::mode, key_code_to_bytes, key_string_lookup_string, KeyCode, KEYC_BSPACE, KEYC_CTRL,
    KEYC_CURSOR, KEYC_IMPLIED_META, KEYC_KEYPAD, KEYC_MASK_KEY, KEYC_MASK_MODIFIERS,
    KEYC_MASK_TYPE, KEYC_META, KEYC_SHIFT,
};

#[path = "input_keys/mouse.rs"]
mod mouse;

#[cfg(test)]
pub(crate) use self::mouse::MAX_SGR_MOUSE_FRAME_BYTES;
#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use self::mouse::{decode_mouse, encode_mouse_event, MouseDecode, MouseForwardEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExtendedKeyFormat {
    Xterm,
    CsiU,
}

impl ExtendedKeyFormat {
    pub(crate) fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or("xterm") {
            "csi-u" => Self::CsiU,
            _ => Self::Xterm,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExtendedKeyDecode {
    Invalid,
    Partial,
    Matched { size: usize, key: KeyCode },
}

pub(crate) fn encode_key(
    pane_mode: u32,
    format: ExtendedKeyFormat,
    key: KeyCode,
) -> Option<Vec<u8>> {
    let format = if (pane_mode & mode::MODE_KEYS_KITTY) != 0 {
        ExtendedKeyFormat::CsiU
    } else {
        format
    };

    if is_backtab(key) {
        if (pane_mode & mode::MODE_KEYS_EXTENDED_2) != 0 {
            return input_key_extended(
                (key & !KEYC_MASK_KEY) | u64::from(b'\t') | KEYC_SHIFT,
                format,
            );
        }
        return Some(b"\x1b[Z".to_vec());
    }

    if is_trivial_key(key) {
        return key_code_to_bytes(key);
    }

    if (pane_mode & mode::MODE_KEYS_EXTENDED_2) != 0 {
        input_key_extended(key, format).or_else(|| input_key_vt10x(pane_mode, key))
    } else if (pane_mode & mode::MODE_KEYS_EXTENDED) != 0 {
        input_key_mode1(key)
            .or_else(|| input_key_extended(key, format))
            .or_else(|| input_key_vt10x(pane_mode, key))
    } else {
        input_key_vt10x(pane_mode, key)
    }
}

pub(crate) fn decode_extended_key(input: &[u8], backspace: Option<u8>) -> ExtendedKeyDecode {
    if input.first() != Some(&0x1b) {
        return ExtendedKeyDecode::Invalid;
    }
    if input.len() == 1 {
        return ExtendedKeyDecode::Partial;
    }
    if input[1] != b'[' {
        return ExtendedKeyDecode::Invalid;
    }
    if input.len() == 2 {
        return ExtendedKeyDecode::Partial;
    }

    let mut end = 2;
    while end < input.len() && end < 64 {
        let byte = input[end];
        if is_extended_key_terminator(byte) {
            break;
        }
        if !byte.is_ascii_digit() && byte != b';' {
            break;
        }
        end += 1;
    }

    if end == input.len() {
        return ExtendedKeyDecode::Partial;
    }
    if end == 64 {
        return ExtendedKeyDecode::Invalid;
    }

    let terminator = input[end];
    if !is_extended_key_terminator(terminator) {
        return ExtendedKeyDecode::Invalid;
    }

    let payload = match std::str::from_utf8(&input[2..end]) {
        Ok(payload) => payload,
        Err(_) => return ExtendedKeyDecode::Invalid,
    };

    let (mut key, modifiers) = if terminator == b'~' {
        let mut parts = payload.split(';');
        let Some(prefix) = parts.next() else {
            return ExtendedKeyDecode::Invalid;
        };
        let Some(modifiers) = parts.next() else {
            return ExtendedKeyDecode::Invalid;
        };
        let Some(number) = parts.next() else {
            return ExtendedKeyDecode::Invalid;
        };
        if prefix != "27" || parts.next().is_some() {
            return ExtendedKeyDecode::Invalid;
        }
        let Ok(modifiers) = modifiers.parse::<u16>() else {
            return ExtendedKeyDecode::Invalid;
        };
        let Ok(number) = number.parse::<u32>() else {
            return ExtendedKeyDecode::Invalid;
        };
        let Some(key) = key_from_extended_number(number, backspace) else {
            return ExtendedKeyDecode::Invalid;
        };
        (key, modifiers)
    } else if terminator == b'u' {
        let mut parts = payload.split(';');
        let Some(number) = parts.next() else {
            return ExtendedKeyDecode::Invalid;
        };
        let Some(modifiers) = parts.next() else {
            return ExtendedKeyDecode::Invalid;
        };
        if parts.next().is_some() {
            return ExtendedKeyDecode::Invalid;
        }
        let Ok(modifiers) = modifiers.parse::<u16>() else {
            return ExtendedKeyDecode::Invalid;
        };
        let Ok(number) = number.parse::<u32>() else {
            return ExtendedKeyDecode::Invalid;
        };
        let Some(key) = key_from_extended_number(number, backspace) else {
            return ExtendedKeyDecode::Invalid;
        };
        (key, modifiers)
    } else {
        let mut parts = payload.split(';');
        let Some(prefix) = parts.next() else {
            return ExtendedKeyDecode::Invalid;
        };
        let Some(modifiers) = parts.next() else {
            return ExtendedKeyDecode::Invalid;
        };
        if parts.next().is_some() {
            return ExtendedKeyDecode::Invalid;
        }
        if prefix.parse::<u16>().is_err() {
            return ExtendedKeyDecode::Invalid;
        }
        let Ok(modifiers) = modifiers.parse::<u16>() else {
            return ExtendedKeyDecode::Invalid;
        };
        let Some(key) = key_from_modified_cursor_terminator(terminator) else {
            return ExtendedKeyDecode::Invalid;
        };
        (key, modifiers)
    };

    if modifiers > 0 {
        let modifiers = modifiers - 1;
        if (modifiers & 1) != 0 {
            key |= KEYC_SHIFT;
        }
        if (modifiers & 2) != 0 {
            key |= KEYC_META | KEYC_IMPLIED_META;
        }
        if (modifiers & 4) != 0 {
            key |= KEYC_CTRL;
        }
        if (modifiers & 8) != 0 {
            key |= KEYC_META | KEYC_IMPLIED_META;
        }
    }

    if is_tab(key) && (key & KEYC_SHIFT) != 0 {
        let modifiers = key & !KEYC_MASK_KEY & !KEYC_SHIFT;
        let Some(btab) = key_string_lookup_string("BTab") else {
            return ExtendedKeyDecode::Invalid;
        };
        key = btab | modifiers;
    }

    if should_strip_shift(key) {
        key &= !KEYC_SHIFT;
    }

    ExtendedKeyDecode::Matched { size: end + 1, key }
}

fn is_extended_key_terminator(byte: u8) -> bool {
    matches!(byte, b'~' | b'u' | b'A' | b'B' | b'C' | b'D' | b'F' | b'H')
}

fn key_from_extended_number(number: u32, backspace: Option<u8>) -> Option<KeyCode> {
    if backspace.is_some_and(|value| u32::from(value) == number) {
        return Some(KEYC_BSPACE);
    }
    if number <= 0x7f {
        return Some(u64::from(number));
    }

    char::from_u32(number).map(|character| character as KeyCode)
}

fn key_from_modified_cursor_terminator(terminator: u8) -> Option<KeyCode> {
    match terminator {
        b'A' => key_string_lookup_string("Up"),
        b'B' => key_string_lookup_string("Down"),
        b'C' => key_string_lookup_string("Right"),
        b'D' => key_string_lookup_string("Left"),
        b'F' => key_string_lookup_string("End"),
        b'H' => key_string_lookup_string("Home"),
        _ => None,
    }
}

fn input_key_extended(key: KeyCode, format: ExtendedKeyFormat) -> Option<Vec<u8>> {
    if let Some(sequence) = input_key_modified_cursor(key) {
        return Some(sequence);
    }

    let modifier = xterm_modifier_parameter(key)?;
    let key = key & KEYC_MASK_KEY;

    Some(match format {
        ExtendedKeyFormat::Xterm => format!("\x1b[27;{modifier};{key}~").into_bytes(),
        ExtendedKeyFormat::CsiU => format!("\x1b[{key};{modifier}u").into_bytes(),
    })
}

fn input_key_modified_cursor(key: KeyCode) -> Option<Vec<u8>> {
    let modifier = xterm_modifier_parameter(key)?;
    let base = key & !(KEYC_SHIFT | KEYC_META | KEYC_CTRL | KEYC_IMPLIED_META);
    let final_byte = if base == (parse_named_key("Up") & !KEYC_IMPLIED_META) {
        b'A'
    } else if base == (parse_named_key("Down") & !KEYC_IMPLIED_META) {
        b'B'
    } else if base == (parse_named_key("Right") & !KEYC_IMPLIED_META) {
        b'C'
    } else if base == (parse_named_key("Left") & !KEYC_IMPLIED_META) {
        b'D'
    } else if base == (parse_named_key("End") & !KEYC_IMPLIED_META) {
        b'F'
    } else if base == (parse_named_key("Home") & !KEYC_IMPLIED_META) {
        b'H'
    } else {
        return None;
    };

    Some(format!("\x1b[1;{}{}", modifier, char::from(final_byte)).into_bytes())
}

fn xterm_modifier_parameter(key: KeyCode) -> Option<u8> {
    let modifiers = key & (KEYC_SHIFT | KEYC_META | KEYC_CTRL);
    match modifiers {
        value if value == KEYC_SHIFT => Some(2),
        value if value == KEYC_META => Some(3),
        value if value == (KEYC_SHIFT | KEYC_META) => Some(4),
        value if value == KEYC_CTRL => Some(5),
        value if value == (KEYC_SHIFT | KEYC_CTRL) => Some(6),
        value if value == (KEYC_META | KEYC_CTRL) => Some(7),
        value if value == (KEYC_SHIFT | KEYC_META | KEYC_CTRL) => Some(8),
        _ => None,
    }
}

fn input_key_vt10x(pane_mode: u32, key: KeyCode) -> Option<Vec<u8>> {
    let mut key = key;

    if let Some(sequence) = input_key_modified_cursor(key) {
        return Some(sequence);
    }

    if is_shift_only_enter(key) {
        return Some(vec![b'\n']);
    }

    let mut output = Vec::new();
    if (key & KEYC_META) != 0 {
        output.push(0x1b);
    }

    if is_unicode_key(key) {
        let character = char::from_u32((key & KEYC_MASK_KEY) as u32)?;
        let mut buffer = [0_u8; 4];
        output.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
        return Some(output);
    }

    if (pane_mode & mode::MODE_KKEYPAD) == 0 {
        key &= !KEYC_KEYPAD;
    }
    if (pane_mode & mode::MODE_KCURSOR) == 0 {
        key &= !KEYC_CURSOR;
    }

    let onlykey = key & KEYC_MASK_KEY;
    if tmux_noop_control_key(key, onlykey) {
        return Some(Vec::new());
    }

    if let Some(sequence) = standard_vt10x_sequence(key)
        .or_else(|| {
            ((key & KEYC_CURSOR) != 0)
                .then(|| standard_vt10x_sequence(key & !KEYC_CURSOR))
                .flatten()
        })
        .or_else(|| {
            ((key & KEYC_KEYPAD) != 0)
                .then(|| standard_vt10x_sequence(key & !KEYC_KEYPAD))
                .flatten()
        })
    {
        output.extend_from_slice(sequence);
        return Some(output);
    }

    if onlykey == KEYC_BSPACE {
        output.push(0x7f);
        return Some(output);
    }
    if (key & KEYC_CTRL) != 0 && onlykey == b' ' as u64 {
        output.push(0x00);
        return Some(output);
    }

    if onlykey == b'\r' as u64 || onlykey == b'\n' as u64 || onlykey == b'\t' as u64 {
        key &= !KEYC_CTRL;
    }

    if (key & KEYC_CTRL) != 0 {
        let mapped = match onlykey {
            value if value == b'1' as u64 || value == b'!' as u64 => Some(b'1'),
            value if value == b'9' as u64 || value == b'(' as u64 => Some(b'9'),
            value if value == b'0' as u64 || value == b')' as u64 => Some(b'0'),
            value if value == b'=' as u64 || value == b'+' as u64 => Some(b'='),
            value if value == b';' as u64 || value == b':' as u64 => Some(b';'),
            value if value == b'\'' as u64 || value == b'"' as u64 => Some(b'\''),
            value if value == b',' as u64 || value == b'<' as u64 => Some(b','),
            value if value == b'.' as u64 || value == b'>' as u64 => Some(b'.'),
            value if value == b'?' as u64 => Some(0x7f),
            value if value == b'/' as u64 => Some(0x1f),
            value if value == b'2' as u64 => Some(0),
            value if (b'3' as u64..=b'7' as u64).contains(&value) => Some((value as u8) - 0x18),
            value if (b'@' as u64..=b'~' as u64).contains(&value) => Some((value as u8) & 0x1f),
            _ => None,
        }?;
        key = u64::from(mapped);
    }

    output.push((key & 0x7f) as u8);
    Some(output)
}

fn tmux_noop_control_key(key: KeyCode, onlykey: KeyCode) -> bool {
    (key & (KEYC_CTRL | KEYC_META | KEYC_SHIFT)) == KEYC_CTRL
        && matches!(
            onlykey,
            value if value == b'3' as u64
                || value == b'4' as u64
                || value == b'5' as u64
                || value == b'7' as u64
                || value == b'8' as u64
        )
}

fn input_key_mode1(key: KeyCode) -> Option<Vec<u8>> {
    let onlykey = key & KEYC_MASK_KEY;
    if (key & (KEYC_CTRL | KEYC_META)) == KEYC_META {
        return input_key_vt10x(0, key);
    }
    if (key & KEYC_CTRL) != 0
        && (onlykey == b' ' as u64
            || onlykey == b'/' as u64
            || onlykey == b'@' as u64
            || onlykey == b'^' as u64
            || (b'2' as u64..=b'8' as u64).contains(&onlykey)
            || (b'@' as u64..=b'~' as u64).contains(&onlykey))
    {
        return input_key_vt10x(0, key);
    }
    None
}

fn standard_vt10x_sequence(key: KeyCode) -> Option<&'static [u8]> {
    match key & !(KEYC_META | KEYC_IMPLIED_META) {
        value if value == parse_named_key("F1") => Some(b"\x1bOP"),
        value if value == parse_named_key("F2") => Some(b"\x1bOQ"),
        value if value == parse_named_key("F3") => Some(b"\x1bOR"),
        value if value == parse_named_key("F4") => Some(b"\x1bOS"),
        value if value == parse_named_key("F5") => Some(b"\x1b[15~"),
        value if value == parse_named_key("F6") => Some(b"\x1b[17~"),
        value if value == parse_named_key("F7") => Some(b"\x1b[18~"),
        value if value == parse_named_key("F8") => Some(b"\x1b[19~"),
        value if value == parse_named_key("F9") => Some(b"\x1b[20~"),
        value if value == parse_named_key("F10") => Some(b"\x1b[21~"),
        value if value == parse_named_key("F11") => Some(b"\x1b[23~"),
        value if value == parse_named_key("F12") => Some(b"\x1b[24~"),
        value if value == parse_named_key("IC") => Some(b"\x1b[2~"),
        value if value == parse_named_key("DC") => Some(b"\x1b[3~"),
        value if value == parse_named_key("Home") => Some(b"\x1b[1~"),
        value if value == parse_named_key("End") => Some(b"\x1b[4~"),
        value if value == parse_named_key("PageUp") => Some(b"\x1b[5~"),
        value if value == parse_named_key("PageDown") => Some(b"\x1b[6~"),
        value if value == parse_named_key("BTab") => Some(b"\x1b[Z"),
        value if value == parse_named_key("Up") => Some(b"\x1bOA"),
        value if value == (parse_named_key("Up") & !KEYC_CURSOR) => Some(b"\x1b[A"),
        value if value == parse_named_key("Down") => Some(b"\x1bOB"),
        value if value == (parse_named_key("Down") & !KEYC_CURSOR) => Some(b"\x1b[B"),
        value if value == parse_named_key("Right") => Some(b"\x1bOC"),
        value if value == (parse_named_key("Right") & !KEYC_CURSOR) => Some(b"\x1b[C"),
        value if value == parse_named_key("Left") => Some(b"\x1bOD"),
        value if value == (parse_named_key("Left") & !KEYC_CURSOR) => Some(b"\x1b[D"),
        value if value == parse_named_key("KP/") => Some(b"\x1bOo"),
        value if value == (parse_named_key("KP/") & !KEYC_KEYPAD) => Some(b"/"),
        value if value == parse_named_key("KP*") => Some(b"\x1bOj"),
        value if value == (parse_named_key("KP*") & !KEYC_KEYPAD) => Some(b"*"),
        value if value == parse_named_key("KP-") => Some(b"\x1bOm"),
        value if value == (parse_named_key("KP-") & !KEYC_KEYPAD) => Some(b"-"),
        value if value == parse_named_key("KP7") => Some(b"\x1bOw"),
        value if value == (parse_named_key("KP7") & !KEYC_KEYPAD) => Some(b"7"),
        value if value == parse_named_key("KP8") => Some(b"\x1bOx"),
        value if value == (parse_named_key("KP8") & !KEYC_KEYPAD) => Some(b"8"),
        value if value == parse_named_key("KP9") => Some(b"\x1bOy"),
        value if value == (parse_named_key("KP9") & !KEYC_KEYPAD) => Some(b"9"),
        value if value == parse_named_key("KP+") => Some(b"\x1bOk"),
        value if value == (parse_named_key("KP+") & !KEYC_KEYPAD) => Some(b"+"),
        value if value == parse_named_key("KP4") => Some(b"\x1bOt"),
        value if value == (parse_named_key("KP4") & !KEYC_KEYPAD) => Some(b"4"),
        value if value == parse_named_key("KP5") => Some(b"\x1bOu"),
        value if value == (parse_named_key("KP5") & !KEYC_KEYPAD) => Some(b"5"),
        value if value == parse_named_key("KP6") => Some(b"\x1bOv"),
        value if value == (parse_named_key("KP6") & !KEYC_KEYPAD) => Some(b"6"),
        value if value == parse_named_key("KP1") => Some(b"\x1bOq"),
        value if value == (parse_named_key("KP1") & !KEYC_KEYPAD) => Some(b"1"),
        value if value == parse_named_key("KP2") => Some(b"\x1bOr"),
        value if value == (parse_named_key("KP2") & !KEYC_KEYPAD) => Some(b"2"),
        value if value == parse_named_key("KP3") => Some(b"\x1bOs"),
        value if value == (parse_named_key("KP3") & !KEYC_KEYPAD) => Some(b"3"),
        value if value == parse_named_key("KPEnter") => Some(b"\x1bOM"),
        value if value == (parse_named_key("KPEnter") & !KEYC_KEYPAD) => Some(b"\r"),
        value if value == parse_named_key("KP0") => Some(b"\x1bOp"),
        value if value == (parse_named_key("KP0") & !KEYC_KEYPAD) => Some(b"0"),
        value if value == parse_named_key("KP.") => Some(b"\x1bOn"),
        value if value == (parse_named_key("KP.") & !KEYC_KEYPAD) => Some(b"."),
        _ => None,
    }
}

fn parse_named_key(name: &str) -> KeyCode {
    key_string_lookup_string(name).expect("named key must parse")
}

fn is_unicode_key(key: KeyCode) -> bool {
    (key & KEYC_MASK_TYPE) == 0 && (key & KEYC_MASK_KEY) > 0x7f
}

fn is_trivial_key(key: KeyCode) -> bool {
    let modifiers = key & (KEYC_SHIFT | KEYC_CTRL | KEYC_META | KEYC_IMPLIED_META);
    if modifiers != 0 {
        return false;
    }

    let onlykey = key & KEYC_MASK_KEY;
    matches!(onlykey, value if value == b'\r' as u64 || value == b'\t' as u64 || value == 0x1b)
        || (0x20_u64..=0x7f).contains(&onlykey)
        || onlykey == KEYC_BSPACE
        || is_unicode_key(key)
}

fn is_backtab(key: KeyCode) -> bool {
    key_string_lookup_string("BTab")
        .map(|btab| (key & KEYC_MASK_KEY) == (btab & KEYC_MASK_KEY))
        .unwrap_or(false)
}

fn should_strip_shift(key: KeyCode) -> bool {
    let onlykey = key & KEYC_MASK_KEY;
    (((0x21_u64..0x7f).contains(&onlykey)) || is_unicode_key(key))
        && (key & KEYC_MASK_MODIFIERS) == KEYC_SHIFT
}

fn is_tab(key: KeyCode) -> bool {
    (key & KEYC_MASK_KEY) == 0x09
}

fn is_shift_only_enter(key: KeyCode) -> bool {
    (key & KEYC_MASK_KEY) == b'\r' as u64 && (key & KEYC_MASK_MODIFIERS) == KEYC_SHIFT
}

#[cfg(test)]
#[path = "input_keys/tests.rs"]
mod tests;
