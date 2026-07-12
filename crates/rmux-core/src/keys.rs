//! tmux-compatible key code parsing and key table storage.
#![allow(clippy::unusual_byte_groupings)]

use crate::command_parser::{CommandParseError, CommandParser, ParsedCommands};

#[path = "keys/defaults.rs"]
mod defaults;
#[path = "keys/store.rs"]
mod store;
#[path = "keys/string_table.rs"]
mod string_table;

pub use store::{
    KeyBinding, KeyBindingDisplay, KeyBindingSortOrder, KeyBindingStore, KeyBindingTable,
    KeyBindingTableRef,
};
use string_table::{
    decode_mouse_key, key_string_entry_for_key, key_string_search_table, mouse_key_name,
};

/// tmux-style 64-bit key code.
pub type KeyCode = u64;

/// Meta modifier bit.
pub const KEYC_META: KeyCode = 0x0010_0000_0000_00;
/// Ctrl modifier bit.
pub const KEYC_CTRL: KeyCode = 0x0020_0000_0000_00;
/// Shift modifier bit.
pub const KEYC_SHIFT: KeyCode = 0x0040_0000_0000_00;
/// Literal flag bit.
pub const KEYC_LITERAL: KeyCode = 0x0100_0000_0000_00;
/// Keypad flag bit.
pub const KEYC_KEYPAD: KeyCode = 0x0200_0000_0000_00;
/// Cursor flag bit.
pub const KEYC_CURSOR: KeyCode = 0x0400_0000_0000_00;
/// Implied-meta flag bit.
pub const KEYC_IMPLIED_META: KeyCode = 0x0800_0000_0000_00;
/// Build-modifiers flag bit.
pub const KEYC_BUILD_MODIFIERS: KeyCode = 0x1000_0000_0000_00;
/// Vi flag bit.
pub const KEYC_VI: KeyCode = 0x2000_0000_0000_00;
/// Sent flag bit.
pub const KEYC_SENT: KeyCode = 0x4000_0000_0000_00;

/// Key type mask.
pub const KEYC_MASK_TYPE: KeyCode = 0x0000_ff00_0000_00;
/// Modifier mask.
pub const KEYC_MASK_MODIFIERS: KeyCode = 0x00ff_0000_0000_00;
/// Flag mask.
pub const KEYC_MASK_FLAGS: KeyCode = 0xff00_0000_0000_00;
/// Key payload mask.
pub const KEYC_MASK_KEY: KeyCode = 0x0000_ffff_ffff_ff;

const KEYC_NUSER: u32 = 1000;

/// No key.
pub const KEYC_NONE: KeyCode = shift_type(KeyCodeType::Function);
/// Unknown key.
pub const KEYC_UNKNOWN: KeyCode = KEYC_NONE + 1;
/// Any key catch-all.
pub const KEYC_ANY: KeyCode = KEYC_NONE + 4;
/// Backspace key.
pub const KEYC_BSPACE: KeyCode = KEYC_NONE + 7;
/// User key range base.
pub const KEYC_USER: KeyCode = shift_type(KeyCodeType::User);

