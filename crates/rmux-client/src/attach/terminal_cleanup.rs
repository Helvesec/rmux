use rmux_core::alternate_screen_exit_sequence;

const DISABLE_MOUSE_FALLBACK: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l";
const DISABLE_BRACKETED_PASTE_FALLBACK: &[u8] = b"\x1b[?2004l";
const DISABLE_FOCUS_FALLBACK: &[u8] = b"\x1b[?1004l";
const DISABLE_THEME_REPORTS_FALLBACK: &[u8] = b"\x1b[?2031l";
const DISABLE_EXTENDED_KEYS_FALLBACK: &[u8] = b"\x1b[>4m";
const DISABLE_MARGINS_FALLBACK: &[u8] = b"\x1b[?69l";
const RESET_CURSOR_STYLE_FALLBACK: &[u8] = b"\x1b[2 q";
const RESET_CURSOR_COLOUR_FALLBACK: &[u8] = b"\x1b]112\x07";

pub(super) fn fallback_attach_stop_sequence(term: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(RESET_CURSOR_COLOUR_FALLBACK);
    bytes.extend_from_slice(RESET_CURSOR_STYLE_FALLBACK);
    bytes.extend_from_slice(DISABLE_FOCUS_FALLBACK);
    bytes.extend_from_slice(DISABLE_THEME_REPORTS_FALLBACK);
    bytes.extend_from_slice(DISABLE_EXTENDED_KEYS_FALLBACK);
    bytes.extend_from_slice(DISABLE_MARGINS_FALLBACK);
    bytes.extend_from_slice(DISABLE_MOUSE_FALLBACK);
    bytes.extend_from_slice(DISABLE_BRACKETED_PASTE_FALLBACK);
    bytes.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
    bytes.extend_from_slice(alternate_screen_exit_sequence(term));
    bytes
}

#[cfg(test)]
mod tests {
    use super::fallback_attach_stop_sequence;

    #[test]
    fn fallback_stop_disables_all_supported_mouse_protocols() {
        let stop = fallback_attach_stop_sequence("xterm-256color");

        assert!(contains(&stop, b"\x1b[?1000l"));
        assert!(contains(&stop, b"\x1b[?1002l"));
        assert!(contains(&stop, b"\x1b[?1003l"));
        assert!(contains(&stop, b"\x1b[?1005l"));
        assert!(contains(&stop, b"\x1b[?1006l"));
    }

    #[test]
    fn fallback_stop_disables_other_attach_terminal_modes() {
        let stop = fallback_attach_stop_sequence("xterm-256color");

        assert!(contains(&stop, b"\x1b[?1004l"));
        assert!(contains(&stop, b"\x1b[?2031l"));
        assert!(contains(&stop, b"\x1b[>4m"));
        assert!(contains(&stop, b"\x1b[?69l"));
        assert!(contains(&stop, b"\x1b[?2004l"));
        assert!(contains(&stop, b"\x1b[?1049l"));
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }
}
