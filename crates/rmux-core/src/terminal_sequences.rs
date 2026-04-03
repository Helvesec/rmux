//! Terminal control sequences shared across client and server components.

const ALT_SCREEN_ENTER: &[u8] = b"\x1b[?1049h";
const ALT_SCREEN_EXIT: &[u8] = b"\x1b[?1049l";
/// Returns the preferred alternate-screen enter sequence for `term`.
#[must_use]
pub fn alternate_screen_enter_sequence(term: &str) -> &'static [u8] {
    if uses_xterm_window_ops(term) {
        b"\x1b[?1049h\x1b[22;0;0t"
    } else {
        ALT_SCREEN_ENTER
    }
}

/// Returns the preferred alternate-screen exit sequence for `term`.
#[must_use]
pub fn alternate_screen_exit_sequence(term: &str) -> &'static [u8] {
    if uses_xterm_window_ops(term) {
        b"\x1b[?1049l\x1b[23;0;0t"
    } else {
        ALT_SCREEN_EXIT
    }
}

fn uses_xterm_window_ops(term: &str) -> bool {
    [
        "xterm",
        "rxvt",
        "foot",
        "alacritty",
        "wezterm",
        "kitty",
        "st",
        "vte",
    ]
    .iter()
    .any(|prefix| term.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::{alternate_screen_enter_sequence, alternate_screen_exit_sequence};

    #[test]
    fn xterm_like_term_uses_window_save_and_restore() {
        assert_eq!(
            alternate_screen_enter_sequence("xterm-256color"),
            b"\x1b[?1049h\x1b[22;0;0t"
        );
        assert_eq!(
            alternate_screen_exit_sequence("xterm-256color"),
            b"\x1b[?1049l\x1b[23;0;0t"
        );
    }

    #[test]
    fn screen_like_term_keeps_plain_private_modes() {
        assert_eq!(
            alternate_screen_enter_sequence("screen-256color"),
            b"\x1b[?1049h"
        );
        assert_eq!(
            alternate_screen_exit_sequence("screen-256color"),
            b"\x1b[?1049l"
        );
    }
}
