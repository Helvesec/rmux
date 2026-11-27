#[cfg(feature = "kitty-keyboard")]
use rmux_core::input::OscColourSlot;
use rmux_core::{ScreenCaptureRange, TerminalScreen};
use rmux_proto::TerminalSize;

fn terminal(cols: u16, rows: u16, history: usize) -> TerminalScreen {
    TerminalScreen::new(TerminalSize { cols, rows }, history)
}

fn flags(terminal: &TerminalScreen) -> Vec<&'static str> {
    terminal
        .screen()
        .capture_line_format_labels(ScreenCaptureRange {
            start: Some(0),
            end: None,
            start_is_absolute: true,
            end_is_absolute: true,
        })
}

#[test]
fn shell_marks_every_fragment_boundary_bel_and_st() {
    let bytes = b"\x1b]133;A\x07prompt\x1b]133;B\x1b\\ cmd\r\n\x1b]133;C;aid=x\x1b\\out\r\n\x1b]133;D;7\x07done";
    for boundary in 0..=bytes.len() {
        let mut t = terminal(20, 3, 10);
        t.feed(&bytes[..boundary]);
        t.feed(&bytes[boundary..]);
        assert_eq!(flags(&t), ["P", "O", "-"], "split {boundary}");
        assert!(t.pending_bytes().is_empty());
        assert!(t.take_replies().is_empty());
        assert!(t.take_terminal_passthrough().is_empty());
    }
    let mut t = terminal(20, 3, 10);
    for byte in bytes {
        t.feed(&[*byte]);
    }
    assert_eq!(flags(&t), ["P", "O", "-"]);
}

#[test]
fn shell_marks_ignore_incomplete_invalid_and_non_consumed_commands() {
    let mut t = terminal(20, 2, 10);
    for data in ["", "B", "D", "D;255", "X", "AA", "Cgarbage", "a", " A"] {
        t.feed(format!("\x1b]133;{data}\x07").as_bytes());
        assert_eq!(flags(&t), ["-", "-"], "{data:?}");
    }
    t.feed(b"\x1b]133;A");
    assert_eq!(flags(&t), ["-", "-"]);
    t.ground_timer_expired();
    t.feed(b"ok");
    assert_eq!(flags(&t), ["-", "-"]);
    t.feed(b"\x1b]133;A\x18\x1b]133;C\x1a");
    assert_eq!(flags(&t), ["-", "-"]);
    t.feed(b"\x1b]133;A\x1b");
    assert_eq!(flags(&t), ["-", "-"]);
    assert!(t.ground_timer_active());
    t.feed(b"\\");
    assert_eq!(flags(&t), ["P", "-"]);
    t.feed(b"\x1bc\x1b]133;C\x1b[31m");
    // The "ok" row carried a prompt mark before this RIS. A reset received on
    // the principal screen with scroll-on-clear on now transfers the used rows
    // to scrollback instead of destroying them, and a transferred row keeps its
    // metadata, so `flags` — which captures history and then the viewport —
    // reports that preserved mark ahead of the two blank visible rows.
    assert_eq!(t.screen().history_size(), 1);
    assert_eq!(flags(&t), ["P", "-", "-"]);
    t.feed(b"\x1b]133;A\x1b");
    t.ground_timer_expired();
    t.feed(b"\\");
    assert_eq!(flags(&t), ["P", "-", "-"]);
    t.set_input_buffer_limit(16);
    t.feed(b"\x1b]133;A;aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\x07");
    assert_eq!(flags(&t), ["P", "-", "-"]);
}

#[test]
fn shell_marks_are_idempotent_line_metadata_and_do_not_flush_pending_wrap() {
    let mut t = terminal(5, 3, 10);
    t.feed(b"abcde\x1b]133;A\x07\x1b]133;C\x07\x1b]133;A\x07f");
    assert_eq!(flags(&t), ["OPW", "-", "-"]);
    assert_eq!(t.screen().cursor_position(), (1, 1));
    let view = t.screen().absolute_line_view(0).unwrap();
    assert!(view.start_prompt() && view.start_output() && view.wrapped());
    let legacy: Vec<u8> = t
        .screen()
        .capture_line_format_flags(ScreenCaptureRange::default());
    assert_eq!(legacy, b"W--");
}

#[test]
fn shell_marks_follow_scrolling_eviction_insert_delete_and_history_clear() {
    let mut t = terminal(10, 3, 2);
    t.feed(b"\x1b]133;A\x07one\r\n\x1b]133;C\x07two\r\nthree\r\nfour");
    assert_eq!(flags(&t), ["P", "O", "-", "-"]);
    t.feed(b"\r\nfive\r\nsix");
    assert_eq!(flags(&t), ["O", "-", "-", "-", "-"]);
    t.feed(b"\x1b[3J");
    assert_eq!(flags(&t), ["-", "-", "-"]);
    t.feed(b"\x1b[H\x1b]133;A\x07\x1b[2;1H\x1b]133;C\x07\x1b[H\x1b[L");
    assert_eq!(flags(&t), ["-", "P", "O"]);
    t.feed(b"\x1b[M");
    assert_eq!(flags(&t), ["P", "O", "-"]);
    t.feed(b"\x1b[2;3r\x1b[3;1H\n");
    assert_eq!(flags(&t), ["P", "-", "-"]);
}

