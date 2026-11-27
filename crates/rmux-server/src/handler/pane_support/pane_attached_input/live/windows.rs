use super::*;

pub(in crate::handler::pane_support::pane_attached_input) fn windows_console_key_event(
    key: rmux_proto::AttachedWindowsConsoleKey,
) -> WindowsConsoleKeyEvent {
    WindowsConsoleKeyEvent::new(
        key.virtual_key_code(),
        key.virtual_scan_code(),
        key.unicode_char(),
        key.control_key_state(),
        key.repeat_count(),
    )
}

pub(super) fn windows_console_binding_key(
    decoded: rmux_core::KeyCode,
    key: WindowsConsoleKeyEvent,
) -> rmux_core::KeyCode {
    windows_console_binding_override_key(decoded, key).unwrap_or(decoded)
}

pub(super) fn windows_synthetic_console_key_for_decoded_key(
    decoded: rmux_core::KeyCode,
) -> Option<WindowsConsoleKeyEvent> {
    key_matches_name(decoded, "C-d").then(WindowsConsoleKeyEvent::ctrl_d)
}

pub(super) fn windows_console_binding_override_key(
    decoded: rmux_core::KeyCode,
    key: WindowsConsoleKeyEvent,
) -> Option<rmux_core::KeyCode> {
    const RIGHT_ALT_PRESSED: u32 = 0x0001;
    const LEFT_ALT_PRESSED: u32 = 0x0002;
    const LEFT_CTRL_PRESSED: u32 = 0x0008;
    const RIGHT_CTRL_PRESSED: u32 = 0x0004;
    const CTRL_PRESSED: u32 = LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED;

    let control_key_state = key.control_key_state();
    if decoded & KEYC_CTRL != 0 || control_key_state & CTRL_PRESSED == 0 {
        return None;
    }

    if control_key_state & RIGHT_ALT_PRESSED != 0 {
        return None;
    }
    if control_key_state & LEFT_ALT_PRESSED != 0 && control_key_state & RIGHT_CTRL_PRESSED == 0 {
        return None;
    }

    let character = char::from_u32(u32::from(key.unicode_char()))?;
    if !character.is_ascii() || character.is_ascii_control() {
        return None;
    }

    let preserved_modifiers = decoded & (KEYC_META | KEYC_IMPLIED_META | KEYC_SHIFT);
    Some(character.to_ascii_lowercase() as rmux_core::KeyCode | KEYC_CTRL | preserved_modifiers)
}

pub(super) fn key_matches_name(key: rmux_core::KeyCode, name: &str) -> bool {
    windows_key_code_named(name).is_some_and(|expected| expected == key)
}

pub(super) fn windows_key_code_named(name: &str) -> Option<rmux_core::KeyCode> {
    key_string_lookup_string(name).map(key_code_lookup_bits)
}

#[cfg(all(test, windows))]
mod windows_console_binding_tests {
    use rmux_core::{KEYC_CTRL, KEYC_IMPLIED_META, KEYC_META, KEYC_SHIFT};
    use rmux_pty::WindowsConsoleKeyEvent;

    use super::windows_console_binding_override_key;

    const RIGHT_ALT_PRESSED: u32 = 0x0001;
    const LEFT_ALT_PRESSED: u32 = 0x0002;
    const RIGHT_CTRL_PRESSED: u32 = 0x0004;
    const LEFT_CTRL_PRESSED: u32 = 0x0008;

    fn key(unicode_char: char, control_key_state: u32) -> WindowsConsoleKeyEvent {
        WindowsConsoleKeyEvent::new(0, 0, unicode_char as u16, control_key_state, 1)
    }

    #[test]
    fn alt_gr_is_not_promoted_to_control_binding() {
        assert_eq!(
            windows_console_binding_override_key(
                b'[' as u64,
                key('[', RIGHT_ALT_PRESSED | LEFT_CTRL_PRESSED),
            ),
            None
        );
    }

    #[test]
    fn plain_left_control_promotes_printable_character() {
        assert_eq!(
            windows_console_binding_override_key(b';' as u64, key(';', LEFT_CTRL_PRESSED)),
            Some(b';' as u64 | KEYC_CTRL)
        );
    }

    #[test]
    fn meta_and_shift_modifiers_survive_control_promotion() {
        let decoded = b';' as u64 | KEYC_META | KEYC_IMPLIED_META | KEYC_SHIFT;

        assert_eq!(
            windows_console_binding_override_key(decoded, key(';', LEFT_CTRL_PRESSED)),
            Some(b';' as u64 | KEYC_CTRL | KEYC_META | KEYC_IMPLIED_META | KEYC_SHIFT)
        );
    }

    #[test]
    fn left_alt_without_right_ctrl_is_not_alt_gr_or_control_override() {
        assert_eq!(
            windows_console_binding_override_key(b'q' as u64, key('q', LEFT_ALT_PRESSED)),
            None
        );
    }

    #[test]
    fn right_control_still_promotes_printable_character() {
        assert_eq!(
            windows_console_binding_override_key(b'a' as u64, key('A', RIGHT_CTRL_PRESSED)),
            Some(b'a' as u64 | KEYC_CTRL)
        );
    }
}
