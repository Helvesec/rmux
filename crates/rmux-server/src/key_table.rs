use rmux_core::{
    key_code_is_mouse_move, key_code_lookup_bits, key_string_lookup_string, KeyBinding, KeyCode,
    KEYC_ANY, KEYC_MASK_KEY, KEYC_MASK_MODIFIERS,
};
use rmux_proto::{OptionName, PaneTarget};

use crate::copy_mode::ModeKeys;
use crate::input_keys::{decode_extended_key, ExtendedKeyDecode};
use crate::pane_terminals::HandlerState;

pub(crate) const ROOT_TABLE: &str = "root";
pub(crate) const PREFIX_TABLE: &str = "prefix";
pub(crate) const COPY_MODE_TABLE: &str = "copy-mode";
pub(crate) const COPY_MODE_VI_TABLE: &str = "copy-mode-vi";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step03PrefixBinding {
    SelectPaneNext,
    SelectPanePrevious,
    NextWindow,
    PreviousWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachedKeyDecode {
    Invalid,
    Partial,
    Matched { size: usize, key: KeyCode },
}

pub(crate) fn decode_attached_key(input: &[u8], backspace: Option<u8>) -> AttachedKeyDecode {
    let Some(&first) = input.first() else {
        return AttachedKeyDecode::Partial;
    };

    if first == b'\x1b' {
        return decode_escape_key(input, backspace);
    }
    if first.is_ascii() && !first.is_ascii_control() {
        return AttachedKeyDecode::Matched {
            size: 1,
            key: KeyCode::from(first),
        };
    }
    if let Some(key) = control_byte_key(first) {
        return AttachedKeyDecode::Matched { size: 1, key };
    }

    AttachedKeyDecode::Invalid
}

pub(crate) fn default_key_table_name(state: &HandlerState, target: &PaneTarget) -> String {
    if target_is_in_copy_mode(state, target) {
        return match ModeKeys::parse(state.options.resolve_for_pane(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            OptionName::ModeKeys,
        )) {
            ModeKeys::Emacs => COPY_MODE_TABLE.to_owned(),
            ModeKeys::Vi => COPY_MODE_VI_TABLE.to_owned(),
        };
    }

    state
        .options
        .resolve(Some(target.session_name()), OptionName::KeyTable)
        .filter(|value| !value.is_empty())
        .unwrap_or(ROOT_TABLE)
        .to_owned()
}

pub(crate) fn session_option_key(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    option: OptionName,
) -> Option<KeyCode> {
    state
        .options
        .resolve(Some(session_name), option)
        .and_then(key_string_lookup_string)
}

pub(crate) fn session_option_u64(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    option: OptionName,
) -> u64 {
    state
        .options
        .resolve(Some(session_name), option)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}

pub(crate) fn matches_prefix_key(
    key: KeyCode,
    prefix: Option<KeyCode>,
    prefix2: Option<KeyCode>,
) -> bool {
    let masked = key & (KEYC_MASK_KEY | KEYC_MASK_MODIFIERS);
    prefix
        .into_iter()
        .chain(prefix2)
        .any(|candidate| key_code_lookup_bits(candidate) == masked)
}

pub(crate) fn lookup_key_table_binding(
    state: &HandlerState,
    table_name: &str,
    key: KeyCode,
) -> Option<KeyBinding> {
    lookup_raw_key_table_binding(state, table_name, key)
}

pub(crate) fn lookup_attached_key_table_binding(
    state: &HandlerState,
    table_name: &str,
    key: KeyCode,
) -> Option<KeyBinding> {
    lookup_raw_key_table_binding(state, table_name, key)
}

fn lookup_raw_key_table_binding(
    state: &HandlerState,
    table_name: &str,
    key: KeyCode,
) -> Option<KeyBinding> {
    state
        .key_bindings
        .get_binding(table_name, key)
        .cloned()
        .or_else(|| {
            equivalent_key_table_alias(key)
                .and_then(|alias| state.key_bindings.get_binding(table_name, alias).cloned())
        })
        .or_else(|| {
            state
                .key_bindings
                .get_binding(table_name, KEYC_ANY)
                .cloned()
        })
}

fn equivalent_key_table_alias(key: KeyCode) -> Option<KeyCode> {
    let c_space = key_string_lookup_string("C-Space")?;
    let c_at = key_string_lookup_string("C-@")?;
    let lookup = key_code_lookup_bits(key);

    if lookup == key_code_lookup_bits(c_space) {
        Some(c_at)
    } else if lookup == key_code_lookup_bits(c_at) {
        Some(c_space)
    } else {
        None
    }
}

pub(crate) fn step03_prefix_binding(key: KeyCode) -> Option<Step03PrefixBinding> {
    ["Right", "Left", "Up", "Down", "n", "p"]
        .into_iter()
        .filter_map(key_string_lookup_string)
        .zip([
            Step03PrefixBinding::SelectPaneNext,
            Step03PrefixBinding::SelectPanePrevious,
            Step03PrefixBinding::SelectPanePrevious,
            Step03PrefixBinding::SelectPaneNext,
            Step03PrefixBinding::NextWindow,
            Step03PrefixBinding::PreviousWindow,
        ])
        .find_map(|(candidate, action)| {
            (key_code_lookup_bits(candidate) == key_code_lookup_bits(key)).then_some(action)
        })
}

pub(crate) fn should_drop_unbound_prefix_key(table_name: &str, key: KeyCode) -> bool {
    table_name == PREFIX_TABLE && !key_code_is_mouse_move(key)
}

fn target_is_in_copy_mode(state: &HandlerState, target: &PaneTarget) -> bool {
    state
        .transcript_handle(target)
        .ok()
        .is_some_and(|transcript| {
            transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned")
                .copy_mode_state()
                .is_some()
        })
}

fn decode_escape_key(input: &[u8], backspace: Option<u8>) -> AttachedKeyDecode {
    match decode_extended_key(input, backspace) {
        ExtendedKeyDecode::Matched { size, key } => {
            return AttachedKeyDecode::Matched { size, key }
        }
        ExtendedKeyDecode::Partial => return AttachedKeyDecode::Partial,
        ExtendedKeyDecode::Invalid => {}
    }

    match input.get(1).copied() {
        Some(b'[') => return decode_csi_key(input),
        Some(b'O') => return decode_ss3_key(input),
        Some(byte) if input.len() < 3 && byte.is_ascii() && !byte.is_ascii_control() => {
            return AttachedKeyDecode::Matched {
                size: 2,
                key: u64::from(byte) | rmux_core::KEYC_META | rmux_core::KEYC_IMPLIED_META,
            };
        }
        Some(byte) if byte != b'\x1b' => {
            if let Some(key) = control_byte_key(byte) {
                return AttachedKeyDecode::Matched {
                    size: 2,
                    key: key | rmux_core::KEYC_META | rmux_core::KEYC_IMPLIED_META,
                };
            }
            if input.len() < 3 {
                return AttachedKeyDecode::Partial;
            }
        }
        Some(_) if input.len() < 3 => return AttachedKeyDecode::Partial,
        None => return AttachedKeyDecode::Partial,
        Some(_) => {}
    }

    if input.len() < 3 {
        return AttachedKeyDecode::Partial;
    }

    if let Some(&byte) = input.get(1) {
        if byte.is_ascii() && !byte.is_ascii_control() {
            return AttachedKeyDecode::Matched {
                size: 2,
                key: u64::from(byte) | rmux_core::KEYC_META | rmux_core::KEYC_IMPLIED_META,
            };
        }
    }

    AttachedKeyDecode::Invalid
}

fn decode_csi_key(input: &[u8]) -> AttachedKeyDecode {
    let Some(third) = input.get(2).copied() else {
        return AttachedKeyDecode::Partial;
    };

    if let Some(key) = match third {
        b'A' => key_string_lookup_string("Up"),
        b'B' => key_string_lookup_string("Down"),
        b'C' => key_string_lookup_string("Right"),
        b'D' => key_string_lookup_string("Left"),
        b'H' => key_string_lookup_string("Home"),
        b'F' => key_string_lookup_string("End"),
        b'Z' => key_string_lookup_string("BTab"),
        _ => None,
    } {
        return AttachedKeyDecode::Matched { size: 3, key };
    }

    let Some(tilde_index) = input[2..].iter().position(|byte| *byte == b'~') else {
        return if input[2..].iter().all(u8::is_ascii_digit) {
            AttachedKeyDecode::Partial
        } else {
            AttachedKeyDecode::Invalid
        };
    };
    let tilde_index = tilde_index + 2;
    let Ok(number) = std::str::from_utf8(&input[2..tilde_index])
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or(())
    else {
        return AttachedKeyDecode::Invalid;
    };
    let key_name = match number {
        1 | 7 => "Home",
        2 => "IC",
        3 => "DC",
        4 | 8 => "End",
        5 => "PageUp",
        6 => "PageDown",
        15 => "F5",
        17 => "F6",
        18 => "F7",
        19 => "F8",
        20 => "F9",
        21 => "F10",
        23 => "F11",
        24 => "F12",
        _ => return AttachedKeyDecode::Invalid,
    };
    key_string_lookup_string(key_name).map_or(AttachedKeyDecode::Invalid, |key| {
        AttachedKeyDecode::Matched {
            size: tilde_index + 1,
            key,
        }
    })
}

fn decode_ss3_key(input: &[u8]) -> AttachedKeyDecode {
    let Some(third) = input.get(2).copied() else {
        return AttachedKeyDecode::Partial;
    };
    let key_name = match third {
        b'A' => "Up",
        b'B' => "Down",
        b'C' => "Right",
        b'D' => "Left",
        b'F' => "End",
        b'H' => "Home",
        b'P' => "F1",
        b'Q' => "F2",
        b'R' => "F3",
        b'S' => "F4",
        _ => return AttachedKeyDecode::Invalid,
    };
    key_string_lookup_string(key_name).map_or(AttachedKeyDecode::Invalid, |key| {
        AttachedKeyDecode::Matched { size: 3, key }
    })
}

fn control_byte_key(byte: u8) -> Option<KeyCode> {
    let name = match byte {
        0x00 => "C-Space",
        b'\r' => "Enter",
        b'\t' => "Tab",
        0x01..=0x1a => return control_alpha_key(byte),
        0x1b => return None,
        0x1c => "C-\\",
        0x1d => "C-]",
        0x1e => "C-^",
        0x1f => "C-_",
        0x7f => "BSpace",
        _ => return None,
    };
    key_string_lookup_string(name)
}

fn control_alpha_key(byte: u8) -> Option<KeyCode> {
    let ch = char::from(b'a' + (byte - 1));
    key_string_lookup_string(&format!("C-{ch}"))
}

#[cfg(test)]
mod tests {
    use super::{decode_attached_key, AttachedKeyDecode};
    use rmux_core::{key_code_lookup_bits, key_string_lookup_string};

    fn assert_attached_key(sequence: &[u8], expected_name: &str) {
        let AttachedKeyDecode::Matched { size, key } = decode_attached_key(sequence, None) else {
            panic!("{expected_name} should decode from {sequence:?}");
        };
        assert_eq!(size, sequence.len(), "{expected_name} consumed size");
        assert_eq!(
            key_code_lookup_bits(key),
            key_code_lookup_bits(key_string_lookup_string(expected_name).expect("key parses")),
            "{expected_name} key bits"
        );
    }

    #[test]
    fn attached_escape_prefix_decodes_meta_printable_digits() {
        let AttachedKeyDecode::Matched { size, key } = decode_attached_key(b"\x1b1", None) else {
            panic!("expected attached meta-digit decode");
        };
        assert_eq!(size, 2);
        assert_eq!(
            key_code_lookup_bits(key),
            key_code_lookup_bits(key_string_lookup_string("M-1").expect("M-1 parses"))
        );
    }

    #[test]
    fn attached_escape_prefix_decodes_meta_printable_letters() {
        let AttachedKeyDecode::Matched { size, key } = decode_attached_key(b"\x1ba", None) else {
            panic!("expected attached meta-letter decode");
        };
        assert_eq!(size, 2);
        assert_eq!(
            key_code_lookup_bits(key),
            key_code_lookup_bits(key_string_lookup_string("M-a").expect("M-a parses"))
        );
    }

    #[test]
    fn attached_escape_prefix_decodes_meta_control_keys() {
        for (sequence, name) in [
            (b"\x1b\x00".as_slice(), "M-C-Space"),
            (b"\x1b\x01".as_slice(), "M-C-a"),
            (b"\x1b\x08".as_slice(), "M-C-h"),
            (b"\x1b\x0a".as_slice(), "M-C-j"),
            (b"\x1b\x1c".as_slice(), "M-C-\\"),
            (b"\x1b\x1d".as_slice(), "M-C-]"),
            (b"\x1b\x1e".as_slice(), "M-C-^"),
            (b"\x1b\x1f".as_slice(), "M-C-_"),
            (b"\x1b\x7f".as_slice(), "M-BSpace"),
        ] {
            assert_attached_key(sequence, name);
        }
    }

    #[test]
    fn attached_escape_prefix_prefers_plain_cursor_sequences_over_meta_bracket() {
        let AttachedKeyDecode::Matched { size, key } = decode_attached_key(b"\x1b[B", None) else {
            panic!("expected attached down-arrow decode");
        };
        assert_eq!(size, 3);
        assert_eq!(
            key_code_lookup_bits(key),
            key_code_lookup_bits(key_string_lookup_string("Down").expect("Down parses"))
        );
    }

    #[test]
    fn attached_escape_prefix_waits_for_ambiguous_csi_and_ss3_prefixes() {
        assert_eq!(
            decode_attached_key(b"\x1b[", None),
            AttachedKeyDecode::Partial
        );
        assert_eq!(
            decode_attached_key(b"\x1bO", None),
            AttachedKeyDecode::Partial
        );
    }

    #[test]
    fn attached_escape_prefix_decodes_navigation_and_function_sequences() {
        for (sequence, name, size) in [
            (b"\x1b[A".as_slice(), "Up", 3),
            (b"\x1bOA".as_slice(), "Up", 3),
            (b"\x1b[H".as_slice(), "Home", 3),
            (b"\x1b[F".as_slice(), "End", 3),
            (b"\x1b[Z".as_slice(), "BTab", 3),
            (b"\x1bOP".as_slice(), "F1", 3),
            (b"\x1bOS".as_slice(), "F4", 3),
            (b"\x1b[3~".as_slice(), "DC", 4),
            (b"\x1b[15~".as_slice(), "F5", 5),
        ] {
            let AttachedKeyDecode::Matched { size: actual, key } =
                decode_attached_key(sequence, None)
            else {
                panic!("{name} should decode from {sequence:?}");
            };
            assert_eq!(actual, size, "{name} consumed size");
            assert_eq!(
                key_code_lookup_bits(key),
                key_code_lookup_bits(key_string_lookup_string(name).expect("key parses")),
                "{name} key bits"
            );
        }
    }

    #[test]
    fn attached_control_bytes_match_tmux_key_names() {
        for (sequence, name) in [
            (b"\x00".as_slice(), "C-Space"),
            (b"\x01".as_slice(), "C-a"),
            (b"\x08".as_slice(), "C-h"),
            (b"\x09".as_slice(), "Tab"),
            (b"\x0a".as_slice(), "C-j"),
            (b"\x0d".as_slice(), "Enter"),
            (b"\x1a".as_slice(), "C-z"),
            (b"\x1c".as_slice(), "C-\\"),
            (b"\x1d".as_slice(), "C-]"),
            (b"\x1e".as_slice(), "C-^"),
            (b"\x1f".as_slice(), "C-_"),
            (b"\x7f".as_slice(), "BSpace"),
        ] {
            assert_attached_key(sequence, name);
        }
    }
}
