use rmux_core::OptionStore;
use rmux_proto::{
    OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize, WindowTarget,
};

use crate::pane_transcript::{PaneTranscript, SharedPaneTranscript};
use crate::renderer::PaneRenderDelta;

use super::LivePaneRender;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

#[derive(Clone, Copy)]
enum PlainForwardingFixtureState {
    Safe,
    PendingWrap,
    InsertMode,
    SgrStyle,
    Osc8Hyperlink,
    ScrollMargins,
}

impl PlainForwardingFixtureState {
    const UNSAFE: [Self; 5] = [
        Self::PendingWrap,
        Self::InsertMode,
        Self::SgrStyle,
        Self::Osc8Hyperlink,
        Self::ScrollMargins,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::PendingWrap => "pending-wrap",
            Self::InsertMode => "IRM",
            Self::SgrStyle => "SGR style",
            Self::Osc8Hyperlink => "OSC 8 hyperlink",
            Self::ScrollMargins => "DECSTBM margins",
        }
    }

    fn bytes(self, size: TerminalSize) -> Vec<u8> {
        match self {
            Self::Safe => b"\x1b[?1000h\x1b[?2004habc".to_vec(),
            Self::PendingWrap => vec![b'A'; usize::from(size.cols)],
            Self::InsertMode => b"abcd\x1b[1;2H\x1b[4h".to_vec(),
            Self::SgrStyle => b"\x1b[31m".to_vec(),
            Self::Osc8Hyperlink => b"\x1b]8;;https://example.test\x1b\\".to_vec(),
            Self::ScrollMargins => b"\x1b[2;3r".to_vec(),
        }
    }
}

fn live_renderer(
    split: bool,
    state: PlainForwardingFixtureState,
    pane_style: bool,
) -> (Box<LivePaneRender>, SharedPaneTranscript) {
    let terminal_size = if split {
        TerminalSize { cols: 20, rows: 4 }
    } else {
        TerminalSize { cols: 8, rows: 4 }
    };
    let mut session =
        crate::session_fixture::SessionFixture::new(session_name("alpha"), terminal_size);
    if split {
        session
            .split_active_pane_with_direction(rmux_proto::SplitDirection::Vertical)
            .expect("split pane");
    }
    let pane = session.window().active_pane().expect("active pane").clone();
    let transcript_size = if split {
        TerminalSize {
            cols: pane.geometry().cols(),
            rows: pane.geometry().rows(),
        }
    } else {
        TerminalSize { cols: 8, rows: 3 }
    };
    let mut options = OptionStore::new();
    if pane_style {
        let target =
            WindowTarget::with_window(session.name().clone(), session.active_window_index());
        options
            .set(
                ScopeSelector::Window(target),
                OptionName::WindowActiveStyle,
                "fg=red".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("window style set succeeds");
    }
    let transcript = PaneTranscript::shared(100, transcript_size);
    transcript
        .lock()
        .append_bytes(&state.bytes(transcript_size));
    let renderer =
        LivePaneRender::new_from_transcript(transcript.clone(), session.view(), options, pane)
            .expect("initial render snapshot");
    (renderer, transcript)
}

fn assert_small_issue63_interactive_frame(frame: &str, stable_prefix: &str) {
    assert!(
            !frame.contains(&format!("{stable_prefix}-00"))
                && !frame.contains(&format!("{stable_prefix}-46")),
            "interactive render must not repaint unchanged history rows over SSH-sized panes: {frame:?}"
        );
    assert!(
            frame.len() < 1024,
            "interactive render should stay small; a full terminal repaint is far larger: len={} frame={frame:?}",
            frame.len()
        );
    assert!(
        frame.matches('H').count() <= 3,
        "interactive key echo should not emit one cursor position per row: {frame:?}"
    );
}

#[test]
fn replaceable_live_render_is_self_contained_for_client_side_coalescing() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 20, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane").clone();
    let options = OptionStore::new();
    let transcript = PaneTranscript::shared(100, TerminalSize { cols: 20, rows: 3 });
    transcript.lock().append_bytes(b"abc");

    let mut renderer =
        LivePaneRender::new_from_transcript(transcript.clone(), session.view(), options, pane)
            .expect("initial render snapshot");

    transcript.lock().append_bytes(b"d");

    let PaneRenderDelta::Incremental(delta) = renderer.render_frame_from_transcript(true) else {
        panic!("single-line output should render as an incremental delta");
    };
    let frame = String::from_utf8(delta.frame().to_vec()).expect("frame is utf8");

    assert!(frame.contains("\u{1b}[1;1H"));
    assert!(
        frame.contains("abcd\u{1b}[K"),
        "replaceable keyframes must clear a long synthetic tail: {frame:?}"
    );
    assert!(
        !frame.contains("abcd                "),
        "replaceable keyframes must not materialize the pane-width tail: {frame:?}"
    );
    assert!(
            frame.contains("\u{1b}[2;1H"),
            "replaceable render frames must be self-contained so clients can keep only the latest one: {frame:?}"
        );
}

