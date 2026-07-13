use super::super::prompt_support::{decode_prompt_key, PromptInputEvent};
use crate::input_keys::{decode_extended_key, decode_mouse, ExtendedKeyDecode, MouseDecode};

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

    if let Some((event, consumed)) = decode_prompt_escape_sequence(bytes) {
        return Some((event, consumed));
    }

    let first = bytes[0];
    let event = match first {
        b'\r' | b'\n' => return Some((PromptInputEvent::Enter, 1)),
        b'\t' => return Some((PromptInputEvent::Tab, 1)),
        0x7f | 0x08 => return Some((PromptInputEvent::Backspace, 1)),
        0x1b => return Some((PromptInputEvent::Escape, 1)),
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

fn decode_prompt_escape_sequence(bytes: &[u8]) -> Option<(PromptInputEvent, usize)> {
    if bytes.first() != Some(&0x1b) {
        return None;
    }
    match bytes.get(1).copied() {
        Some(b'[') => match bytes.get(2).copied() {
            Some(b'A') => Some((PromptInputEvent::Up, 3)),
            Some(b'B') => Some((PromptInputEvent::Down, 3)),
            Some(b'C') => Some((PromptInputEvent::Right, 3)),
            Some(b'D') => Some((PromptInputEvent::Left, 3)),
            Some(b'H') => Some((PromptInputEvent::Home, 3)),
            Some(b'F') => Some((PromptInputEvent::End, 3)),
            Some(b'3') if bytes.get(3) == Some(&b'~') => Some((PromptInputEvent::Delete, 4)),
            None => None,
            Some(_) => {
                // Unknown CSI sequence: consume through the final byte
                // (0x40..=0x7e) to avoid leaking intermediate chars into the prompt.
                let consumed = csi_sequence_length(bytes);
                Some((PromptInputEvent::Escape, consumed))
            }
        },
        Some(_) => Some((PromptInputEvent::Escape, 1)),
        None => Some((PromptInputEvent::Escape, 1)),
    }
}

/// Returns the length of a CSI sequence starting at `bytes[0]` (`\x1b`).
/// Scans for the final byte in 0x40..=0x7e range. Falls back to 3 bytes
/// (ESC [ + one unknown byte) if no final byte is found in the buffer.
fn csi_sequence_length(bytes: &[u8]) -> usize {
    // bytes[0] = ESC, bytes[1] = '[', scan from bytes[2..]
    for (i, &byte) in bytes.iter().enumerate().skip(2) {
        if (0x40..=0x7e).contains(&byte) {
            return i + 1;
        }
        // Parameter/intermediate bytes are 0x20..=0x3f; keep scanning.
        if !(0x20..=0x3f).contains(&byte) {
            return i;
        }
    }
    // Partial sequence; consume what we have (at least ESC + '[' + 1).
    bytes.len().min(3)
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
        assert_eq!(consumed, bytes.len());
        assert!(matches!(event, PromptInputEvent::Escape));
    }

    #[test]
    fn long_unterminated_extended_key_returns_none_for_bound_guard() {
        let mut bytes = b"\x1b[27;2;65".to_vec();
        bytes.resize(128, b'9');
        assert!(decode_prompt_input_event(&bytes).is_none());
    }

    #[test]
    fn unknown_csi_consumed_through_final_byte() {
        let bytes = b"\x1b[2J";
        let (event, consumed) = decode_prompt_input_event(bytes).unwrap();
        assert_eq!(consumed, 4);
        assert!(matches!(event, PromptInputEvent::Escape));
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
