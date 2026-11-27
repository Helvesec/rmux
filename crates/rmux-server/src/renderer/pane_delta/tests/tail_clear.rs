//! tmux tail-clearing and background regression cases.
use super::*;

#[test]
fn gh217_pane_snapshot_keyframe_and_delta_match_tmux_tail_clear_ladder() {
    let size = TerminalSize { cols: 20, rows: 2 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let full = screen_with_size(b"xxxxxxxxxxxxxxxxxxxx", size);
    let full =
        PaneRenderSnapshot::capture(session.view(), &options, pane, &full).expect("full snapshot");

    for tail in 1_usize..=12 {
        let content = "x".repeat(usize::from(size.cols) - tail);
        let screen = screen_with_size(content.as_bytes(), size);
        let snapshot =
            PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).expect("snapshot");
        let frame = snapshot.full_frame();
        let mut expected = content.into_bytes();
        if tail <= 9 {
            expected.extend(std::iter::repeat_n(b' ', tail));
        } else {
            expected.extend_from_slice(b"\x1b[K");
        }

        assert!(
            frame
                .windows(expected.len())
                .any(|window| window == expected.as_slice()),
            "keyframe tail {tail} must use the measured tmux ladder: {frame:?}"
        );

        let PaneRenderDelta::Incremental(delta) = full.diff_to(&snapshot) else {
            panic!("tail {tail} repaint should stay incremental");
        };
        assert!(
            delta
                .frame()
                .windows(expected.len())
                .any(|window| window == expected.as_slice()),
            "ordinary delta tail {tail} must use the measured tmux ladder: {:?}",
            delta.frame()
        );
    }

    let empty = Screen::new(size, 100);
    let snapshot = PaneRenderSnapshot::capture(session.view(), &options, pane, &empty)
        .expect("empty snapshot");
    let frame = snapshot.full_frame();
    assert!(
        frame
            .windows(b"\x1b[1;1H\x1b[0m\x1b[K".len())
            .any(|window| window == b"\x1b[1;1H\x1b[0m\x1b[K"),
        "an empty keyframe row must use EL: {frame:?}"
    );
    let PaneRenderDelta::Incremental(delta) = full.diff_to(&snapshot) else {
        panic!("empty-row repaint should stay incremental");
    };
    assert!(
        delta
            .frame()
            .windows(b"\x1b[1;1H\x1b[0m\x1b[K".len())
            .any(|window| window == b"\x1b[1;1H\x1b[0m\x1b[K"),
        "an empty ordinary delta row must use EL: {:?}",
        delta.frame()
    );
}

#[test]
fn gh217_pane_delta_styled_shrink_establishes_background_before_erase() {
    let size = TerminalSize { cols: 20, rows: 2 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let window = WindowTarget::with_window(session.name().clone(), 0);
    let pane = session.window().active_pane().expect("active pane");
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Window(window),
            OptionName::WindowActiveStyle,
            "bg=blue".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("window style set succeeds");
    let before = Screen::new(size, 100);
    let mut after = Screen::new(size, 100);
    let mut parser = InputParser::new();
    parser.parse(b"abc", &mut after);
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
        .expect("after snapshot");

    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("styled row repaint should stay incremental");
    };
    assert!(
        delta
            .frame()
            .windows(b"\x1b[44mabc\x1b[K".len())
            .any(|window| window == b"\x1b[44mabc\x1b[K"),
        "the styled delta must establish its background before EL: {:?}",
        delta.frame()
    );
}

