use rmux_core::{key_string_lookup_string, KeyCode, KEYC_META};

use super::super::prompt_support::{decode_prompt_key, PromptInputEvent};
use crate::input_keys::{decode_extended_key, decode_mouse, ExtendedKeyDecode, MouseDecode};
use crate::key_table::{decode_attached_key, decode_xterm_modified_key, AttachedKeyDecode};

pub(in crate::handler) fn decode_prompt_input_event(
    bytes: &[u8],
) -> Option<(PromptInputEvent, usize)> {
    if bytes.is_empty() {
        return None;
    }

    if bytes.starts_with(b"\x1b[<") {
        match decode_mouse(bytes, None) {
            MouseDecode::Matched { size, .. } | MouseDecode::Discard { size } => {
                return Some((PromptInputEvent::KeyName("Mouse".to_owned()), size));
            }
            MouseDecode::Partial | MouseDecode::Overlong => return None,
            MouseDecode::Invalid => {}
        }
    }

    if is_extended_key_prefix(bytes) {
        match decode_extended_key(bytes, Some(0x7f)) {
            ExtendedKeyDecode::Matched { size, key } => {
                return Some((decode_prompt_key(key), size));
            }
            ExtendedKeyDecode::Partial => return None,
            ExtendedKeyDecode::Invalid if is_unterminated_extended_key(bytes) => return None,
            ExtendedKeyDecode::Invalid => {}
        }
    }

    if bytes[0] == 0x1b {
        return decode_prompt_escape_sequence(bytes);
    }

    let first = bytes[0];
    let event = match first {
        b'\r' | b'\n' => return Some((PromptInputEvent::Enter, 1)),
        b'\t' => return Some((PromptInputEvent::Tab, 1)),
        0x7f | 0x08 => return Some((PromptInputEvent::Backspace, 1)),
        0x01..=0x1a => {
            let ch = char::from(b'a' + (first - 1));
            return Some((PromptInputEvent::Ctrl(ch), 1));
        }
        _ if first.is_ascii() => PromptInputEvent::Char(char::from(first)),
        _ => match decode_utf8_char(bytes) {
            Some((ch, size)) => return Some((PromptInputEvent::Char(ch), size)),
            None if is_utf8_lead_byte(first) && bytes.len() < utf8_expected_len(first) => {
                // Partial multi-byte sequence - wait for more data.
                return None;
            }
            None => return Some((PromptInputEvent::KeyName("Invalid".to_owned()), 1)),
        },
    };

    Some((event, 1))
}

pub(in crate::handler) fn prompt_text_prefix_len(bytes: &[u8]) -> usize {
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] == 0x1b {
            break;
        }
        match decode_prompt_input_event(&bytes[offset..]) {
            Some((PromptInputEvent::Char(_), consumed)) if consumed > 0 => {
                offset += consumed;
            }
            _ => break,
        }
    }
    offset
}

