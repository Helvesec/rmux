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
            state
                .key_bindings
                .get_binding(table_name, KEYC_ANY)
                .cloned()
        })
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

    if input.len() < 3 {
        if let Some(&byte) = input.get(1)
            && byte.is_ascii() && !byte.is_ascii_control() {
                return AttachedKeyDecode::Matched {
                    size: 2,
                    key: u64::from(byte) | rmux_core::KEYC_META | rmux_core::KEYC_IMPLIED_META,
                };
            }
        return AttachedKeyDecode::Partial;
    }
    let key = match &input[..3] {
        b"\x1b[A" | b"\x1bOA" => key_string_lookup_string("Up"),
        b"\x1b[B" | b"\x1bOB" => key_string_lookup_string("Down"),
        b"\x1b[C" | b"\x1bOC" => key_string_lookup_string("Right"),
        b"\x1b[D" | b"\x1bOD" => key_string_lookup_string("Left"),
        _ => None,
    };

    if let Some(key) = key {
        return AttachedKeyDecode::Matched { size: 3, key };
    }

    if let Some(&byte) = input.get(1)
        && byte.is_ascii() && !byte.is_ascii_control() {
            return AttachedKeyDecode::Matched {
                size: 2,
                key: u64::from(byte) | rmux_core::KEYC_META | rmux_core::KEYC_IMPLIED_META,
            };
        }

    AttachedKeyDecode::Invalid
}

fn control_byte_key(byte: u8) -> Option<KeyCode> {
    match byte {
        b'\r' | b'\n' => key_string_lookup_string("Enter"),
        b'\t' => key_string_lookup_string("Tab"),
        0x7f | 0x08 => key_string_lookup_string("BSpace"),
        0x01..=0x1a => {
            let ch = char::from(b'a' + (byte - 1));
            key_string_lookup_string(&format!("C-{ch}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_attached_key, AttachedKeyDecode};
    use rmux_core::{key_code_lookup_bits, key_string_lookup_string};

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
}
