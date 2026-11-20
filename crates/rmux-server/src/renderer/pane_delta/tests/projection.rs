//! Clipped geometry, scrollbar and styled capture.
use super::*;

#[test]
fn pane_delta_repaints_scrollbar_when_history_changes() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane");
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Window(WindowTarget::with_window(session.name().clone(), 0)),
            OptionName::PaneScrollbars,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("scrollbar mode");
    let before = screen_with(b"one\r\ntwo\r\nthree");
    let after = screen_with(b"one\r\ntwo\r\nthree\r\nfour");
    assert_eq!(before.history_size(), 0);
    assert_eq!(after.history_size(), 1);
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("history update should stay incremental");
    };

    assert!(delta
        .frame()
        .windows(b"\x1b[1;10H\x1b[0;37;40m ".len())
        .any(|window| window == b"\x1b[1;10H\x1b[0;37;40m "));
    assert!(delta
        .frame()
        .windows(b"\x1b[2;10H\x1b[0;30;47m ".len())
        .any(|window| window == b"\x1b[2;10H\x1b[0;30;47m "));
}

#[test]
fn pane_delta_live_capture_applies_window_style_without_full_screen_clone() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let window = WindowTarget::with_window(session.name().clone(), 0);
    let pane = session.window().active_pane().expect("active pane");
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Window(window),
            OptionName::WindowActiveStyle,
            "bg=red".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("window style set succeeds");
    let mut transcript = PaneTranscript::new(100, TerminalSize { cols: 10, rows: 3 });
    transcript.append_bytes(b"one\r\ntwo\r\nthree");

    let snapshot = PaneRenderSnapshot::capture_unstyled_transcript_reusing(
        session.view(),
        &options,
        pane,
        &transcript,
        None,
    )
    .expect("styled live snapshot should not require full screen clone");

    let line = String::from_utf8(snapshot.lines[0].bytes().to_vec()).expect("line is utf8");
    assert!(line.contains("\u{1b}[41mone"), "{line:?}");
}

#[test]
fn pane_delta_projects_signed_floating_source_origin() {
    let mut session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    session.split_active_pane().expect("split succeeds");
    float_pane(
        &mut session,
        1,
        FloatingPaneRect::new(-3, 0, 8, 2).expect("valid floating rect"),
    );
    let pane = session.window().pane(1).expect("floating pane");
    let options = OptionStore::new();
    let before = screen_with_size(b"ABCDEFGH", TerminalSize { cols: 8, rows: 2 });
    let after = screen_with_size(b"ABCxyFGH", TerminalSize { cols: 8, rows: 2 });
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let keyframe = String::from_utf8(after.full_frame()).expect("keyframe is utf-8");
    assert!(
        keyframe.contains("\u{1b}[1;1H\u{1b}[0mxyFGH"),
        "{keyframe:?}"
    );
    assert!(
        !keyframe.contains("ABCxyFGH"),
        "off-screen source cells must not be repainted: {keyframe:?}"
    );

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("same projection line repaint should stay incremental");
    };
    let frame = String::from_utf8(delta.frame().to_vec()).expect("delta is utf-8");
    assert!(frame.contains("\u{1b}[1;1H\u{1b}[0mxyFGH"), "{frame:?}");
    assert!(
        !frame.contains("ABCxyFGH"),
        "delta must use the projected source start: {frame:?}"
    );
}

#[test]
fn projected_cell_full_delta_and_keyframe_keep_source_cells() {
    for (origin, text, expected) in [(1, "表ABC", " AB"), (2, "👩‍💻ABC", "ABC")] {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 10, rows: 4 },
        );
        session.split_active_pane().unwrap();
        float_pane(
            &mut session,
            1,
            FloatingPaneRect::new(-origin, 0, 8, 2).unwrap(),
        );
        let pane = session.window().pane(1).unwrap();
        let options = OptionStore::new();
        let size = TerminalSize { cols: 8, rows: 2 };
        let blank = screen_with_size(b"xxxxxxxx", size);
        let screen = screen_with_size(text.as_bytes(), size);
        let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &blank).unwrap();
        let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).unwrap();
        let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
            panic!("unchanged projection must remain incremental");
        };
        for frame in [
            crate::renderer::render_pane_screen(session.view(), &options, pane, &screen),
            after.full_frame(),
            delta.frame().to_vec(),
        ] {
            let frame = String::from_utf8(frame).unwrap();
            assert!(
                frame.contains(&format!("\x1b[1;1H\x1b[0m{expected}")),
                "{frame:?}"
            );
            assert!(!frame.contains('表') && !frame.contains('💻'), "{frame:?}");
        }
    }
}
