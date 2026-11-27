use rmux_proto::TerminalSize;

use crate::{GridRenderOptions, ScreenCaptureRange, TerminalScreen};

fn capture(screen: &crate::Screen) -> Vec<u8> {
    screen.capture_transcript(ScreenCaptureRange::default(), GridRenderOptions::default())
}

#[test]
fn fragmented_synchronized_output_keeps_one_stable_presentation_until_release() {
    let mut terminal = TerminalScreen::new(TerminalSize { cols: 48, rows: 4 }, 100);
    terminal.feed(b"\x1b[2J\x1b[HSTABLE_BEFORE_SYNC");

    terminal.feed(b"\x1b[?20");
    terminal.feed(b"26h\x1b[2J\x1b[HPAUSED_PARTIAL_ONLY");

    assert!(terminal.synchronized_output_active());
    assert!(capture(terminal.screen())
        .windows(b"PAUSED_PARTIAL_ONLY".len())
        .any(|window| window == b"PAUSED_PARTIAL_ONLY"));
    assert!(capture(terminal.presentation_screen())
        .windows(b"STABLE_BEFORE_SYNC".len())
        .any(|window| window == b"STABLE_BEFORE_SYNC"));

    terminal.feed(b"\x1b[2J\x1b[HPAUSED_FINAL\x1b[?20");
    assert!(capture(terminal.presentation_screen())
        .windows(b"STABLE_BEFORE_SYNC".len())
        .any(|window| window == b"STABLE_BEFORE_SYNC"));
    terminal.feed(b"26l");

    assert!(!terminal.synchronized_output_active());
    let released = capture(terminal.presentation_screen());
    assert!(released
        .windows(b"PAUSED_FINAL".len())
        .any(|window| window == b"PAUSED_FINAL"));
    assert!(!released
        .windows(b"PAUSED_PARTIAL_ONLY".len())
        .any(|window| window == b"PAUSED_PARTIAL_ONLY"));
}

#[test]
fn resize_updates_stable_and_pending_synchronized_screens() {
    let mut terminal = TerminalScreen::new(TerminalSize { cols: 24, rows: 3 }, 100);
    terminal.feed(b"STABLE\x1b[?2026h\x1b[2J\x1b[HPENDING");

    terminal.resize(TerminalSize { cols: 12, rows: 2 });

    assert_eq!(
        terminal.presentation_screen().size(),
        TerminalSize { cols: 12, rows: 2 }
    );
    assert_eq!(terminal.screen().size(), TerminalSize { cols: 12, rows: 2 });
    assert!(capture(terminal.presentation_screen())
        .windows(b"STABLE".len())
        .any(|window| window == b"STABLE"));
    assert!(!capture(terminal.presentation_screen())
        .windows(b"PENDING".len())
        .any(|window| window == b"PENDING"));

    terminal.feed(b"\x1b[?2026l");
    assert!(capture(terminal.presentation_screen())
        .windows(b"PENDING".len())
        .any(|window| window == b"PENDING"));
}

#[test]
fn ris_preserves_cursor_shape_but_returns_it_to_steady() {
    for (blinking, steady) in [(1, 2), (3, 4), (5, 6)] {
        let mut terminal = TerminalScreen::new(TerminalSize { cols: 24, rows: 3 }, 100);
        terminal.feed(format!("\x1b[{blinking} q\x1bc").as_bytes());

        assert_eq!(
            terminal.screen().resolved_cursor_style(),
            steady,
            "RIS must retain shape and reset blink for DECSCUSR {blinking}"
        );
    }
}
