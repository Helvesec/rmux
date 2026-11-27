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
    key_table_name_for_mode(state, target, target_is_in_copy_mode(state, target))
}

/// Client listings and client renders sweep every session that shows a
/// changed window. A poisoned active pane reports its session's non-copy
/// table instead of failing every request that refreshes its clients.
pub(crate) fn listed_session_key_table_name(
    state: &HandlerState,
    session: rmux_core::ManagedSessionView<'_>,
) -> String {
    let target = active_pane_target(session);
    let in_copy_mode = state
        .transcript_handle(&target)
        .ok()
        .is_some_and(|transcript| {
            transcript
                .lock_for_cross_pane_sweep()
                .is_some_and(|transcript| transcript.copy_mode_state().is_some())
        });
    key_table_name_for_mode(state, &target, in_copy_mode)
}

fn key_table_name_for_mode(
    state: &HandlerState,
    target: &PaneTarget,
    in_copy_mode: bool,
) -> String {
    if in_copy_mode {
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

pub(crate) fn effective_client_key_table_name(
    state: &HandlerState,
    session: rmux_core::ManagedSessionView<'_>,
    transient_table: Option<&str>,
) -> String {
    if let Some(table_name) = transient_table {
        return table_name.to_owned();
    }

    listed_session_key_table_name(state, session)
}

fn active_pane_target(session: rmux_core::ManagedSessionView<'_>) -> PaneTarget {
    PaneTarget::with_window(
        session.name().clone(),
        session.active_window_index(),
        session.active_pane_index(),
    )
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

pub(crate) fn should_drop_unbound_prefix_key(table_name: &str, key: KeyCode) -> bool {
    table_name == PREFIX_TABLE && !key_code_is_mouse_move(key)
}

fn target_is_in_copy_mode(state: &HandlerState, target: &PaneTarget) -> bool {
    state
        .transcript_handle(target)
        .ok()
        .is_some_and(|transcript| transcript.lock().copy_mode_state().is_some())
}

fn decode_escape_key(input: &[u8], backspace: Option<u8>) -> AttachedKeyDecode {
    match decode_extended_key(input, backspace) {
        ExtendedKeyDecode::Matched { size, key } => {
            return AttachedKeyDecode::Matched { size, key }
        }
        ExtendedKeyDecode::Partial => return AttachedKeyDecode::Partial,
        ExtendedKeyDecode::Invalid => {}
    }
    if let Some(decoded) = decode_escape_prefixed_key(input) {
        return decoded;
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

/// tmux 3.7b `tty_keys_next`: Escape before a key of the terminal key tree
/// (a CSI or SS3 sequence) is that key with an explicit Meta, which has no
/// `KEYC_IMPLIED_META`, so the pane receives Escape then the key's own
/// sequence, in order. A key whose sequence already implies Meta stays an
/// Escape followed by that key. The raw table maps Escape before the xterm
/// Home and End to M-Home and M-End with implied Meta. `None` leaves every
/// other Escape-prefixed input to the generic Meta rules.
fn decode_escape_prefixed_key(input: &[u8]) -> Option<AttachedKeyDecode> {
    let [b'\x1b', inner @ ..] = input else {
        return None;
    };
    let [b'\x1b', introducer, ..] = inner else {
        return None;
    };
    let decoded = match *introducer {
        b'[' => decode_xterm_csi_key(inner),
        b'O' => decode_ss3_key(inner),
        _ => return None,
    };
    match decoded {
        AttachedKeyDecode::Partial => Some(AttachedKeyDecode::Partial),
        AttachedKeyDecode::Invalid => None,
        AttachedKeyDecode::Matched { size, key } => Some(if is_xterm_home_or_end(&inner[..size]) {
            AttachedKeyDecode::Matched {
                size: size + 1,
                key: key | rmux_core::KEYC_META | rmux_core::KEYC_IMPLIED_META,
            }
        } else if key & rmux_core::KEYC_IMPLIED_META != 0 {
            AttachedKeyDecode::Matched {
                size: 1,
                key: KeyCode::from(b'\x1b'),
            }
        } else {
            AttachedKeyDecode::Matched {
                size: size + 1,
                key: key | rmux_core::KEYC_META,
            }
        }),
    }
}

fn is_xterm_home_or_end(sequence: &[u8]) -> bool {
    matches!(sequence, b"\x1b[H" | b"\x1b[F" | b"\x1bOH" | b"\x1bOF")
}

/// The CSI keys of tmux 3.7b's terminal key tree: the plain forms, then the
/// xterm forms with a modifier parameter (`tty_default_xterm_keys`).
fn decode_xterm_csi_key(input: &[u8]) -> AttachedKeyDecode {
    match decode_csi_key(input) {
        AttachedKeyDecode::Invalid => decode_xterm_modified_key(input),
        decoded => decoded,
    }
}

/// Longest parameter run of `CSI number ; modifier final` (`24;8`).
const XTERM_MODIFIED_PARAMETERS_MAX: usize = 5;

/// `CSI 1 ; m A..D H F P..S` and `CSI n ; m ~`, with tmux's modifier table
/// `tty_default_xterm_modifiers` (m = 2..9; Meta is implied by the sequence).
pub(crate) fn decode_xterm_modified_key(input: &[u8]) -> AttachedKeyDecode {
    let parameters = input.get(2..).unwrap_or_default();
    let Some(end) = parameters
        .iter()
        .position(|byte| !byte.is_ascii_digit() && *byte != b';')
    else {
        return if parameters.len() <= XTERM_MODIFIED_PARAMETERS_MAX {
            AttachedKeyDecode::Partial
        } else {
            AttachedKeyDecode::Invalid
        };
    };
    let Some((number, modifier)) = std::str::from_utf8(&parameters[..end])
        .ok()
        .and_then(|text| text.split_once(';'))
    else {
        return AttachedKeyDecode::Invalid;
    };
    let key_name = match (number, parameters[end]) {
        ("1", b'A') => "Up",
        ("1", b'B') => "Down",
        ("1", b'C') => "Right",
        ("1", b'D') => "Left",
        ("1", b'H') => "Home",
        ("1", b'F') => "End",
        ("1", b'P') => "F1",
        ("1", b'Q') => "F2",
        ("1", b'R') => "F3",
        ("1", b'S') => "F4",
        ("15", b'~') => "F5",
        ("17", b'~') => "F6",
        ("18", b'~') => "F7",
        ("19", b'~') => "F8",
        ("20", b'~') => "F9",
        ("21", b'~') => "F10",
        ("23", b'~') => "F11",
        ("24", b'~') => "F12",
        ("5", b'~') => "PPage",
        ("6", b'~') => "NPage",
        ("2", b'~') => "IC",
        ("3", b'~') => "DC",
        _ => return AttachedKeyDecode::Invalid,
    };
    let Some(modifiers) = modifier
        .parse::<u8>()
        .ok()
        .and_then(xterm_parameter_modifiers)
    else {
        return AttachedKeyDecode::Invalid;
    };
    key_string_lookup_string(key_name).map_or(AttachedKeyDecode::Invalid, |key| {
        AttachedKeyDecode::Matched {
            size: 2 + end + 1,
            key: key | modifiers,
        }
    })
}

fn xterm_parameter_modifiers(parameter: u8) -> Option<KeyCode> {
    let bits = parameter
        .checked_sub(1)
        .filter(|bits| (1..=8).contains(bits))?;
    let mut modifiers = 0;
    if bits & 1 != 0 {
        modifiers |= rmux_core::KEYC_SHIFT;
    }
    if bits & (2 | 8) != 0 {
        modifiers |= rmux_core::KEYC_META | rmux_core::KEYC_IMPLIED_META;
    }
    if bits & 4 != 0 {
        modifiers |= rmux_core::KEYC_CTRL;
    }
    Some(modifiers)
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
    use super::{decode_attached_key, effective_client_key_table_name, AttachedKeyDecode};
    use crate::pane_terminals::HandlerState;
    use rmux_core::{key_code_lookup_bits, key_string_lookup_string, KEYC_META};
    use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize};

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
    fn effective_client_table_uses_session_default_beneath_transient_table() {
        let session_name = SessionName::new("alpha").expect("valid session name");
        let session = crate::session_fixture::SessionFixture::new(
            session_name.clone(),
            TerminalSize { cols: 80, rows: 24 },
        );
        let mut state = HandlerState::default();
        state
            .options
            .set(
                ScopeSelector::Session(session_name),
                OptionName::KeyTable,
                "off".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("session key-table option should be accepted");

        assert_eq!(
            effective_client_key_table_name(&state, session.view(), None),
            "off"
        );
        assert_eq!(
            effective_client_key_table_name(&state, session.view(), Some("prefix")),
            "prefix"
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

    /// tmux 3.7b `tty_keys_next`: Escape then a CSI or SS3 key is that key
    /// with an explicit Meta, consumed together.
    #[test]
    fn attached_escape_before_a_csi_or_ss3_key_is_that_key_with_explicit_meta() {
        for (sequence, name) in [
            (b"\x1b\x1b[15~".as_slice(), "F5"),
            (b"\x1b\x1b[24~".as_slice(), "F12"),
            (b"\x1b\x1b[3~".as_slice(), "DC"),
            (b"\x1b\x1b[6~".as_slice(), "NPage"),
            (b"\x1b\x1b[A".as_slice(), "Up"),
            (b"\x1b\x1bOA".as_slice(), "Up"),
            (b"\x1b\x1bOP".as_slice(), "F1"),
            (b"\x1b\x1b[Z".as_slice(), "BTab"),
            (b"\x1b\x1b[1;2A".as_slice(), "S-Up"),
            (b"\x1b\x1b[1;5D".as_slice(), "C-Left"),
            (b"\x1b\x1b[15;2~".as_slice(), "S-F5"),
            (b"\x1b\x1b[3;5~".as_slice(), "C-DC"),
            (b"\x1b\x1b[1;2P".as_slice(), "S-F1"),
        ] {
            let AttachedKeyDecode::Matched { size, key } = decode_attached_key(sequence, None)
            else {
                panic!("Escape then {name} should decode from {sequence:?}");
            };
            assert_eq!(size, sequence.len(), "Escape then {name} consumed size");
            assert_eq!(
                key,
                key_string_lookup_string(name).expect("key parses") | KEYC_META,
                "Escape then {name} is {name} with Meta and without implied Meta"
            );
        }
    }

    #[test]
    fn attached_escape_before_an_implied_meta_key_stays_an_escape_key() {
        for sequence in [b"\x1b\x1b[1;3A".as_slice(), b"\x1b\x1b[15;3~".as_slice()] {
            assert_eq!(
                decode_attached_key(sequence, None),
                AttachedKeyDecode::Matched { size: 1, key: 0x1b },
                "{sequence:?}"
            );
        }
    }

    #[test]
    fn attached_escape_before_xterm_home_and_end_is_implied_meta_like_tmux() {
        for (sequence, name) in [
            (b"\x1b\x1b[H".as_slice(), "M-Home"),
            (b"\x1b\x1b[F".as_slice(), "M-End"),
            (b"\x1b\x1bOH".as_slice(), "M-Home"),
            (b"\x1b\x1bOF".as_slice(), "M-End"),
        ] {
            assert_eq!(
                decode_attached_key(sequence, None),
                AttachedKeyDecode::Matched {
                    size: 4,
                    key: key_string_lookup_string(name).expect("key parses"),
                },
                "{sequence:?}"
            );
        }
    }

    #[test]
    fn attached_escape_before_an_unfinished_key_waits_and_other_meta_input_is_unchanged() {
        for partial in [
            b"\x1b\x1b".as_slice(),
            b"\x1b\x1b[".as_slice(),
            b"\x1b\x1b[15".as_slice(),
            b"\x1b\x1b[15;".as_slice(),
            b"\x1b\x1b[1;2".as_slice(),
            b"\x1b\x1bO".as_slice(),
        ] {
            assert_eq!(
                decode_attached_key(partial, None),
                AttachedKeyDecode::Partial,
                "{partial:?}"
            );
        }
        for invalid in [
            b"\x1b\x1ba".as_slice(),
            b"\x1b\x1b[200~".as_slice(),
            b"\x1b\x1b[1;2~".as_slice(),
            b"\x1b\x1b[1;10A".as_slice(),
        ] {
            assert_eq!(
                decode_attached_key(invalid, None),
                AttachedKeyDecode::Invalid,
                "{invalid:?}"
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
