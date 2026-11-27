//! Cursor style and visibility transitions.
use super::*;

#[test]
fn pane_delta_does_not_scroll_empty_zero_revision_rows_for_cursor_move() {
    let before = PaneRenderSnapshot {
        x: 0,
        y: 0,
        rows: 3,
        cols: 10,
        terminal_cols: 10,
        terminal_rows: 3,
        source_row_start: 0,
        source_width: 10,
        row_spans: rect_spans(0, 0, 3, 10),
        utf8: rmux_core::Utf8Config::default(),
        lines: render_lines(&[b"", b"", b""]),
        cursor: b"\x1b[1;1H".to_vec(),
        cursor_row: 0,
        cursor_col: 0,
        cursor_style: 0.into(),
        title: String::new(),
        path: String::new(),
        mode: 0,
        line_revisions: vec![0, 0, 0],
        scrollbar: None,
    };
    let after = PaneRenderSnapshot {
        cursor: b"\x1b[1;2H".to_vec(),
        cursor_col: 1,
        ..before.clone()
    };

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("cursor move should stay incremental");
    };
    let text = String::from_utf8(delta.frame().to_vec()).expect("delta is utf8");

    assert!(
        !text.contains('S'),
        "empty rows must not use scroll: {text:?}"
    );
    assert_eq!(delta.frame(), b"\x1b[1;2H");
}

#[test]
fn pane_delta_publishes_blink_only_cursor_style_change() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let before = screen_with(b"\x1b[6 q");
    let after = screen_with(b"\x1b[6 q\x1b[?12h");
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("cursor style-only update should stay incremental");
    };
    assert_eq!(delta.cursor_style(), Some(5));
}

#[test]
fn pane_delta_resolves_blink_against_inherited_cursor_default() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Window(WindowTarget::with_window(session.name().clone(), 0)),
            OptionName::CursorStyle,
            "bar".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("window cursor style");
    let before = screen_with(b"");
    let after = screen_with(b"\x1b[?12h");
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("configured cursor style-only update should stay incremental");
    };
    assert_eq!(delta.cursor_style(), Some(5));
}

#[test]
fn pane_delta_publishes_blink_for_terminal_default_shape() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let before = screen_with(b"");
    let after = screen_with(b"\x1b[0 q\x1b[?12h");
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("default cursor blink-only update should stay incremental");
    };
    assert_eq!(delta.cursor_style(), Some(0));
    assert_eq!(
        delta
            .cursor_style_state()
            .and_then(rmux_core::ResolvedCursorStyle::default_blink),
        Some(true)
    );
}

#[test]
fn pane_delta_ignores_invalid_cursor_style_without_output() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let before = screen_with(b"\x1b[3 q");
    let after = screen_with(b"\x1b[3 q\x1b[99 q");
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("invalid cursor style must not require a refresh");
    };
    assert!(delta.frame().is_empty());
    assert_eq!(delta.cursor_style(), None);
}

#[test]
fn pane_snapshot_full_frame_restores_hidden_cursor_state() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let screen = screen_with(b"\x1b[?25labc");
    let snapshot =
        PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).expect("snapshot");

    let text = String::from_utf8(snapshot.full_frame()).expect("frame is utf8");

    assert!(text.starts_with("\u{1b}[s\u{1b}[?25l\u{1b}[0m"));
    assert!(text.ends_with("\u{1b}[1;4H\u{1b}[?25l"));
}

#[test]
fn pane_delta_emits_cursor_visibility_only_change() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let before = screen_with(b"abc");
    let after = screen_with(b"abc\x1b[?25l");
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("cursor visibility change should stay incremental");
    };

    assert_eq!(delta.frame(), b"\x1b[1;4H\x1b[?25l");
}
