use super::super::pane_prompt_input::{decode_utf8_char, is_utf8_lead_byte, utf8_expected_len};
use crate::key_table::{
    decode_attached_key, default_key_table_name, lookup_attached_key_table_binding,
    matches_prefix_key, session_option_key, AttachedKeyDecode,
};
use rmux_core::{key_code_lookup_bits, KeyCode, KEYC_ANY};
use rmux_proto::PaneTarget;

pub(super) struct PlainKeyDispatchSnapshot {
    dead_key: bool,
    prefix: Option<KeyCode>,
    prefix2: Option<KeyCode>,
    bindings: Vec<KeyCode>,
}

impl PlainKeyDispatchSnapshot {
    pub(super) fn new(state: &crate::pane_terminals::HandlerState, target: &PaneTarget) -> Self {
        let table_name = default_key_table_name(state, target);
        let bindings = state
            .key_bindings
            .table(&table_name)
            .into_iter()
            .flat_map(|table| table.active().keys())
            .map(|key| key_code_lookup_bits(*key))
            .filter(|key| *key == KEYC_ANY || *key <= u64::from(char::MAX as u32))
            .collect();
        Self {
            dead_key: super::super::pane_key_retirement::is_dead_key(state, target),
            prefix: session_option_key(
                state,
                target.session_name(),
                rmux_proto::OptionName::Prefix,
            ),
            prefix2: session_option_key(
                state,
                target.session_name(),
                rmux_proto::OptionName::Prefix2,
            ),
            bindings,
        }
    }

    pub(super) fn requires_dispatch(&self, key: KeyCode) -> bool {
        let key = key_code_lookup_bits(key);
        self.dead_key
            || matches_prefix_key(key, self.prefix, self.prefix2)
            || self
                .bindings
                .iter()
                .any(|binding| *binding == KEYC_ANY || *binding == key)
    }
}

pub(super) fn is_plain_attached_fast_path_input(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes
            .iter()
            .all(|byte| matches!(*byte, b'\r' | b'\n' | b' '..=b'~'))
}

pub(super) fn decode_live_attached_key(input: &[u8], backspace: Option<u8>) -> AttachedKeyDecode {
    match decode_attached_key(input, backspace) {
        AttachedKeyDecode::Invalid => {
            let Some(&first) = input.first() else {
                return AttachedKeyDecode::Partial;
            };
            let (utf8, consumed_prefix, modifiers) = if first == b'\x1b' {
                let Some(&second) = input.get(1) else {
                    return AttachedKeyDecode::Partial;
                };
                if second.is_ascii() {
                    return AttachedKeyDecode::Invalid;
                }
                (
                    &input[1..],
                    1,
                    rmux_core::KEYC_META | rmux_core::KEYC_IMPLIED_META,
                )
            } else {
                (input, 0, 0)
            };
            let Some(&utf8_first) = utf8.first() else {
                return AttachedKeyDecode::Partial;
            };
            if let Some((character, size)) = decode_utf8_char(utf8) {
                return AttachedKeyDecode::Matched {
                    size: consumed_prefix + size,
                    key: character as rmux_core::KeyCode | modifiers,
                };
            }
            if is_utf8_lead_byte(utf8_first) && utf8.len() < utf8_expected_len(utf8_first) {
                AttachedKeyDecode::Partial
            } else {
                AttachedKeyDecode::Invalid
            }
        }
        decoded => decoded,
    }
}

pub(super) fn plain_input_requires_key_dispatch(
    bytes: &[u8],
    target: &PaneTarget,
    state: &crate::pane_terminals::HandlerState,
) -> bool {
    if !state
        .options
        .resolve_array_entries(None, rmux_proto::OptionName::UserKeys)
        .is_empty()
    {
        return true;
    }
    if super::super::pane_key_retirement::is_dead_key(state, target) {
        return true;
    }
    let prefix = session_option_key(state, target.session_name(), rmux_proto::OptionName::Prefix);
    let prefix2 = session_option_key(
        state,
        target.session_name(),
        rmux_proto::OptionName::Prefix2,
    );
    let table_name = default_key_table_name(state, target);
    let mut checked_bytes = [false; 256];

    bytes.iter().copied().any(|byte| {
        let byte_index = usize::from(byte);
        if checked_bytes[byte_index] {
            return false;
        }
        checked_bytes[byte_index] = true;

        let encoded = [byte];
        let AttachedKeyDecode::Matched { key, .. } = decode_live_attached_key(&encoded, None)
        else {
            return true;
        };
        matches_prefix_key(key, prefix, prefix2)
            || lookup_attached_key_table_binding(state, &table_name, key_code_lookup_bits(key))
                .is_some()
    })
}
