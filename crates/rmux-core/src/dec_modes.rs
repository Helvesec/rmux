//! Re-assert DEC private terminal modes from a tracked mode bitmap.
//!
//! When a viewer (re)joins a live session — a reconnecting browser, a late
//! web-share client — it receives a snapshot that paints the visible grid but,
//! by itself, does not restore the *interactive* terminal modes the inner
//! program had asserted (mouse reporting, bracketed paste, application cursor
//! keys, cursor style, ...). Without them the reconstructed view looks right
//! but behaves wrong: clicks are ignored, paste breaks, arrow keys misbehave.
//!
//! [`render_dec_modes`] emits the escape sequences that re-assert enabled modes
//! from a [`crate::Screen`]'s mode bitmap. [`render_dec_modes_for_snapshot`]
//! emits a complete mode state, including explicit resets for stale browser
//! state after backlog resync.
//!
//! Adapted clean-room from the `render_dec_modes` helper in rmux PR #26
//! (passthrough mode, by @gilescope): rewritten against this crate's `Screen`
//! mode API and reused as a shared primitive for the web-share snapshot.

use crate::input::mode;

/// Appends the DEC private *interactive* mode setters implied by `mode_bits`
/// (and the DECSCUSR `cursor_style`) to `out`.
///
/// Only persistent modes that differ from a terminal's post-soft-reset defaults
/// are emitted: mouse tracking (1000/1002/1003 plus the 1005/1006 encodings),
/// bracketed paste (2004), focus events (1004), application cursor keys and
/// keypad, insert mode, CR/LF, theme updates (2031), `modifyOtherKeys`, and the
/// cursor style. Synchronized output (2026) is intentionally excluded: it is a
/// transient begin/end batch marker, never a persistent state to re-assert.
///
/// These modes are independent of grid painting, so a caller may emit them
/// immediately after a reset prefix. Layout modes that interact with painting —
/// alternate screen (1049), scroll region (DECSTBM), origin mode (6) — are
/// intentionally *not* emitted here; a caller that needs them must order them
/// around the painted content itself (see the web-share snapshot).
pub fn render_dec_modes(mode_bits: u32, cursor_style: u32, out: &mut Vec<u8>) {
    let on = |bit: u32| mode_bits & bit != 0;

    // On-by-default modes: emit the reset only when currently off.
    if !on(mode::MODE_CURSOR) {
        out.extend_from_slice(b"\x1b[?25l");
    }
    if !on(mode::MODE_WRAP) {
        out.extend_from_slice(b"\x1b[?7l");
    }

    // Off-by-default mode setters.
    if on(mode::MODE_INSERT) {
        out.extend_from_slice(b"\x1b[4h");
    }
    if on(mode::MODE_KCURSOR) {
        // DECCKM — application cursor keys (arrows emit `\x1bOA` etc.).
        out.extend_from_slice(b"\x1b[?1h");
    }
    if on(mode::MODE_KKEYPAD) {
        // DECPAM — application keypad (note the `\x1b=` form, no CSI).
        out.extend_from_slice(b"\x1b=");
    }
    if on(mode::MODE_CRLF) {
        // LNM is an ANSI mode (CSI 20 h), not a DEC private mode (no `?`).
        out.extend_from_slice(b"\x1b[20h");
    }
    if on(mode::MODE_FOCUSON) {
        out.extend_from_slice(b"\x1b[?1004h");
    }
    if on(mode::MODE_BRACKETPASTE) {
        out.extend_from_slice(b"\x1b[?2004h");
    }
    if on(mode::MODE_THEME_UPDATES) {
        out.extend_from_slice(b"\x1b[?2031h");
    }
    // NOTE: synchronized output (?2026) is deliberately NOT re-asserted. It is a
    // *transient* batch marker (begin/end pairs); re-emitting `?2026h` from a
    // static snapshot would leave a conforming emulator (xterm.js) waiting
    // forever for the matching end-of-batch and freeze/blank the screen.

    // Mouse tracking family — the levels are mutually exclusive at the
    // terminal, so pick the highest one that is set.
    if on(mode::MODE_MOUSE_ALL) {
        out.extend_from_slice(b"\x1b[?1003h");
    } else if on(mode::MODE_MOUSE_BUTTON) {
        out.extend_from_slice(b"\x1b[?1002h");
    } else if on(mode::MODE_MOUSE_STANDARD) {
        out.extend_from_slice(b"\x1b[?1000h");
    }
    // Mouse encoding (SGR preferred over the legacy UTF-8 form).
    if on(mode::MODE_MOUSE_SGR) {
        out.extend_from_slice(b"\x1b[?1006h");
    } else if on(mode::MODE_MOUSE_UTF8) {
        out.extend_from_slice(b"\x1b[?1005h");
    }

    // Keyboard enhancement protocols.
    if on(mode::MODE_KEYS_KITTY) {
        out.extend_from_slice(b"\x1b[>1u");
    } else if on(mode::MODE_KEYS_EXTENDED_2) {
        out.extend_from_slice(b"\x1b[>4;2m");
    } else if on(mode::MODE_KEYS_EXTENDED) {
        out.extend_from_slice(b"\x1b[>4;1m");
    }

    // Cursor style (DECSCUSR). 0 == "terminal default" → leave untouched.
    if cursor_style != 0 {
        out.extend_from_slice(format!("\x1b[{cursor_style} q").as_bytes());
    }
}

