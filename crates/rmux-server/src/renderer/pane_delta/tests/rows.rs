//! Incremental row repaint, scrolling and full frames.
use super::*;

#[test]
fn pane_delta_renders_only_changed_lines_and_cursor() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let before = screen_with(b"abc");
    let after = screen_with(b"abcd");
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("line update should not require a full refresh");
    };
    let text = String::from_utf8(delta.frame().to_vec()).expect("delta is utf8");

    assert_eq!(text, "d");
}

#[test]
fn pane_delta_keeps_styled_zwj_text_at_right_edge() {
    let size = TerminalSize { cols: 5, rows: 3 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let before = Screen::new(size, 100);
    let mut after = Screen::new(size, 100);
    let mut parser = InputParser::new();
    parser.parse("\x1b[31m👩\u{200d}💻ABC".as_bytes(), &mut after);
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("composed-cell repaint should stay incremental");
    };
    let frame = String::from_utf8(delta.frame().to_vec()).expect("delta frame is utf-8");

    assert!(
        frame.contains("\u{1b}[31m👩\u{200d}💻ABC"),
        "delta repaint must preserve styled ZWJ cells and following text: {frame:?}"
    );
}

#[test]
fn pane_delta_trims_plain_ascii_padding_for_short_line_growth() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let before = screen_with(b"");
    let after = screen_with(b"abc");
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("short ASCII output should stay incremental");
    };
    let text = String::from_utf8(delta.frame().to_vec()).expect("delta is utf8");

    assert_eq!(text, "abc");
    assert!(
        !text.contains("abc       "),
        "plain ASCII append should not repaint trailing padding: {text:?}"
    );
    assert!(!text.contains("\u{1b}[K"), "{text:?}");
}

#[test]
fn pane_delta_uses_plain_ascii_cursor_append_for_line_advance() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let before = screen_with(b"");
    let after = screen_with(b"abc\r\n");
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("line advance should stay incremental");
    };

    assert_eq!(delta.frame(), b"abc\r\n");
}

#[test]
fn pane_delta_uses_spaces_for_small_full_width_shrink_tail() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let before = screen_with(b"abcdefghij");
    let after = screen_with(b"abcdefghij\r\x1b[Kabc");
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("line shrink should stay incremental");
    };
    let text = String::from_utf8(delta.frame().to_vec()).expect("delta is utf8");

    assert!(text.contains("\u{1b}[1;1H\u{1b}[0mabc       "), "{text:?}");
    assert!(
        !text.contains("abc\u{1b}[K"),
        "tmux keeps literal spaces for nonempty tails below ten columns: {text:?}"
    );
}

#[test]
fn pane_delta_keeps_padding_for_plain_ascii_shrink_inside_split_pane() {
    let before = PaneRenderSnapshot {
        x: 2,
        y: 0,
        rows: 1,
        cols: 5,
        terminal_cols: 10,
        terminal_rows: 1,
        source_row_start: 0,
        source_width: 5,
        row_spans: rect_spans(2, 0, 1, 5),
        utf8: rmux_core::Utf8Config::default(),
        lines: render_lines(&[b"abcde"]),
        cursor: b"\x1b[1;6H".to_vec(),
        cursor_row: 0,
        cursor_col: 5,
        cursor_style: 0.into(),
        title: String::new(),
        path: String::new(),
        mode: 0,
        line_revisions: vec![1],
        scrollbar: None,
    };
    let after = PaneRenderSnapshot {
        lines: render_lines(&[b"ab   "]),
        cursor: b"\x1b[1;4H".to_vec(),
        cursor_col: 3,
        line_revisions: vec![2],
        ..before.clone()
    };

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("split pane shrink should stay incremental");
    };
    let text = String::from_utf8(delta.frame().to_vec()).expect("delta is utf8");

    assert!(text.contains("\u{1b}[1;3H\u{1b}[0mab   "), "{text:?}");
    assert!(
        !text.contains("\u{1b}[K"),
        "split pane shrink must not clear outside pane bounds: {text:?}"
    );
}

#[test]
fn pane_delta_keeps_title_changes_incremental() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let before = screen_with(b"abc");
    let mut after = screen_with(b"abc");
    after.set_title("new title");
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    assert_eq!(
        before.diff_to(&after),
        PaneRenderDelta::Incremental(super::super::PaneRenderDeltaFrame {
            frame: Vec::new(),
            cursor_style: None,
        })
    );
}

#[test]
fn pane_delta_renders_new_prompt_lines_incrementally() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let before = screen_with(b"abc");
    let after = screen_with(b"abc\r\ndef");
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("new shell prompt lines should not force a full refresh");
    };
    let text = String::from_utf8(delta.frame().to_vec()).expect("delta is utf8");

    assert!(text.contains("\u{1b}[2;1H"));
    assert!(text.contains("def"));
    assert!(text.ends_with("\u{1b}[2;4H\u{1b}[?25h"));
}