#[test]
fn shell_marks_whole_line_erase_clears_but_cell_edits_preserve() {
    for erase in ["\x1b[2K", "\x1b[K", "\x1b[99X", "\x1b[99P"] {
        let mut t = terminal(10, 2, 10);
        t.feed(format!("\x1b]133;A\x07hello\r{erase}").as_bytes());
        assert_eq!(flags(&t), ["-", "-"], "{erase:?}");
    }
    for edit in ["\x1b[X", "\x1b[P", "\x1b[@", "\x1b#8", "replace"] {
        let mut t = terminal(10, 2, 10);
        t.feed(format!("\x1b]133;A\x07hello\r{edit}").as_bytes());
        assert_eq!(flags(&t)[0], "P", "{edit:?}");
    }
}

#[test]
fn shell_marks_reflow_plain_styled_wide_and_continuation_rows() {
    for prefix in ["", "\x1b[31m"] {
        let mut t = terminal(5, 8, 100);
        t.feed(format!("{prefix}\x1b]133;A\x07abcdef\x1b]133;C\x07ghijZ").as_bytes());
        assert_eq!(&flags(&t)[..3], ["PW", "OW", "-"]);
        t.resize(TerminalSize { cols: 10, rows: 8 });
        assert_eq!(&flags(&t)[..2], ["OPW", "-"]);
        t.resize(TerminalSize { cols: 5, rows: 8 });
        assert_eq!(&flags(&t)[..3], ["OPW", "W", "-"]);
    }
    let mut t = terminal(5, 8, 100);
    t.feed("\x1b]133;A\x07abcd界\x1b]133;C\x07e".as_bytes());
    assert_eq!(&flags(&t)[..2], ["PW", "O"]);
    t.resize(TerminalSize { cols: 4, rows: 8 });
    assert_eq!(&flags(&t)[..2], ["PW", "O"]);
    t.resize(TerminalSize { cols: 10, rows: 8 });
    assert_eq!(flags(&t)[0], "OP");
}

#[test]
fn shell_marks_reflow_tracks_content_when_wide_cells_are_clamped() {
    let mut t = terminal(4, 20, 100);
    t.feed("\x1b]133;A\x07界abc\x1b]133;C\x07defgh".as_bytes());
    t.resize(TerminalSize { cols: 1, rows: 20 });
    let marks = flags(&t)
        .into_iter()
        .enumerate()
        .filter(|(_, label)| label.contains('P') || label.contains('O'))
        .collect::<Vec<_>>();
    assert_eq!(marks, [(0, "PW"), (3, "OW")]);
}

#[test]
fn shell_marks_on_blank_rows_survive_main_reflow_with_cursor_above() {
    for history_limit in [0, 100] {
        let mut unmarked = terminal(4, 8, history_limit);
        unmarked.feed(b"abcdefgh\x1b[H");
        unmarked.resize(TerminalSize { cols: 2, rows: 8 });
        assert_eq!(
            flags(&unmarked),
            ["W", "W", "W", "-", "-", "-", "-", "-"],
            "truly blank tail rows remain discardable with history-limit={history_limit}"
        );

        for (command, expected_mark) in [("A", "P"), ("C", "O")] {
            let mut t = terminal(4, 8, history_limit);
            t.feed(format!("abcdefgh\x1b[8;1H\x1b]133;{command}\x07\x1b[H").as_bytes());

            t.resize(TerminalSize { cols: 2, rows: 8 });
            assert_eq!(
                flags(&t),
                ["W", "W", "W", "-", "-", "-", "-", "-", "-", expected_mark],
                "marked blank row survives shrink with history-limit={history_limit}"
            );

            t.resize(TerminalSize { cols: 4, rows: 8 });
            assert_eq!(
                flags(&t),
                ["W", "-", "-", "-", "-", "-", "-", expected_mark],
                "marked blank row survives grow with history-limit={history_limit}"
            );
        }
    }
}

#[test]
fn shell_marks_on_blank_rows_match_alternate_reflow_retention() {
    for (command, expected_mark) in [("A", "P"), ("C", "O")] {
        let mut t = terminal(4, 8, 100);
        t.feed(format!("\x1b[?1049habcdefgh\x1b[8;1H\x1b]133;{command}\x07\x1b[H").as_bytes());

        t.resize(TerminalSize { cols: 2, rows: 8 });
        assert_eq!(
            flags(&t)
                .into_iter()
                .filter(|mark| *mark == expected_mark)
                .count(),
            1,
            "alternate shrink retains exactly one blank-row mark"
        );
        t.resize(TerminalSize { cols: 4, rows: 8 });
        assert_eq!(
            flags(&t)
                .into_iter()
                .filter(|mark| *mark == expected_mark)
                .count(),
            1,
            "alternate grow retains exactly one blank-row mark"
        );
    }
}