#[test]
fn gh217_replaceable_live_keyframe_clears_application_authored_space_tails() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 20, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane").clone();
    let options = OptionStore::new();
    let transcript = PaneTranscript::shared(100, TerminalSize { cols: 20, rows: 3 });
    transcript.lock().append_bytes(b"A       ");

    let mut renderer =
        LivePaneRender::new_from_transcript(transcript, session.view(), options, pane)
            .expect("initial render snapshot");
    let PaneRenderDelta::Incremental(delta) = renderer.render_frame_from_transcript(true) else {
        panic!("replaceable keyframe should be incremental");
    };
    let frame = String::from_utf8(delta.frame().to_vec()).expect("frame is utf8");

    assert!(
        frame.contains("A\u{1b}[K"),
        "replaceable keyframes must clear matching application spaces: {frame:?}"
    );
    assert!(
        !frame.contains("A       \u{1b}[K"),
        "replaceable keyframes must not retain the application-space discriminator: {frame:?}"
    );
}

#[test]
fn interactive_live_render_only_repaints_changed_rows() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 10, rows: 4 },
    );
    let pane = session.window().active_pane().expect("active pane").clone();
    let options = OptionStore::new();
    let transcript = PaneTranscript::shared(100, TerminalSize { cols: 10, rows: 3 });
    transcript.lock().append_bytes(b"abc");

    let mut renderer =
        LivePaneRender::new_from_transcript(transcript.clone(), session.view(), options, pane)
            .expect("initial render snapshot");

    transcript.lock().append_bytes(b"d");

    let PaneRenderDelta::Incremental(delta) = renderer.render_interactive_frame_from_transcript()
    else {
        panic!("single-line output should render as an incremental delta");
    };
    let frame = String::from_utf8(delta.frame().to_vec()).expect("frame is utf8");

    assert!(
        frame.contains("d"),
        "interactive delta should include new text: {frame:?}"
    );
    assert!(
        !frame.contains("\u{1b}[2;1H"),
        "interactive render should not repaint unchanged rows: {frame:?}"
    );
}

#[test]
fn issue63_interactive_render_does_not_repaint_large_ssh_pane() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize {
            cols: 160,
            rows: 49,
        },
    );
    let pane = session.window().active_pane().expect("active pane").clone();
    let options = OptionStore::new();
    let transcript = PaneTranscript::shared(
        5000,
        TerminalSize {
            cols: 160,
            rows: 48,
        },
    );

    {
        let mut transcript = transcript.lock();
        for row in 0..47 {
            transcript.append_bytes(format!("ssh-row-{row:02} stable content\r\n").as_bytes());
        }
        transcript.append_bytes(b"prompt> ");
    }

    let mut renderer =
        LivePaneRender::new_from_transcript(transcript.clone(), session.view(), options, pane)
            .expect("initial render snapshot");

    transcript.lock().append_bytes(b"x");

    let PaneRenderDelta::Incremental(delta) = renderer.render_interactive_frame_from_transcript()
    else {
        panic!("single key echo should render as an incremental delta");
    };
    let frame = String::from_utf8(delta.frame().to_vec()).expect("frame is utf8");

    assert!(
        frame.contains('x'),
        "interactive delta should include the echoed key: {frame:?}"
    );
    assert!(
        frame.len() < 512,
        "plain append should normally use the tiny cursor append path: len={} frame={frame:?}",
        frame.len()
    );
    assert_small_issue63_interactive_frame(&frame, "ssh-row");
}

