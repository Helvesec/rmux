//! Plain output forwarding and positioned echo.
use super::*;

#[test]
fn pane_snapshot_forwarded_plain_bytes_bumps_revision_once_per_line() {
    let mut snapshot = PaneRenderSnapshot {
        x: 0,
        y: 0,
        rows: 2,
        cols: 10,
        terminal_cols: 10,
        terminal_rows: 2,
        source_row_start: 0,
        source_width: 10,
        row_spans: rect_spans(0, 0, 2, 10),
        utf8: rmux_core::Utf8Config::default(),
        lines: render_lines(&[b"", b""]),
        cursor: b"\x1b[1;1H".to_vec(),
        cursor_row: 0,
        cursor_col: 0,
        cursor_style: 0.into(),
        title: String::new(),
        path: String::new(),
        mode: 0,
        line_revisions: vec![7, 11],
        scrollbar: None,
    };

    assert!(snapshot.apply_forwarded_plain_bytes(b"abc\r\ndef"));

    assert_eq!(snapshot.line_revisions, vec![8, 12]);
}

#[test]
fn pane_snapshot_raw_forwarding_rejects_plain_bottom_scroll_with_status_line() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let screen = screen_with(b"\x1b[3;1H");
    let snapshot =
        PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).expect("snapshot");

    assert!(!snapshot.can_forward_plain_bytes(b"x\r\n"));
    assert!(snapshot.can_forward_plain_bytes(b"x"));
    assert!(!snapshot.can_forward_plain_bytes(b"\x1b[31m"));
}

#[test]
fn pane_snapshot_raw_forwarding_allows_plain_bottom_scroll_without_status_line() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 3 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::Status,
            "off".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("status option can be disabled");
    let screen = screen_with(b"\x1b[3;1H");
    let snapshot =
        PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).expect("snapshot");

    assert!(snapshot.can_forward_plain_bytes(b"x\r\n"));
    assert!(snapshot.can_forward_plain_bytes(b"x"));
    assert!(!snapshot.can_forward_plain_bytes(b"\x1b[31m"));
}

#[test]
fn pane_snapshot_builds_positioned_echo_for_split_panes() {
    let snapshot = PaneRenderSnapshot {
        x: 10,
        y: 2,
        rows: 3,
        cols: 20,
        terminal_cols: 80,
        terminal_rows: 24,
        source_row_start: 0,
        source_width: 20,
        row_spans: rect_spans(10, 2, 3, 20),
        utf8: rmux_core::Utf8Config::default(),
        lines: render_lines(&[b"prompt>             ", b"", b""]),
        cursor: b"\x1b[3;18H".to_vec(),
        cursor_row: 2,
        cursor_col: 17,
        cursor_style: 0.into(),
        title: String::new(),
        path: String::new(),
        mode: 0,
        line_revisions: vec![1, 0, 0],
        scrollbar: None,
    };

    let frame = snapshot
        .positioned_plain_echo_frame(b"abc")
        .expect("split pane echo can be positioned");

    assert_eq!(frame, b"\x1b[3;18Habc\x1b[3;21H");
}

#[test]
fn pane_snapshot_rejects_positioned_echo_on_styled_or_wrapping_input() {
    let snapshot = PaneRenderSnapshot {
        x: 10,
        y: 2,
        rows: 1,
        cols: 4,
        terminal_cols: 80,
        terminal_rows: 24,
        source_row_start: 0,
        source_width: 4,
        row_spans: rect_spans(10, 2, 1, 4),
        utf8: rmux_core::Utf8Config::default(),
        lines: render_lines(&[b"\x1b[31mred"]),
        cursor: b"\x1b[3;14H".to_vec(),
        cursor_row: 2,
        cursor_col: 13,
        cursor_style: 0.into(),
        title: String::new(),
        path: String::new(),
        mode: 0,
        line_revisions: vec![1],
        scrollbar: None,
    };

    assert!(snapshot.positioned_plain_echo_frame(b"x").is_none());
    assert!(PaneRenderSnapshot {
        lines: render_lines(&[b"    "]),
        ..snapshot
    }
    .positioned_plain_echo_frame(b"xy")
    .is_none());
}

