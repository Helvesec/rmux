use rmux_core::{input::mode, TerminalScreen};
use rmux_proto::TerminalSize;

#[test]
fn kitty_keyboard_falls_back_unless_explicitly_enabled() {
    let mut terminal = TerminalScreen::new(TerminalSize { cols: 80, rows: 24 }, 10);
    let initial_mode = terminal.screen().mode();
    terminal.feed(b"\x1b[?u\x1b[=8u\x1b[>1u\x1b[?u");
    if cfg!(feature = "kitty-keyboard") {
        assert_eq!(terminal.take_replies(), b"\x1b[?0u\x1b[?1u");
        assert_eq!(terminal.screen().keyboard_flags(), 1);
    } else {
        assert!(terminal.take_replies().is_empty());
        assert_eq!(terminal.screen().keyboard_flags(), 0);
    }
    assert_eq!(terminal.screen().mode(), initial_mode);
    terminal.feed(b"\x1b[>4;2m");
    assert_ne!(terminal.screen().mode() & mode::MODE_KEYS_EXTENDED_2, 0);
}