#[test]
fn issue63_interactive_render_stays_small_for_styled_prompts() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize {
            cols: 160,
            rows: 49,
        },
    );
    let pane = session.window().active_pane().expect("active pane").clone();
    let options = OptionStore::new();
    let transcript = PaneTranscript::shared(
        5000,
        TerminalSize {
            cols: 160,
            rows: 48,
        },
    );

    {
        let mut transcript = transcript.lock();
        for row in 0..47 {
            transcript.append_bytes(format!("styled-row-{row:02} stable content\r\n").as_bytes());
        }
        transcript.append_bytes(b"\x1b[32mprompt>\x1b[0m ");
    }

    let mut renderer =
        LivePaneRender::new_from_transcript(transcript.clone(), session.view(), options, pane)
            .expect("initial render snapshot");

    transcript.lock().append_bytes(b"x");

    let PaneRenderDelta::Incremental(delta) = renderer.render_interactive_frame_from_transcript()
    else {
        panic!("single styled key echo should render as an incremental delta");
    };
    let frame = String::from_utf8(delta.frame().to_vec()).expect("frame is utf8");

    assert!(
        frame.contains('x'),
        "interactive delta should include the echoed key: {frame:?}"
    );
    assert_small_issue63_interactive_frame(&frame, "styled-row");
}

#[test]
fn issue63_interactive_render_stays_small_for_split_panes() {
    let mut session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize {
            cols: 160,
            rows: 49,
        },
    );
    session
        .split_active_pane_with_direction(rmux_proto::SplitDirection::Vertical)
        .expect("split pane");
    let pane = session.window().active_pane().expect("active pane").clone();
    let pane_size = TerminalSize {
        cols: pane.geometry().cols(),
        rows: pane.geometry().rows(),
    };
    let options = OptionStore::new();
    let transcript = PaneTranscript::shared(5000, pane_size);

    {
        let mut transcript = transcript.lock();
        for row in 0..20 {
            transcript.append_bytes(format!("split-row-{row:02} stable content\r\n").as_bytes());
        }
        transcript.append_bytes(b"prompt> ");
    }

    let mut renderer =
        LivePaneRender::new_from_transcript(transcript.clone(), session.view(), options, pane)
            .expect("initial render snapshot");

    transcript.lock().append_bytes(b"x");

    let PaneRenderDelta::Incremental(delta) = renderer.render_interactive_frame_from_transcript()
    else {
        panic!("single split-pane key echo should render as an incremental delta");
    };
    let frame = String::from_utf8(delta.frame().to_vec()).expect("frame is utf8");

    assert!(
        frame.contains('x'),
        "interactive delta should include the echoed key: {frame:?}"
    );
    assert!(
        !frame.contains("split-row-00"),
        "split-pane interactive render must not repaint unchanged top rows: {frame:?}"
    );
    assert!(
        frame.len() < 1024,
        "split-pane interactive render should stay small; len={} frame={frame:?}",
        frame.len()
    );
}

#[test]
fn safe_plain_output_keeps_full_width_and_split_fast_paths() {
    let (mut full_width, _) = live_renderer(false, PlainForwardingFixtureState::Safe, false);
    assert!(full_width.can_forward_plain_bytes(b"d"));
    assert!(full_width.apply_forwarded_plain_bytes(b"d"));

    let (mut split, _) = live_renderer(true, PlainForwardingFixtureState::Safe, false);
    assert!(
        split.positioned_plain_output_frame(b"d").is_some(),
        "safe split-pane output should keep the positioned fast path"
    );
}

#[test]
fn unsafe_terminal_state_blocks_full_width_and_split_plain_output_fast_paths() {
    for state in PlainForwardingFixtureState::UNSAFE {
        let (full_width, _) = live_renderer(false, state, false);
        assert!(
            !full_width.can_forward_plain_bytes(b"X"),
            "{} must block full-width raw forwarding",
            state.label()
        );

        let (mut split, _) = live_renderer(true, state, false);
        assert!(
            split.positioned_plain_output_frame(b"X").is_none(),
            "{} must block split-pane positioned forwarding",
            state.label()
        );
    }
}