pub(super) fn is_extended_key_prefix(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x1b[") && bytes.get(2).is_some_and(|byte| byte.is_ascii_digit())
}

fn is_unterminated_extended_key(bytes: &[u8]) -> bool {
    bytes.len() >= 3
        && bytes.starts_with(b"\x1b[")
        && bytes[2..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || *byte == b';')
}

/// Input that starts with Escape, read as tmux 3.7b's `tty_keys_next` reads
/// the client terminal. A key of the terminal key tree is that key, with or
/// without an xterm modifier parameter (`ESC [ 5 ~` is PPage, `ESC [ 6 ; 2 ~`
/// is S-NPage). Any other input is Meta with the character after Escape, and
/// what follows it is read again as keys. Only a lone Escape is the Escape
/// key, so a key that the prompt or the mode does not bind never closes it.
fn decode_prompt_escape_sequence(bytes: &[u8]) -> Option<(PromptInputEvent, usize)> {
    let [0x1b, after_escape @ ..] = bytes else {
        return None;
    };
    if after_escape.is_empty() {
        return Some((PromptInputEvent::Escape, 1));
    }
    match decode_terminal_key(bytes, Some(0x7f)) {
        AttachedKeyDecode::Matched { size, key } => Some((decode_prompt_key(key), size)),
        AttachedKeyDecode::Partial => None,
        AttachedKeyDecode::Invalid => decode_meta_prefixed_char(after_escape),
    }
}

/// Input that starts with Escape once the escape time has expired, read as
/// tmux 3.7b's `tty_keys_next` reads it when its key timer fires: it waits no
/// longer for the rest of a key. A lone Escape is Escape and a complete key of
/// the terminal key tree is that key. Anything else, an unfinished key
/// included, is Meta with the character after Escape, and the caller reads
/// the rest again as keys: `ESC [` is M-[, and `ESC [ ? 2 5` is M-[ followed
/// by the text `?25`. `None` when the input does not start with Escape.
pub(in crate::handler) fn decode_expired_escape_key(
    bytes: &[u8],
    backspace: Option<u8>,
) -> Option<(KeyCode, usize)> {
    let [0x1b, after_escape @ ..] = bytes else {
        return None;
    };
    let Some(&next) = after_escape.first() else {
        return key_string_lookup_string("Escape").map(|escape| (escape, 1));
    };
    if let AttachedKeyDecode::Matched { size, key } = decode_terminal_key(bytes, backspace) {
        return Some((key, size));
    }
    let (key, size) = decode_utf8_char(after_escape)
        .map_or((KeyCode::from(next), 1), |(ch, size)| {
            (KeyCode::from(ch), size)
        });
    Some((key | KEYC_META, 1 + size))
}

/// A key of the terminal key tree at the start of `bytes`, with or without an
/// xterm modifier parameter.
fn decode_terminal_key(bytes: &[u8], backspace: Option<u8>) -> AttachedKeyDecode {
    match decode_attached_key(bytes, backspace) {
        AttachedKeyDecode::Invalid if bytes.starts_with(b"\x1b[") => {
            decode_xterm_modified_key(bytes)
        }
        decoded => decoded,
    }
}

/// tmux 3.7b's reading of Escape before input that is no key: Meta with the
/// next character (`ESC [ 2 J` is M-[ and then the text `2J`).
fn decode_meta_prefixed_char(after_escape: &[u8]) -> Option<(PromptInputEvent, usize)> {
    let first = *after_escape.first()?;
    let (key, size) = match decode_utf8_char(after_escape) {
        Some((ch, size)) => (KeyCode::from(ch), size),
        None if is_utf8_lead_byte(first) && after_escape.len() < utf8_expected_len(first) => {
            return None;
        }
        None => return Some((PromptInputEvent::KeyName("Invalid".to_owned()), 1)),
    };
    Some((decode_prompt_key(key | KEYC_META), 1 + size))
}

pub(super) fn is_utf8_lead_byte(byte: u8) -> bool {
    matches!(byte, 0xc0..=0xf7)
}

pub(super) fn utf8_expected_len(lead: u8) -> usize {
    match lead {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

pub(super) fn decode_utf8_char(bytes: &[u8]) -> Option<(char, usize)> {
    let first = *bytes.first()?;
    let expected = match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return None,
    };
    if bytes.len() < expected {
        return None;
    }
    let text = std::str::from_utf8(&bytes[..expected]).ok()?;
    let ch = text.chars().next()?;
    Some((ch, expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_utf8_returns_none() {
        // First two bytes of a 3-byte UTF-8 char (e.g. '日' = 0xe6 0x97 0xa5).
        let partial = [0xe6, 0x97];
        assert!(decode_prompt_input_event(&partial).is_none());
    }

    #[test]
    fn complete_utf8_returns_char() {
        let bytes = "日".as_bytes();
        let (event, consumed) = decode_prompt_input_event(bytes).unwrap();
        assert_eq!(consumed, 3);
        assert!(matches!(event, PromptInputEvent::Char('日')));
    }

    #[test]
    fn invalid_continuation_byte_returns_invalid() {
        // 0xff is never a valid UTF-8 lead byte.
        let bytes = [0xff, 0x41];
        let (event, consumed) = decode_prompt_input_event(&bytes).unwrap();
        assert_eq!(consumed, 1);
        assert!(matches!(event, PromptInputEvent::KeyName(_)));
    }

    #[test]
    fn prompt_text_prefix_is_utf8_complete_and_stops_before_control_events() {
        let text = "aé日本";
        let mut bytes = text.as_bytes().to_vec();
        bytes.extend_from_slice(b"\x1bTAIL");
        assert_eq!(prompt_text_prefix_len(&bytes), text.len());

        let partial = b"abc\xe6\x97";
        assert_eq!(prompt_text_prefix_len(partial), 3);

        assert_eq!(
            prompt_text_prefix_len(b"\x1b[27;2;65~"),
            0,
            "an encoded printable key must be projected through its decoded character"
        );
    }

    #[test]
    fn mouse_sequence_consumed_whole() {
        let bytes = b"\x1b[<0;12;34M";
        let (event, consumed) = decode_prompt_input_event(bytes).unwrap();
        assert_eq!(consumed, bytes.len());
        matches!(event, PromptInputEvent::KeyName(name) if name == "Mouse");
    }

    #[test]
    fn partial_mouse_sequence_returns_none() {
        let bytes = b"\x1b[<0;12;34";
        assert!(decode_prompt_input_event(bytes).is_none());
    }

    #[test]
    fn invalid_mouse_prefix_does_not_poison_prompt_input() {
        let bytes = b"\x1b[<a";
        let (event, consumed) = decode_prompt_input_event(bytes).expect("invalid CSI resolves");
        assert_eq!(consumed, 2, "the rest is read again as keys");
        assert_eq!(event, PromptInputEvent::KeyName("M-[".to_owned()));
    }

    #[test]
    fn long_unterminated_extended_key_returns_none_for_bound_guard() {
        let mut bytes = b"\x1b[27;2;65".to_vec();
        bytes.resize(128, b'9');
        assert!(decode_prompt_input_event(&bytes).is_none());
    }

    #[test]
    fn unknown_escape_sequence_is_meta_of_its_next_character_never_escape() {
        for (bytes, name) in [
            (&b"\x1b[2J"[..], "M-["),
            (b"\x1bOz", "M-O"),
            (b"\x1b\x1bx", "M-Escape"),
            (b"\x1b\xc3\xa9", "M-\u{e9}"),
        ] {
            let (event, consumed) = decode_prompt_input_event(bytes).expect("input resolves");
            let meta_size = if bytes[1].is_ascii() { 2 } else { bytes.len() };
            assert_eq!(consumed, meta_size, "{bytes:?}");
            assert_eq!(
                event,
                PromptInputEvent::KeyName(name.to_owned()),
                "{bytes:?}"
            );
        }
        assert!(decode_prompt_input_event(b"\x1b\xc3").is_none());
    }

    #[test]
    fn terminal_keys_after_escape_decode_to_their_key_never_escape() {
        for (bytes, event) in [
            (
                &b"\x1b[5~"[..],
                PromptInputEvent::KeyName("PPage".to_owned()),
            ),
            (b"\x1b[6~", PromptInputEvent::KeyName("NPage".to_owned())),
            (b"\x1b[2~", PromptInputEvent::KeyName("IC".to_owned())),
            (b"\x1b[15~", PromptInputEvent::KeyName("F5".to_owned())),
            (b"\x1b[24~", PromptInputEvent::KeyName("F12".to_owned())),
            (b"\x1bOP", PromptInputEvent::KeyName("F1".to_owned())),
            (
                b"\x1b[6;2~",
                PromptInputEvent::KeyName("S-NPage".to_owned()),
            ),
            (b"\x1b[3;5~", PromptInputEvent::KeyName("C-DC".to_owned())),
            (b"\x1b[15;2~", PromptInputEvent::KeyName("S-F5".to_owned())),
            (b"\x1b[15;3~", PromptInputEvent::KeyName("M-F5".to_owned())),
            (b"\x1b[1;2P", PromptInputEvent::KeyName("S-F1".to_owned())),
            (b"\x1b[1;5A", PromptInputEvent::KeyName("C-Up".to_owned())),
            (b"\x1bx", PromptInputEvent::KeyName("M-x".to_owned())),
            (b"\x1b[3~", PromptInputEvent::Delete),
            (b"\x1b[H", PromptInputEvent::Home),
            (b"\x1b[B", PromptInputEvent::Down),
        ] {
            assert_eq!(
                decode_prompt_input_event(bytes),
                Some((event, bytes.len())),
                "{bytes:?}"
            );
        }
    }

    #[test]
    fn only_a_lone_escape_is_escape_and_unfinished_keys_wait() {
        assert_eq!(
            decode_prompt_input_event(b"\x1b"),
            Some((PromptInputEvent::Escape, 1))
        );
        for partial in [&b"\x1b["[..], b"\x1bO", b"\x1b[6", b"\x1b[6;2", b"\x1b[1;2"] {
            assert!(decode_prompt_input_event(partial).is_none(), "{partial:?}");
        }
    }

    #[test]
    fn expired_escape_input_is_its_key_or_meta_of_its_next_character() {
        let expired = |bytes: &[u8]| {
            decode_expired_escape_key(bytes, Some(0x7f))
                .map(|(key, size)| (decode_prompt_key(key), size))
        };
        assert_eq!(expired(b"\x1b"), Some((PromptInputEvent::Escape, 1)));
        for (bytes, name, size) in [
            (&b"\x1b["[..], "M-[", 2),
            (b"\x1bO", "M-O", 2),
            (b"\x1b[?25", "M-[", 2),
            (b"\x1b[<", "M-[", 2),
            (b"\x1b\x1b", "M-Escape", 2),
        ] {
            assert_eq!(
                expired(bytes),
                Some((PromptInputEvent::KeyName(name.to_owned()), size)),
                "{bytes:?}"
            );
        }
        assert_eq!(expired(b"\x1bOH"), Some((PromptInputEvent::Home, 3)));
        assert_eq!(expired(b"\x1bOF"), Some((PromptInputEvent::End, 3)));
        assert_eq!(expired(b"abc"), None);
    }

    #[test]
    fn ctrl_a_through_z_decoded() {
        // Some control codes are intercepted before the Ctrl range.
        let special = [0x08u8, 0x09, 0x0a, 0x0d];
        for code in 1u8..=26 {
            let bytes = [code];
            let (event, consumed) = decode_prompt_input_event(&bytes).unwrap();
            assert_eq!(consumed, 1);
            if special.contains(&code) {
                continue;
            }
            let expected_ch = char::from(b'a' + (code - 1));
            assert!(
                matches!(event, PromptInputEvent::Ctrl(ch) if ch == expected_ch),
                "code 0x{code:02x} expected Ctrl({expected_ch}) got {event:?}",
            );
        }
    }
}