#[test]
fn pane_snapshot_applies_forwarded_plain_bytes_without_recapture() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let screen = screen_with(b"");
    let mut snapshot =
        PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).expect("snapshot");

    assert!(snapshot.apply_forwarded_plain_bytes(b"abc\r\n"));

    assert_eq!(&snapshot.lines[0].bytes()[..3], b"abc");
    assert_eq!(snapshot.cursor_row, 1);
    assert_eq!(snapshot.cursor_col, 0);
    assert_eq!(snapshot.cursor, b"\x1b[2;1H");
}

#[test]
fn pane_snapshot_applies_positioned_echo_for_split_panes() {
    let mut snapshot = PaneRenderSnapshot {
        x: 10,
        y: 2,
        rows: 3,
        cols: 20,
        terminal_cols: 80,
        terminal_rows: 24,
        source_row_start: 0,
        source_width: 20,
        row_spans: rect_spans(10, 2, 3, 20),
        utf8: rmux_core::Utf8Config::default(),
        lines: render_lines(&[b"prompt>             ", b"", b""]),
        cursor: b"\x1b[3;18H".to_vec(),
        cursor_row: 2,
        cursor_col: 17,
        cursor_style: 0.into(),
        title: String::new(),
        path: String::new(),
        mode: 0,
        line_revisions: vec![1, 0, 0],
        scrollbar: None,
    };

    assert!(!snapshot.can_forward_plain_bytes(b"abc"));
    assert!(snapshot.apply_forwarded_plain_bytes(b"abc"));

    assert_eq!(&snapshot.lines[0].bytes()[7..10], b"abc");
    assert_eq!(snapshot.cursor_col, 20);
    assert_eq!(snapshot.cursor, b"\x1b[3;21H");
}

#[test]
fn pane_snapshot_plain_forwarding_handles_bottom_scroll() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 3 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::Status,
            "off".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("status option can be disabled");
    let mut screen = Screen::new(TerminalSize { cols: 10, rows: 3 }, 100);
    let mut parser = InputParser::new();
    parser.parse(b"one\r\ntwo\r\nthree", &mut screen);
    let mut forwarded =
        PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).expect("snapshot");
    let before_revisions = forwarded.line_revisions.clone();

    assert!(forwarded.can_forward_plain_bytes(b"\r\nfour"));
    assert!(forwarded.apply_forwarded_plain_bytes(b"\r\nfour"));

    parser.parse(b"\r\nfour", &mut screen);
    let recaptured =
        PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).expect("snapshot");

    assert_eq!(forwarded.lines, recaptured.lines);
    assert_eq!(forwarded.cursor, recaptured.cursor);
    assert_eq!(forwarded.cursor_row, recaptured.cursor_row);
    assert_eq!(forwarded.cursor_col, recaptured.cursor_col);
    assert_eq!(
        &forwarded.line_revisions[..2],
        &recaptured.line_revisions[..2],
        "scrolled rows must retain their original line identities"
    );
    assert_ne!(
        forwarded.line_revisions[2] & super::super::LINE_REVISION_ID_MASK,
        before_revisions[2] & super::super::LINE_REVISION_ID_MASK,
        "bottom scroll must allocate a fresh line identity"
    );
    assert_eq!(
        forwarded.line_revisions[2] & super::super::LINE_REVISION_GENERATION_MASK,
        1,
        "writing to the fresh bottom line bumps its generation once"
    );
}

#[test]
fn pane_snapshot_positioned_plain_output_rejects_split_pane_scroll() {
    let mut session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 20, rows: 4 },
    );
    session
        .split_active_pane_with_direction(SplitDirection::Vertical)
        .expect("split pane");
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let mut screen = Screen::new(TerminalSize { cols: 10, rows: 3 }, 100);
    let mut parser = InputParser::new();
    parser.parse(b"one\r\ntwo\r\nthree", &mut screen);
    let mut forwarded =
        PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).expect("snapshot");

    assert!(
        forwarded
            .positioned_plain_output_frame(b"\r\nfour")
            .is_none(),
        "split-pane scroll must not emit DECSTBM/SU because that scrolls all terminal columns"
    );
    assert_eq!(
        forwarded,
        PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).expect("snapshot"),
        "rejected positioned plain output must not mutate the reusable snapshot"
    );
}