#[test]
fn pane_default_style_blocks_plain_output_fast_paths() {
    let (full_width, _) = live_renderer(false, PlainForwardingFixtureState::Safe, true);
    assert!(!full_width.can_forward_plain_bytes(b"X"));

    let (mut split, _) = live_renderer(true, PlainForwardingFixtureState::Safe, true);
    assert!(split.positioned_plain_output_frame(b"X").is_none());
}

#[test]
fn structured_render_reenables_fast_path_after_terminal_state_returns_safe() {
    let (mut renderer, transcript) =
        live_renderer(false, PlainForwardingFixtureState::SgrStyle, false);
    assert!(!renderer.can_forward_plain_bytes(b"X"));

    transcript.lock().append_bytes(b"\x1b[0m");
    assert!(matches!(
        renderer.render_interactive_frame_from_transcript(),
        PaneRenderDelta::Incremental(_)
    ));
    assert!(renderer.can_forward_plain_bytes(b"X"));
}

/// Counts the moves to the top-left cell that a client would show while the
/// cursor is visible and no synchronized update is open (issue #78: the cursor
/// flashed in the top-left corner while a full-screen TUI redrew its screen).
fn issue_78_visible_homes(frame: &[u8], mut visible: bool) -> usize {
    let mut synchronized = false;
    let mut homes = 0;
    let mut index = 0;
    while index + 1 < frame.len() {
        if frame[index] != 0x1b || frame[index + 1] != b'[' {
            index += 1;
            continue;
        }
        let mut end = index + 2;
        let private = frame.get(end) == Some(&b'?');
        if private {
            end += 1;
        }
        let params_start = end;
        while end < frame.len() && (frame[end].is_ascii_digit() || frame[end] == b';') {
            end += 1;
        }
        let Some(&final_byte) = frame.get(end) else {
            break;
        };
        let params = std::str::from_utf8(&frame[params_start..end]).expect("ascii params");
        if private && matches!(final_byte, b'h' | b'l') {
            for param in params.split(';') {
                match param {
                    "25" => visible = final_byte == b'h',
                    "2026" => synchronized = final_byte == b'h',
                    _ => {}
                }
            }
        } else if !private && matches!(final_byte, b'H' | b'f') {
            let mut fields = params.split(';');
            let row = fields
                .next()
                .filter(|value| !value.is_empty())
                .unwrap_or("1");
            let col = fields
                .next()
                .filter(|value| !value.is_empty())
                .unwrap_or("1");
            if row == "1" && col == "1" && visible && !synchronized {
                homes += 1;
            }
        }
        index = end + 1;
    }
    homes
}

#[test]
fn issue_78_tui_style_redraw_never_homes_a_visible_cursor() {
    let session = crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    );
    let pane = session.window().active_pane().expect("active pane").clone();
    let transcript = PaneTranscript::shared(
        1000,
        TerminalSize {
            cols: 120,
            rows: 39,
        },
    );
    transcript.lock().append_bytes(b"$ tui\r\n> ");
    let mut renderer = LivePaneRender::new_from_transcript(
        transcript.clone(),
        session.view(),
        OptionStore::new(),
        pane,
    )
    .expect("initial render snapshot");

    for (step, replaceable) in [true, false, true, false].into_iter().enumerate() {
        // The issue's steps: the TUI runs, the user sends "hi", the TUI redraws its
        // screen inside a synchronized update with the cursor hidden, then
        // parks the visible cursor back in its composer.
        transcript.lock().append_bytes(
            format!(
                "hi\x1b[?2026h\x1b[?25l\x1b[10;1H\x1b[2Ktui reply {step}\x1b[20;5H\x1b[?25h\x1b[?2026l"
            )
            .as_bytes(),
        );
        let PaneRenderDelta::Incremental(delta) =
            renderer.render_frame_from_transcript(replaceable)
        else {
            panic!("a TUI redraw should render as an incremental delta");
        };
        let frame = delta.frame();
        assert!(
            frame.windows(b"tui reply".len()).any(|w| w == b"tui reply"),
            "step {step}: the redraw must reach the client: {:?}",
            String::from_utf8_lossy(frame)
        );
        assert_eq!(
            issue_78_visible_homes(frame, true),
            0,
            "step {step} (replaceable={replaceable}): the client cursor must never sit visible in the top-left cell: {:?}",
            String::from_utf8_lossy(frame)
        );
    }
}