const KEYC_FOCUS_IN: KeyCode = KEYC_NONE + 2;
const KEYC_FOCUS_OUT: KeyCode = KEYC_NONE + 3;
const KEYC_PASTE_START: KeyCode = KEYC_NONE + 5;
const KEYC_PASTE_END: KeyCode = KEYC_NONE + 6;
const KEYC_F1: KeyCode = KEYC_NONE + 8;
const KEYC_F2: KeyCode = KEYC_NONE + 9;
const KEYC_F3: KeyCode = KEYC_NONE + 10;
const KEYC_F4: KeyCode = KEYC_NONE + 11;
const KEYC_F5: KeyCode = KEYC_NONE + 12;
const KEYC_F6: KeyCode = KEYC_NONE + 13;
const KEYC_F7: KeyCode = KEYC_NONE + 14;
const KEYC_F8: KeyCode = KEYC_NONE + 15;
const KEYC_F9: KeyCode = KEYC_NONE + 16;
const KEYC_F10: KeyCode = KEYC_NONE + 17;
const KEYC_F11: KeyCode = KEYC_NONE + 18;
const KEYC_F12: KeyCode = KEYC_NONE + 19;
const KEYC_IC: KeyCode = KEYC_NONE + 20;
const KEYC_DC: KeyCode = KEYC_NONE + 21;
const KEYC_HOME: KeyCode = KEYC_NONE + 22;
const KEYC_END: KeyCode = KEYC_NONE + 23;
const KEYC_NPAGE: KeyCode = KEYC_NONE + 24;
const KEYC_PPAGE: KeyCode = KEYC_NONE + 25;
const KEYC_BTAB: KeyCode = KEYC_NONE + 26;
const KEYC_UP: KeyCode = KEYC_NONE + 27;
const KEYC_DOWN: KeyCode = KEYC_NONE + 28;
const KEYC_LEFT: KeyCode = KEYC_NONE + 29;
const KEYC_RIGHT: KeyCode = KEYC_NONE + 30;
const KEYC_KP_SLASH: KeyCode = KEYC_NONE + 31;
const KEYC_KP_STAR: KeyCode = KEYC_NONE + 32;
const KEYC_KP_MINUS: KeyCode = KEYC_NONE + 33;
const KEYC_KP_SEVEN: KeyCode = KEYC_NONE + 34;
const KEYC_KP_EIGHT: KeyCode = KEYC_NONE + 35;
const KEYC_KP_NINE: KeyCode = KEYC_NONE + 36;
const KEYC_KP_PLUS: KeyCode = KEYC_NONE + 37;
const KEYC_KP_FOUR: KeyCode = KEYC_NONE + 38;
const KEYC_KP_FIVE: KeyCode = KEYC_NONE + 39;
const KEYC_KP_SIX: KeyCode = KEYC_NONE + 40;
const KEYC_KP_ONE: KeyCode = KEYC_NONE + 41;
const KEYC_KP_TWO: KeyCode = KEYC_NONE + 42;
const KEYC_KP_THREE: KeyCode = KEYC_NONE + 43;
const KEYC_KP_ENTER: KeyCode = KEYC_NONE + 44;
const KEYC_KP_ZERO: KeyCode = KEYC_NONE + 45;
const KEYC_KP_PERIOD: KeyCode = KEYC_NONE + 46;
const KEYC_REPORT_DARK_THEME: KeyCode = KEYC_NONE + 47;
const KEYC_REPORT_LIGHT_THEME: KeyCode = KEYC_NONE + 48;
const KEYC_MOUSE: KeyCode = KEYC_NONE + 49;
/// Internal drag-in-progress sentinel key.
pub const KEYC_DRAGGING: KeyCode = KEYC_NONE + 50;

/// Default `list-keys` template.
pub const LIST_KEYS_TEMPLATE: &str = "#{?notes_only,#{key_prefix}#{p|#{key_string_width}:key_string} #{?key_note,#{key_note},#{key_command}},bind-key#{?key_has_repeat, #{?key_repeat,-r,  },} -T #{p|#{key_table_width}:key_table} #{p|#{key_string_width}:key_string} #{key_command}}";

/// Returns the key bits used for binding lookup.
#[must_use]
pub const fn key_code_lookup_bits(key: KeyCode) -> KeyCode {
    key & (KEYC_MASK_KEY | KEYC_MASK_MODIFIERS)
}

/// Returns whether the key is a mouse-move key.
#[must_use]
pub fn key_code_is_mouse_move(key: KeyCode) -> bool {
    matches!(
        decode_mouse_key(key),
        Some((MouseEventType::MouseMove, _, _))
    )
}