#[test]
fn shell_marks_alternate_reentry_resize_and_disabled_alternate() {
    for mode in [47, 1047, 1049] {
        let mut t = terminal(10, 5, 10);
        t.feed(b"\x1b]133;A\x07main\r\n\x1b]133;C\x07output");
        t.feed(format!("\x1b[?{mode}h\x1b[H\x1b]133;C\x07alt").as_bytes());
        assert_eq!(flags(&t)[0], "O");
        t.resize(TerminalSize { cols: 20, rows: 5 });
        t.feed(format!("\x1b[?{mode}l").as_bytes());
        assert_eq!(&flags(&t)[..2], ["P", "O"]);
        t.feed(format!("\x1b[?{mode}h\x1b[H").as_bytes());
        assert!(flags(&t).iter().all(|flag| *flag == "-"));
    }
    let mut t = terminal(10, 5, 10);
    t.set_alternate_screen_enabled(false);
    t.feed(b"\x1b]133;A\x07\x1b[?1049h\x1b]133;C\x07");
    assert!(!t.screen().is_alternate());
    assert_eq!(flags(&t)[0], "OP");
}

#[test]
fn shell_marks_reset_history_parser_and_synchronized_snapshot() {
    let mut t = terminal(10, 2, 10);
    t.feed(b"\x1b]133;A\x07one\r\n\x1b]133;C\x07two\r\nlast");
    t.feed(b"\x1b[!p");
    assert_eq!(flags(&t), ["P", "O", "-"]);
    t.feed(b"\x1b]133;");
    t.reset_parser();
    t.feed(b"A\x07");
    assert_eq!(flags(&t), ["P", "O", "-"]);
    // tmux 3.7b's RIS (`screen_write_reset`) stays on the alternate screen.
    // It blanks the alternate rows and their output mark, and the history
    // row keeps its prompt mark. Measured: alternate_on=1, history "one" P.
    t.feed(b"\x1b[?1049h\x1b]133;C\x07\x1bc");
    assert!(t.screen().is_alternate());
    assert_eq!(flags(&t), ["P", "-", "-"]);
    t.feed(b"\x1b[3J\x1b[?2026h\x1b]133;A\x07");
    assert!(t.screen().absolute_line_view(0).unwrap().start_prompt());
    assert!(!t
        .presentation_screen()
        .absolute_line_view(0)
        .unwrap()
        .start_prompt());
    t.feed(b"\x1b[?2026l");
    assert!(t
        .presentation_screen()
        .absolute_line_view(0)
        .unwrap()
        .start_prompt());
    // Leaving the alternate screen restores the saved principal rows and their
    // output mark. The history went with `CSI 3 J`. Measured on tmux 3.7b:
    // alternate_on=0, history_size=0, "two" O, "lastA" unmarked.
    t.feed(b"\x1b[?1049l");
    assert!(!t.screen().is_alternate());
    assert_eq!(t.screen().history_size(), 0);
    assert_eq!(flags(&t), ["O", "-"]);
}

#[test]
#[cfg(feature = "kitty-keyboard")]
fn shell_marks_preserve_dynamic_colours_clipboard_and_kitty() {
    let mut t = terminal(20, 3, 10);
    for b in b"\x1b]133;A\x07\x1b]10;rgb:1234/5678/9abc\x07\x1b]11;rgb:1111/2222/3333\x1b\\\x1b]12;rgb:aaaa/bbbb/cccc\x07\x1b]52;c;T0sxMzM=\x07\x1b[=8u\x1b[?u\x1b]133;C\x07\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]12;?\x07" {
        t.feed(&[*b]);
    }
    assert_eq!(flags(&t), ["OP", "-", "-"]);
    for (slot, value) in [
        (OscColourSlot::Foreground, "rgb:1234/5678/9abc"),
        (OscColourSlot::Background, "rgb:1111/2222/3333"),
        (OscColourSlot::Cursor, "rgb:aaaa/bbbb/cccc"),
    ] {
        assert_eq!(t.dynamic_colour(slot), Some(value));
    }
    assert_eq!(t.take_replies(), b"\x1b[?8u\x1b]10;rgb:1234/5678/9abc\x07\x1b]11;rgb:1111/2222/3333\x1b\\\x1b]12;rgb:aaaa/bbbb/cccc\x07");
    let events = t.take_terminal_passthrough();
    assert!(events
        .iter()
        .any(|event| event.payload() == b"\x1b]52;c;T0sxMzM=\x07"));
    assert_eq!(t.screen().keyboard_flags(), 8);
    t.feed(b"\x1b_Ga=q,i=1;AAAA\x1b\\\x1b]133;A\x07");
    let events = t.take_terminal_passthrough();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].kind(),
        rmux_core::TerminalPassthroughKind::KittyGraphics
    );
}