#[test]
fn gh217_pane_delta_split_repaints_match_bounded_tmux_ladder() {
    let size = TerminalSize { cols: 41, rows: 2 };
    let mut session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    session.split_active_pane().expect("split pane");
    let pane = session.window().pane(0).expect("left pane");
    assert_eq!(pane.geometry().cols(), 20);
    let options = OptionStore::new();
    let before = screen_with_size(b"xxxxxxxxxxxxxxxxxxxx", size);
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &before)
        .expect("before snapshot");

    for tail in 1_usize..=12 {
        let content = "x".repeat(usize::from(pane.geometry().cols()) - tail);
        let after = screen_with_size(content.as_bytes(), size);
        let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &after)
            .expect("after snapshot");
        let mut expected = content.into_bytes();
        if tail <= 9 {
            expected.extend(std::iter::repeat_n(b' ', tail));
        } else {
            expected.extend_from_slice(format!("\x1b[{tail}X").as_bytes());
        }

        let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
            panic!("split tail {tail} repaint should stay incremental");
        };
        assert!(
            delta
                .frame()
                .windows(expected.len())
                .any(|window| window == expected.as_slice()),
            "split delta tail {tail} must use the bounded tmux ladder: {:?}",
            delta.frame()
        );
        assert!(
            !delta
                .frame()
                .windows(b"\x1b[K".len())
                .any(|window| window == b"\x1b[K"),
            "split delta tail {tail} must not erase the border or sibling: {:?}",
            delta.frame()
        );

        let keyframe = after.full_frame();
        assert!(
            keyframe
                .windows(expected.len())
                .any(|window| window == expected.as_slice()),
            "split keyframe tail {tail} must use the bounded tmux ladder: {keyframe:?}"
        );
    }

    let empty = Screen::new(size, 100);
    let empty = PaneRenderSnapshot::capture(session.view(), &options, pane, &empty)
        .expect("empty split snapshot");
    let empty_keyframe = empty.full_frame();
    assert!(
        empty_keyframe
            .windows(b"\x1b[1;20H\x1b[1K".len())
            .any(|window| window == b"\x1b[1;20H\x1b[1K"),
        "empty split keyframe must use EL1: {empty_keyframe:?}"
    );
    let PaneRenderDelta::Incremental(delta) = before.diff_to(&empty) else {
        panic!("empty split repaint should stay incremental");
    };
    assert!(
        delta
            .frame()
            .windows(b"\x1b[1;20H\x1b[1K".len())
            .any(|window| window == b"\x1b[1;20H\x1b[1K"),
        "empty split delta must use bounded EL1: {:?}",
        delta.frame()
    );
}

#[test]
fn gh217_pane_delta_scroll_repaint_matches_ladder_and_preserves_style() {
    let size = TerminalSize { cols: 20, rows: 4 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let window = WindowTarget::with_window(session.name().clone(), 0);
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();

    for tail in 1_usize..=12 {
        let mut screen = Screen::new(TerminalSize { cols: 20, rows: 3 }, 100);
        let mut parser = InputParser::new();
        parser.parse(b"one\r\ntwo\r\nthree", &mut screen);
        let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &screen)
            .expect("before snapshot");
        let content = "x".repeat(usize::from(size.cols) - tail);
        parser.parse(format!("\r\n{content}").as_bytes(), &mut screen);
        let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &screen)
            .expect("after snapshot");
        let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
            panic!("scroll tail {tail} should stay incremental");
        };
        let mut expected = content.into_bytes();
        if tail <= 9 {
            expected.extend(std::iter::repeat_n(b' ', tail));
        } else {
            expected.extend_from_slice(b"\x1b[K");
        }
        assert!(
            delta
                .frame()
                .windows(expected.len())
                .any(|window| window == expected.as_slice()),
            "scroll-exposed tail {tail} must use the measured tmux ladder: {:?}",
            delta.frame()
        );
    }

    let mut empty_screen = Screen::new(TerminalSize { cols: 20, rows: 3 }, 100);
    let mut parser = InputParser::new();
    parser.parse(b"one\r\ntwo\r\nthree", &mut empty_screen);
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &empty_screen)
        .expect("before empty scroll snapshot");
    parser.parse(b"\r\n", &mut empty_screen);
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &empty_screen)
        .expect("after empty scroll snapshot");
    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("empty scroll row should stay incremental");
    };
    assert!(
        delta
            .frame()
            .windows(b"\x1b[3;1H\x1b[0m\x1b[K".len())
            .any(|window| window == b"\x1b[3;1H\x1b[0m\x1b[K"),
        "an empty scroll-exposed row must use EL: {:?}",
        delta.frame()
    );

    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Window(window),
            OptionName::WindowActiveStyle,
            "bg=blue".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("window style set succeeds");
    let mut screen = Screen::new(TerminalSize { cols: 20, rows: 3 }, 100);
    let mut parser = InputParser::new();
    parser.parse(b"one\r\ntwo\r\nthree", &mut screen);
    let before = PaneRenderSnapshot::capture(session.view(), &options, pane, &screen)
        .expect("before snapshot");

    parser.parse(b"\r\nfour", &mut screen);
    let after = PaneRenderSnapshot::capture(session.view(), &options, pane, &screen)
        .expect("after snapshot");
    let PaneRenderDelta::Incremental(delta) = before.diff_to(&after) else {
        panic!("scroll repaint should stay incremental");
    };

    assert!(
        delta
            .frame()
            .windows(b"\x1b[44mfour\x1b[K".len())
            .any(|window| window == b"\x1b[44mfour\x1b[K"),
        "the exposed scroll row must establish background before EL: {:?}",
        delta.frame()
    );
}