/// Converts a canonical key name into a tmux key code.
#[must_use]
pub fn key_string_lookup_string(string: &str) -> Option<KeyCode> {
    if string.eq_ignore_ascii_case("None") {
        return Some(KEYC_NONE);
    }
    if string.eq_ignore_ascii_case("Any") {
        return Some(KEYC_ANY);
    }

    if let Some(hex) = string.strip_prefix("0x") {
        let value = u32::from_str_radix(hex, 16).ok()?;
        if value < 32 {
            return Some(KeyCode::from(value));
        }
        return char::from_u32(value).map(|character| character as KeyCode);
    }

    let mut modifiers = 0;
    let mut rest = string;

    if rest.starts_with('^') && rest.len() > 1 {
        if rest.chars().count() == 2 {
            let character = rest.chars().nth(1)?;
            if let Some(alias) = control_key_alias(character) {
                return Some(alias);
            }
            return Some(character.to_ascii_lowercase() as KeyCode | KEYC_CTRL);
        }
        modifiers |= KEYC_CTRL;
        rest = &rest[1..];
    }

    modifiers |= parse_modifiers(&mut rest)?;
    if rest.is_empty() {
        return None;
    }

    if rest.is_ascii() {
        let bytes = rest.as_bytes();
        if bytes.len() == 1 {
            let key = KeyCode::from(bytes[0]);
            if key < 32 {
                return None;
            }
            if modifiers & KEYC_CTRL != 0 {
                if let Some(alias) = control_key_alias(char::from(bytes[0])) {
                    return Some(alias | (modifiers & !KEYC_CTRL));
                }
            }
            return Some(key | modifiers);
        }
    } else {
        let mut chars = rest.chars();
        let character = chars.next()?;
        if chars.next().is_none() {
            return Some(character as KeyCode | modifiers);
        }
    }

    let mut key = key_string_search_table(rest)?;
    if modifiers & KEYC_META == 0 {
        key &= !KEYC_IMPLIED_META;
    }
    Some(key | modifiers)
}

fn control_key_alias(character: char) -> Option<KeyCode> {
    match character {
        '[' => Some(0x1b),
        _ => None,
    }
}

/// Converts a key code into its canonical tmux string.
#[must_use]
pub fn key_string_lookup_key(key: KeyCode, with_flags: bool) -> String {
    let saved = key;
    let mut output = String::new();

    if key & KEYC_LITERAL != 0 {
        output.push(char::from_u32((key & 0xff) as u32).unwrap_or('\0'));
        return maybe_append_flags(output, saved, with_flags);
    }

    if key & KEYC_CTRL != 0 {
        output.push_str("C-");
    }
    if key & KEYC_META != 0 {
        output.push_str("M-");
    }
    if key & KEYC_SHIFT != 0 {
        output.push_str("S-");
    }

    let key = key & KEYC_MASK_KEY;
    let suffix = match key {
        KEYC_NONE => Some("None".to_owned()),
        KEYC_UNKNOWN => Some("Unknown".to_owned()),
        KEYC_ANY => Some("Any".to_owned()),
        KEYC_FOCUS_IN => Some("FocusIn".to_owned()),
        KEYC_FOCUS_OUT => Some("FocusOut".to_owned()),
        KEYC_PASTE_START => Some("PasteStart".to_owned()),
        KEYC_PASTE_END => Some("PasteEnd".to_owned()),
        KEYC_REPORT_DARK_THEME => Some("ReportDarkTheme".to_owned()),
        KEYC_REPORT_LIGHT_THEME => Some("ReportLightTheme".to_owned()),
        KEYC_MOUSE => Some("Mouse".to_owned()),
        KEYC_DRAGGING => Some("Dragging".to_owned()),
        value if value == make_mouse_key(MouseEventType::MouseMove, 0, MouseLocation::Pane) => {
            Some("MouseMovePane".to_owned())
        }
        value if value == make_mouse_key(MouseEventType::MouseMove, 0, MouseLocation::Status) => {
            Some("MouseMoveStatus".to_owned())
        }
        value
            if value == make_mouse_key(MouseEventType::MouseMove, 0, MouseLocation::StatusLeft) =>
        {
            Some("MouseMoveStatusLeft".to_owned())
        }
        value
            if value
                == make_mouse_key(MouseEventType::MouseMove, 0, MouseLocation::StatusRight) =>
        {
            Some("MouseMoveStatusRight".to_owned())
        }
        value
            if value
                == make_mouse_key(MouseEventType::MouseMove, 0, MouseLocation::StatusDefault) =>
        {
            Some("MouseMoveStatusDefault".to_owned())
        }
        value if value == make_mouse_key(MouseEventType::MouseMove, 0, MouseLocation::Border) => {
            Some("MouseMoveBorder".to_owned())
        }
        value if is_user_key(value) => Some(format!("User{}", value - KEYC_USER)),
        value => key_string_entry_for_key(value)
            .map(|entry| entry.string.to_owned())
            .or_else(|| mouse_key_name(value))
            .or_else(|| {
                if is_unicode_key(value) {
                    char::from_u32(value as u32).map(|character| character.to_string())
                } else if value > 255 {
                    Some(format!("Invalid#{saved:#x}"))
                } else if (33..=126).contains(&value) {
                    Some((value as u8 as char).to_string())
                } else if value == 127 {
                    Some("C-?".to_owned())
                } else if value >= 128 {
                    Some(format!("\\{:o}", value))
                } else {
                    key_string_entry_for_key(value).map(|entry| entry.string.to_owned())
                }
            }),
    };

    if let Some(suffix) = suffix {
        output.push_str(&suffix);
    }
    maybe_append_flags(output, saved, with_flags)
}