/// Appends a complete set of tracked terminal-mode transitions for a snapshot.
///
/// A web snapshot may be delivered to a browser terminal that already exists and
/// missed earlier live bytes. This function therefore does not assume post-reset
/// defaults: it explicitly clears transient or off-by-default modes before
/// re-enabling the modes present in `mode_bits`. Synchronized output (`?2026`)
/// is always ended, never started, because a static snapshot must not leave the
/// browser waiting for a later batch terminator.
pub fn render_dec_modes_for_snapshot(mode_bits: u32, cursor_style: u32, out: &mut Vec<u8>) {
    let on = |bit: u32| mode_bits & bit != 0;

    out.extend_from_slice(if on(mode::MODE_WRAP) {
        b"\x1b[?7h".as_slice()
    } else {
        b"\x1b[?7l".as_slice()
    });
    out.extend_from_slice(if on(mode::MODE_INSERT) {
        b"\x1b[4h".as_slice()
    } else {
        b"\x1b[4l".as_slice()
    });
    out.extend_from_slice(if on(mode::MODE_KCURSOR) {
        b"\x1b[?1h".as_slice()
    } else {
        b"\x1b[?1l".as_slice()
    });
    out.extend_from_slice(if on(mode::MODE_KKEYPAD) {
        b"\x1b=".as_slice()
    } else {
        b"\x1b>".as_slice()
    });
    out.extend_from_slice(if on(mode::MODE_CRLF) {
        b"\x1b[20h".as_slice()
    } else {
        b"\x1b[20l".as_slice()
    });
    out.extend_from_slice(if on(mode::MODE_FOCUSON) {
        b"\x1b[?1004h".as_slice()
    } else {
        b"\x1b[?1004l".as_slice()
    });
    out.extend_from_slice(if on(mode::MODE_BRACKETPASTE) {
        b"\x1b[?2004h".as_slice()
    } else {
        b"\x1b[?2004l".as_slice()
    });
    out.extend_from_slice(if on(mode::MODE_THEME_UPDATES) {
        b"\x1b[?2031h".as_slice()
    } else {
        b"\x1b[?2031l".as_slice()
    });

    out.extend_from_slice(b"\x1b[?2026l");

    out.extend_from_slice(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l");
    if on(mode::MODE_MOUSE_ALL) {
        out.extend_from_slice(b"\x1b[?1003h");
    } else if on(mode::MODE_MOUSE_BUTTON) {
        out.extend_from_slice(b"\x1b[?1002h");
    } else if on(mode::MODE_MOUSE_STANDARD) {
        out.extend_from_slice(b"\x1b[?1000h");
    }

    out.extend_from_slice(b"\x1b[?1005l\x1b[?1006l");
    if on(mode::MODE_MOUSE_SGR) {
        out.extend_from_slice(b"\x1b[?1006h");
    } else if on(mode::MODE_MOUSE_UTF8) {
        out.extend_from_slice(b"\x1b[?1005h");
    }

    if on(mode::MODE_KEYS_KITTY) {
        out.extend_from_slice(b"\x1b[>1u");
    } else if on(mode::MODE_KEYS_EXTENDED_2) {
        out.extend_from_slice(b"\x1b[>4;2m");
    } else if on(mode::MODE_KEYS_EXTENDED) {
        out.extend_from_slice(b"\x1b[>4;1m");
    } else {
        out.extend_from_slice(b"\x1b[<u");
        out.extend_from_slice(b"\x1b[>4;0m");
    }

    out.extend_from_slice(format!("\x1b[{cursor_style} q").as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(mode_bits: u32, cursor_style: u32) -> String {
        let mut out = Vec::new();
        render_dec_modes(mode_bits, cursor_style, &mut out);
        String::from_utf8(out).expect("dec-mode sequences are ascii")
    }

    #[test]
    fn post_reset_defaults_emit_nothing() {
        // Cursor + wrap on, everything else off == post-DECSTR defaults.
        assert_eq!(rendered(mode::MODE_CURSOR | mode::MODE_WRAP, 0), "");
    }

    #[test]
    fn mouse_button_sgr_and_bracketed_paste_are_reasserted() {
        let bits = mode::MODE_CURSOR
            | mode::MODE_WRAP
            | mode::MODE_MOUSE_BUTTON
            | mode::MODE_MOUSE_SGR
            | mode::MODE_BRACKETPASTE;
        let out = rendered(bits, 0);
        assert!(out.contains("\x1b[?1002h"), "{out:?}");
        assert!(out.contains("\x1b[?1006h"), "{out:?}");
        assert!(out.contains("\x1b[?2004h"), "{out:?}");
    }

    #[test]
    fn highest_mouse_level_wins() {
        let bits =
            mode::MODE_CURSOR | mode::MODE_WRAP | mode::MODE_MOUSE_ALL | mode::MODE_MOUSE_BUTTON;
        let out = rendered(bits, 0);
        assert!(out.contains("\x1b[?1003h"), "{out:?}");
        assert!(!out.contains("\x1b[?1002h"), "{out:?}");
    }

    #[test]
    fn application_cursor_keys_and_keypad() {
        let bits = mode::MODE_CURSOR | mode::MODE_WRAP | mode::MODE_KCURSOR | mode::MODE_KKEYPAD;
        let out = rendered(bits, 0);
        assert!(out.contains("\x1b[?1h"), "{out:?}");
        assert!(out.contains("\x1b="), "{out:?}");
    }

    #[test]
    fn cursor_hidden_when_mode_off() {
        let out = rendered(mode::MODE_WRAP, 0); // MODE_CURSOR bit cleared
        assert!(out.contains("\x1b[?25l"), "{out:?}");
    }

    #[test]
    fn cursor_style_emitted_when_set() {
        let out = rendered(mode::MODE_CURSOR | mode::MODE_WRAP, 4);
        assert!(out.contains("\x1b[4 q"), "{out:?}");
    }

    #[test]
    fn kitty_keyboard_mode_is_reasserted_as_csi_u() {
        let bits = mode::MODE_CURSOR
            | mode::MODE_WRAP
            | mode::MODE_KEYS_EXTENDED_2
            | mode::MODE_KEYS_KITTY;
        let out = rendered(bits, 0);
        assert!(out.contains("\x1b[>1u"), "{out:?}");
        assert!(!out.contains("\x1b[>4;2m"), "{out:?}");
    }

    #[test]
    fn snapshot_mode_render_clears_stale_transient_modes() {
        let mut out = Vec::new();
        render_dec_modes_for_snapshot(mode::MODE_CURSOR | mode::MODE_WRAP, 0, &mut out);
        let out = String::from_utf8(out).expect("snapshot modes are ascii");

        assert!(out.contains("\x1b[?2026l"), "{out:?}");
        assert!(out.contains("\x1b[?2004l"), "{out:?}");
        assert!(out.contains("\x1b[?1006l"), "{out:?}");
        assert!(out.contains("\x1b[<u"), "{out:?}");
        assert!(out.contains("\x1b[>4;0m"), "{out:?}");
        assert!(out.contains("\x1b[0 q"), "{out:?}");
        assert!(!out.contains("\x1b[?2026h"), "{out:?}");
    }

    #[test]
    fn snapshot_mode_render_sets_enabled_modes_after_clearing_family_state() {
        let bits = mode::MODE_CURSOR
            | mode::MODE_WRAP
            | mode::MODE_MOUSE_BUTTON
            | mode::MODE_MOUSE_SGR
            | mode::MODE_BRACKETPASTE
            | mode::MODE_KEYS_EXTENDED_2;
        let mut out = Vec::new();
        render_dec_modes_for_snapshot(bits, 4, &mut out);
        let out = String::from_utf8(out).expect("snapshot modes are ascii");

        assert!(out.contains("\x1b[?1002h"), "{out:?}");
        assert!(out.contains("\x1b[?1006h"), "{out:?}");
        assert!(out.contains("\x1b[?2004h"), "{out:?}");
        assert!(out.contains("\x1b[>4;2m"), "{out:?}");
        assert!(out.contains("\x1b[4 q"), "{out:?}");
    }

    #[test]
    fn snapshot_mode_render_prefers_kitty_keyboard_over_xterm_modifier_mode() {
        let bits = mode::MODE_CURSOR
            | mode::MODE_WRAP
            | mode::MODE_KEYS_EXTENDED_2
            | mode::MODE_KEYS_KITTY;
        let mut out = Vec::new();
        render_dec_modes_for_snapshot(bits, 0, &mut out);
        let out = String::from_utf8(out).expect("snapshot modes are ascii");

        assert!(out.contains("\x1b[>1u"), "{out:?}");
        assert!(!out.contains("\x1b[>4;2m"), "{out:?}");
    }
}
