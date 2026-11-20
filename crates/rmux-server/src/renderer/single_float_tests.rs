use super::*;

fn options(position: &str) -> OptionStore {
    let mut options = OptionStore::new();
    for (name, value) in [
        (OptionName::Status, "off"),
        (OptionName::PaneBorderStatus, position),
        (OptionName::PaneBorderFormat, "FLOAT"),
    ] {
        options
            .set(
                ScopeSelector::Global,
                name,
                value.to_owned(),
                SetOptionMode::Replace,
            )
            .expect("option set");
    }
    options
}

#[test]
fn last_tiled_pane_float_renders_frame_and_status() {
    for position in ["top", "bottom"] {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 20, rows: 10 },
        );
        let options = options(position);
        let tiled = String::from_utf8(render(session.view(), &options)).unwrap();
        assert!(tiled.is_empty(), "{tiled:?}");
        float_pane(&mut session, 0, FloatingPaneRect::new(2, 2, 10, 3).unwrap());
        assert_eq!(session.window().pane_count(), 1);
        let frame = String::from_utf8(render(session.view(), &options)).unwrap();
        assert!(
            frame.contains('┌') && frame.contains('┘'),
            "{position}: {frame:?}"
        );
        assert!(frame.contains("FLOAT"), "{position}: {frame:?}");
        let screen = screen_with(frame.as_bytes(), TerminalSize { cols: 20, rows: 10 });
        let status_row = if position == "top" { 1 } else { 5 };
        assert!(visible_line_text(&screen, status_row, 20).contains("FLOAT"));

        session
            .resize_pane(0, ResizePaneAdjustment::Zoom)
            .expect("zoom succeeds");
        let zoomed = String::from_utf8(render(session.view(), &options)).unwrap();
        assert!(
            !session.window().is_zoomed(),
            "single-pane zoom remains a no-op"
        );
        assert_eq!(zoomed, frame);
    }
}

#[test]
fn single_float_frame_and_status_clip_at_left_edge() {
    for position in ["top", "bottom"] {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 20, rows: 10 },
        );
        float_pane(
            &mut session,
            0,
            FloatingPaneRect::new(-3, 2, 10, 3).unwrap(),
        );
        let frame = String::from_utf8(render(session.view(), &options(position))).unwrap();
        assert!(!frame.contains('┌') && !frame.contains('└'), "{frame:?}");
        assert!(frame.contains('┐') && frame.contains('┘'), "{frame:?}");
        assert!(
            frame.contains("LOAT") && !frame.contains("FLOAT"),
            "{frame:?}"
        );
    }
}