#[test]
fn gh217_pane_snapshot_and_delta_clear_application_authored_trailing_spaces() {
    let size = TerminalSize { cols: 20, rows: 2 };
    let session = crate::session_fixture::SessionFixture::new(session_name("alpha"), size);
    let pane = session.window().active_pane().expect("active pane");
    let options = OptionStore::new();
    let mut screen = Screen::new(size, 100);
    let mut parser = InputParser::new();
    parser.parse(b"A       ", &mut screen);
    let snapshot =
        PaneRenderSnapshot::capture(session.view(), &options, pane, &screen).expect("snapshot");
    let frame = snapshot.full_frame();

    assert!(
        frame
            .windows(b"A\x1b[K".len())
            .any(|window| window == b"A\x1b[K"),
        "keyframes must clear matching application spaces as the tmux empty tail: {frame:?}"
    );
    let full = PaneRenderSnapshot::capture(
        session.view(),
        &options,
        pane,
        &screen_with_size(b"xxxxxxxxxxxxxxxxxxxx", size),
    )
    .expect("full snapshot");
    let PaneRenderDelta::Incremental(delta) = full.diff_to(&snapshot) else {
        panic!("application-space repaint should stay incremental");
    };
    assert!(
        delta
            .frame()
            .windows(b"A\x1b[K".len())
            .any(|window| window == b"A\x1b[K"),
        "ordinary deltas must clear matching application spaces with EL: {:?}",
        delta.frame()
    );

    let split_size = TerminalSize { cols: 41, rows: 2 };
    let mut split_session =
        crate::session_fixture::SessionFixture::new(session_name("split"), split_size);
    split_session.split_active_pane().expect("split pane");
    let split_pane = split_session.window().pane(0).expect("left pane");
    let mut split_screen = Screen::new(split_size, 100);
    parser.parse(b"A       ", &mut split_screen);
    let split_snapshot = PaneRenderSnapshot::capture(
        split_session.view(),
        &OptionStore::new(),
        split_pane,
        &split_screen,
    )
    .expect("split snapshot");
    let split_frame = split_snapshot.full_frame();
    assert!(
        split_frame
            .windows(b"A\x1b[19X".len())
            .any(|window| window == b"A\x1b[19X"),
        "split keyframes must clear matching application spaces with bounded ECH: {split_frame:?}"
    );
    let split_full = PaneRenderSnapshot::capture(
        split_session.view(),
        &OptionStore::new(),
        split_pane,
        &screen_with_size(b"xxxxxxxxxxxxxxxxxxxx", split_size),
    )
    .expect("full split snapshot");
    let PaneRenderDelta::Incremental(delta) = split_full.diff_to(&split_snapshot) else {
        panic!("split application-space repaint should stay incremental");
    };
    assert!(
        delta
            .frame()
            .windows(b"A\x1b[19X".len())
            .any(|window| window == b"A\x1b[19X"),
        "split deltas must clear matching application spaces with bounded ECH: {:?}",
        delta.frame()
    );
}