/// Converts a key code into bytes suitable for the legacy direct PTY path.
#[must_use]
pub fn key_code_to_bytes(key: KeyCode) -> Option<Vec<u8>> {
    let key = key_code_lookup_bits(key);
    if key == KEYC_NONE || key == KEYC_UNKNOWN || KEYC_IS_MOUSE(key) {
        return None;
    }

    let base = key & KEYC_MASK_KEY;
    if base == b'\r' as u64 && (key & KEYC_MASK_MODIFIERS) == KEYC_SHIFT {
        return Some(vec![b'\n']);
    }
    if key & KEYC_CTRL != 0 {
        if base == b'?' as u64 {
            return Some(vec![0x7f]);
        }
        if base == b' ' as u64 {
            return Some(vec![0x00]);
        }
        if (b'a' as u64..=b'z' as u64).contains(&base) {
            return Some(vec![((base as u8) - b'a') + 1]);
        }
        if (b'A' as u64..=b'Z' as u64).contains(&base) {
            return Some(vec![((base as u8) - b'A') + 1]);
        }
    }

    match base {
        value if value == b'\r' as u64 || value == b'\t' as u64 || value == 0x1b => {
            Some(vec![value as u8])
        }
        value if value == KEYC_BSPACE => Some(vec![0x7f]),
        value if value <= 0x7f => Some(vec![value as u8]),
        value if is_unicode_key(value) => char::from_u32(value as u32).map(|character| {
            let mut buffer = [0_u8; 4];
            character.encode_utf8(&mut buffer).as_bytes().to_vec()
        }),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
enum KeyCodeType {
    Unicode = 0,
    User = 1,
    Function = 2,
    MouseMove = 3,
    MouseDown = 4,
    MouseUp = 5,
    MouseDrag = 6,
    MouseDragEnd = 7,
    WheelDown = 8,
    WheelUp = 9,
    SecondClick = 10,
    DoubleClick = 11,
    TripleClick = 12,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
enum MouseLocation {
    Pane = 0,
    Status = 1,
    StatusLeft = 2,
    StatusRight = 3,
    StatusDefault = 4,
    Border = 5,
    ScrollbarUp = 6,
    ScrollbarSlider = 7,
    ScrollbarDown = 8,
    Control0 = 9,
    Control1 = 10,
    Control2 = 11,
    Control3 = 12,
    Control4 = 13,
    Control5 = 14,
    Control6 = 15,
    Control7 = 16,
    Control8 = 17,
    Control9 = 18,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseEventType {
    MouseMove,
    MouseDown,
    MouseUp,
    MouseDrag,
    MouseDragEnd,
    WheelDown,
    WheelUp,
    SecondClick,
    DoubleClick,
    TripleClick,
}

const fn shift_type(kind: KeyCodeType) -> KeyCode {
    (kind as KeyCode) << 32
}

const fn make_mouse_key(kind: MouseEventType, button: u64, location: MouseLocation) -> KeyCode {
    shift_type(match kind {
        MouseEventType::MouseMove => KeyCodeType::MouseMove,
        MouseEventType::MouseDown => KeyCodeType::MouseDown,
        MouseEventType::MouseUp => KeyCodeType::MouseUp,
        MouseEventType::MouseDrag => KeyCodeType::MouseDrag,
        MouseEventType::MouseDragEnd => KeyCodeType::MouseDragEnd,
        MouseEventType::WheelDown => KeyCodeType::WheelDown,
        MouseEventType::WheelUp => KeyCodeType::WheelUp,
        MouseEventType::SecondClick => KeyCodeType::SecondClick,
        MouseEventType::DoubleClick => KeyCodeType::DoubleClick,
        MouseEventType::TripleClick => KeyCodeType::TripleClick,
    }) | (button << 8)
        | location as u64
}

const fn strip_flags(key: KeyCode) -> KeyCode {
    key & !KEYC_MASK_FLAGS
}

const fn is_unicode_key(key: KeyCode) -> bool {
    (key & KEYC_MASK_TYPE) == shift_type(KeyCodeType::Unicode) && (key & KEYC_MASK_KEY) > 0x7f
}

const fn is_user_key(key: KeyCode) -> bool {
    (key & KEYC_MASK_TYPE) == shift_type(KeyCodeType::User)
}

#[allow(non_snake_case)]
const fn KEYC_IS_MOUSE(key: KeyCode) -> bool {
    (key & KEYC_MASK_KEY) == KEYC_MOUSE
        || ((key & KEYC_MASK_TYPE) >= shift_type(KeyCodeType::MouseMove)
            && (key & KEYC_MASK_TYPE) <= shift_type(KeyCodeType::TripleClick))
}

fn parse_modifiers(rest: &mut &str) -> Option<KeyCode> {
    let mut modifiers = 0;
    loop {
        let bytes = rest.as_bytes();
        if bytes.len() < 2 || bytes[1] != b'-' {
            break;
        }
        match bytes[0].to_ascii_lowercase() {
            b'c' => modifiers |= KEYC_CTRL,
            b'm' => modifiers |= KEYC_META,
            b's' => modifiers |= KEYC_SHIFT,
            _ => return None,
        }
        *rest = &rest[2..];
    }
    Some(modifiers)
}

fn maybe_append_flags(mut output: String, saved: KeyCode, with_flags: bool) -> String {
    if with_flags && (saved & KEYC_MASK_FLAGS) != 0 {
        output.push('[');
        if saved & KEYC_LITERAL != 0 {
            output.push('L');
        }
        if saved & KEYC_KEYPAD != 0 {
            output.push('K');
        }
        if saved & KEYC_CURSOR != 0 {
            output.push('C');
        }
        if saved & KEYC_IMPLIED_META != 0 {
            output.push('I');
        }
        if saved & KEYC_BUILD_MODIFIERS != 0 {
            output.push('B');
        }
        if saved & KEYC_SENT != 0 {
            output.push('S');
        }
        output.push(']');
    }
    output
}

/// Parses a `bind-key` command payload from raw argv-style tokens.
pub fn parse_binding_command_tokens(
    tokens: &[String],
) -> Result<ParsedCommands, CommandParseError> {
    parse_binding_command_tokens_with_parser(&CommandParser::new(), tokens)
}

/// Parses a `bind-key` payload with the caller's environment and alias table.
pub fn parse_binding_command_tokens_with_parser(
    parser: &CommandParser,
    tokens: &[String],
) -> Result<ParsedCommands, CommandParseError> {
    if tokens.len() == 1 {
        parser.parse(&tokens[0])
    } else {
        parser.parse_arguments(tokens)
    }
}

#[cfg(test)]
#[path = "keys/tests.rs"]
mod tests;