#[test]
fn pane_delta_scrolls_full_width_panes_instead_of_repainting_shifted_rows() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let mut screen = Screen::new(TerminalSize { cols: 10, rows: 3 }, 100);
    let mut parser = InputParser::new();
    parser.parse(b"one\r\ntwo\r\nthree", &mut screen);
    let before =
        PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).expect("snapshot");

    parser.parse(b"\r\nfour", &mut screen);
    let after =
        PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).expect("snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("scrolling a full-width pane should stay incremental");
    };
    let text = String::from_utf8(delta.frame().to_vec()).expect("delta is utf8");

    assert!(text.contains("\u{1b}[1;3r"), "{text:?}");
    assert!(text.contains("\u{1b}[1S"), "{text:?}");
    assert!(text.contains("\u{1b}[3;1H\u{1b}[0mfour"), "{text:?}");
    assert!(
        !text.contains("\u{1b}[1;1H\u{1b}[0mtwo"),
        "shifted rows should be moved by terminal scroll, not repainted: {text:?}"
    );
}

#[test]
fn pane_delta_reuses_rendered_lines_shifted_by_scroll() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let mut transcript = PaneTranscript::new(100, TerminalSize { cols: 10, rows: 3 });
    transcript.append_bytes(b"one\r\ntwo\r\nthree");
    let before = PaneRenderSnapshot::capture_unstyled_transcript_reusing(
        session.view(),
        &options,
        pane,
        &transcript,
        None,
    )
    .expect("before snapshot");

    transcript.append_bytes(b"\r\nfour");
    let after = PaneRenderSnapshot::capture_unstyled_transcript_reusing(
        session.view(),
        &options,
        pane,
        &transcript,
        Some(&before),
    )
    .expect("after snapshot");

    assert_eq!(after.lines[0], before.lines[1]);
    assert_eq!(after.lines[1], before.lines[2]);
    assert!(Arc::ptr_eq(&after.lines[0], &before.lines[1]));
    assert!(Arc::ptr_eq(&after.lines[1], &before.lines[2]));
    assert!(String::from_utf8(after.lines[2].bytes().to_vec())
        .expect("line is utf8")
        .contains("four"));

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("scrolling a full-width pane should stay incremental");
    };
    let text = String::from_utf8(delta.frame().to_vec()).expect("delta is utf8");
    assert!(text.contains("\u{1b}[1;3r"), "{text:?}");
    assert!(
        !text.contains("\u{1b}[1;1H\u{1b}[0mtwo"),
        "shifted rows should be moved by terminal scroll, not repainted: {text:?}"
    );
}

#[test]
fn pane_snapshot_full_frame_repaints_all_visible_lines() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let screen = screen_with(b"abc\r\ndef");
    let snapshot =
        PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).expect("snapshot");

    let text = String::from_utf8(snapshot.full_frame()).expect("frame is utf8");

    assert!(text.contains("\u{1b}[1;1H\u{1b}[0mabc"));
    assert!(text.contains("\u{1b}[2;1H\u{1b}[0mdef"));
    assert!(text.starts_with("\u{1b}[s\u{1b}[?25l\u{1b}[0m"));
    assert!(text.ends_with("\u{1b}[2;4H\u{1b}[?25h"));
}

#[test]
fn pane_snapshot_full_frame_resets_before_default_row_after_styled_full_width_row() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let screen = screen_with(b"\x1b[48;5;255m          \r\n\x1b[0mplain");
    let snapshot =
        PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).expect("snapshot");

    let text = String::from_utf8(snapshot.full_frame()).expect("frame is utf8");

    assert!(
        text.contains("\u{1b}[1;1H\u{1b}[0m\u{1b}[48;5;255m          "),
        "{text:?}"
    );
    assert!(
        text.contains("\u{1b}[2;1H\u{1b}[0mplain"),
        "default rows must not inherit the previous row's background: {text:?}"
    );
}

#[test]
fn pane_delta_repaints_blank_lines_when_background_changes() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let before = screen_with(b"alpha\r\n          ");
    let after = screen_with(b"\x1b[48;2;20;20;20malpha\r\n          ");
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("style-only row changes should repaint incrementally");
    };
    let text = String::from_utf8(delta.frame().to_vec()).expect("delta is utf8");

    assert!(text.contains("\u{1b}[1;1H"));
    assert!(text.contains("\u{1b}[48;2;20;20;20m"));
    assert!(text.contains("\u{1b}[2;1H"));
}

#[test]
fn pane_delta_resets_each_repainted_row_independently() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let before = screen_with(b"0123456789\r\nbase");
    let after = screen_with(b"\x1b[48;2;20;20;20mabcdefghij\r\n\x1b[0mnext");
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("changed rows should repaint incrementally");
    };
    let text = String::from_utf8(delta.frame().to_vec()).expect("delta is utf8");

    assert!(
        text.contains("\u{1b}[1;1H\u{1b}[0m\u{1b}[48;2;20;20;20mabcdefghij"),
        "{text:?}"
    );
    assert!(
        text.contains("\u{1b}[2;1H\u{1b}[0mnext"),
        "the second row must not inherit row one's background: {text:?}"
    );
}
