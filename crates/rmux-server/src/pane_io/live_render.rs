use crate::pane_transcript::SharedPaneTranscript;
use crate::renderer::{pane_default_style, PaneRenderDelta, PaneRenderSnapshot};
use rmux_core::{OptionStore, Pane, Session};

#[derive(Debug)]
pub(crate) struct LivePaneRender {
    transcript: SharedPaneTranscript,
    session: Session,
    options: OptionStore,
    pane: Pane,
    snapshot: PaneRenderSnapshot,
    plain_output_forwarding_safe: bool,
}

impl LivePaneRender {
    pub(crate) fn new_from_transcript(
        transcript: SharedPaneTranscript,
        session: Session,
        options: OptionStore,
        pane: Pane,
    ) -> Option<Box<Self>> {
        let (snapshot, plain_output_forwarding_safe) = {
            let transcript_guard = transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned");
            if let Some(snapshot) = PaneRenderSnapshot::capture_unstyled_transcript_reusing(
                &session,
                &options,
                &pane,
                &transcript_guard,
                None,
            ) {
                let safe = transcript_guard.plain_output_forwarding_safe()
                    && pane_default_style(&session, &options, &pane).is_none();
                (snapshot, safe)
            } else {
                (
                    PaneRenderSnapshot::capture(
                        &session,
                        &options,
                        &pane,
                        transcript_guard.screen(),
                    )?,
                    false,
                )
            }
        };
        Some(Box::new(Self {
            transcript,
            session,
            options,
            pane,
            snapshot,
            plain_output_forwarding_safe,
        }))
    }

    pub(crate) fn render_frame_from_transcript(&mut self, replaceable: bool) -> PaneRenderDelta {
        let Some((next, plain_output_forwarding_safe)) = self.capture_snapshot_from_transcript()
        else {
            return PaneRenderDelta::RequiresFullRefresh;
        };
        if replaceable {
            let cursor_style = (self.snapshot.cursor_style() != next.cursor_style())
                .then_some(next.cursor_style());
            let frame = next.full_frame();
            self.snapshot = next;
            self.plain_output_forwarding_safe = plain_output_forwarding_safe;
            return PaneRenderDelta::Incremental(crate::renderer::PaneRenderDeltaFrame::new(
                frame,
                cursor_style,
            ));
        }
        let delta = self.snapshot.diff_to(&next);
        if matches!(delta, PaneRenderDelta::Incremental(_)) {
            self.snapshot = next;
            self.plain_output_forwarding_safe = plain_output_forwarding_safe;
        }
        delta
    }

    pub(crate) fn render_interactive_frame_from_transcript(&mut self) -> PaneRenderDelta {
        self.render_frame_from_transcript(false)
    }

    pub(crate) fn can_forward_plain_bytes(&self, bytes: &[u8]) -> bool {
        self.plain_output_forwarding_safe && self.snapshot.can_forward_plain_bytes(bytes)
    }

    pub(crate) fn positioned_plain_output_frame(&mut self, bytes: &[u8]) -> Option<Vec<u8>> {
        self.plain_output_forwarding_safe
            .then(|| self.snapshot.positioned_plain_output_frame(bytes))
            .flatten()
    }

    pub(crate) fn apply_forwarded_plain_bytes(&mut self, bytes: &[u8]) -> bool {
        self.plain_output_forwarding_safe && self.snapshot.apply_forwarded_plain_bytes(bytes)
    }

    fn capture_snapshot_from_transcript(&self) -> Option<(PaneRenderSnapshot, bool)> {
        let screen = {
            let transcript = self
                .transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned");
            if let Some(snapshot) = PaneRenderSnapshot::capture_unstyled_transcript_reusing(
                &self.session,
                &self.options,
                &self.pane,
                &transcript,
                Some(&self.snapshot),
            ) {
                let safe = transcript.plain_output_forwarding_safe()
                    && pane_default_style(&self.session, &self.options, &self.pane).is_none();
                return Some((snapshot, safe));
            }
            transcript.clone_screen()
        };
        PaneRenderSnapshot::capture(&self.session, &self.options, &self.pane, &screen)
            .map(|snapshot| (snapshot, false))
    }
}

#[cfg(test)]
mod tests {
    use rmux_core::{OptionStore, Session};
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
        let mut session = Session::new(session_name("alpha"), terminal_size);
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
            .expect("transcript mutex must not be poisoned")
            .append_bytes(&state.bytes(transcript_size));
        let renderer =
            LivePaneRender::new_from_transcript(transcript.clone(), session, options, pane)
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
        let session = Session::new(session_name("alpha"), TerminalSize { cols: 10, rows: 4 });
        let pane = session.window().active_pane().expect("active pane").clone();
        let options = OptionStore::new();
        let transcript = PaneTranscript::shared(100, TerminalSize { cols: 10, rows: 3 });
        transcript
            .lock()
            .expect("transcript mutex must not be poisoned")
            .append_bytes(b"abc");

        let mut renderer =
            LivePaneRender::new_from_transcript(transcript.clone(), session, options, pane)
                .expect("initial render snapshot");

        transcript
            .lock()
            .expect("transcript mutex must not be poisoned")
            .append_bytes(b"d");

        let PaneRenderDelta::Incremental(delta) = renderer.render_frame_from_transcript(true)
        else {
            panic!("single-line output should render as an incremental delta");
        };
        let frame = String::from_utf8(delta.frame().to_vec()).expect("frame is utf8");

        assert!(frame.contains("\u{1b}[1;1H"));
        assert!(frame.contains("abcd"));
        assert!(
            frame.contains("\u{1b}[2;1H"),
            "replaceable render frames must be self-contained so clients can keep only the latest one: {frame:?}"
        );
    }

    #[test]
    fn interactive_live_render_only_repaints_changed_rows() {
        let session = Session::new(session_name("alpha"), TerminalSize { cols: 10, rows: 4 });
        let pane = session.window().active_pane().expect("active pane").clone();
        let options = OptionStore::new();
        let transcript = PaneTranscript::shared(100, TerminalSize { cols: 10, rows: 3 });
        transcript
            .lock()
            .expect("transcript mutex must not be poisoned")
            .append_bytes(b"abc");

        let mut renderer =
            LivePaneRender::new_from_transcript(transcript.clone(), session, options, pane)
                .expect("initial render snapshot");

        transcript
            .lock()
            .expect("transcript mutex must not be poisoned")
            .append_bytes(b"d");

        let PaneRenderDelta::Incremental(delta) =
            renderer.render_interactive_frame_from_transcript()
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
        let session = Session::new(
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
            let mut transcript = transcript
                .lock()
                .expect("transcript mutex must not be poisoned");
            for row in 0..47 {
                transcript.append_bytes(format!("ssh-row-{row:02} stable content\r\n").as_bytes());
            }
            transcript.append_bytes(b"prompt> ");
        }

        let mut renderer =
            LivePaneRender::new_from_transcript(transcript.clone(), session, options, pane)
                .expect("initial render snapshot");

        transcript
            .lock()
            .expect("transcript mutex must not be poisoned")
            .append_bytes(b"x");

        let PaneRenderDelta::Incremental(delta) =
            renderer.render_interactive_frame_from_transcript()
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
        let session = Session::new(
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
            let mut transcript = transcript
                .lock()
                .expect("transcript mutex must not be poisoned");
            for row in 0..47 {
                transcript
                    .append_bytes(format!("styled-row-{row:02} stable content\r\n").as_bytes());
            }
            transcript.append_bytes(b"\x1b[32mprompt>\x1b[0m ");
        }

        let mut renderer =
            LivePaneRender::new_from_transcript(transcript.clone(), session, options, pane)
                .expect("initial render snapshot");

        transcript
            .lock()
            .expect("transcript mutex must not be poisoned")
            .append_bytes(b"x");

        let PaneRenderDelta::Incremental(delta) =
            renderer.render_interactive_frame_from_transcript()
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
        let mut session = Session::new(
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
            let mut transcript = transcript
                .lock()
                .expect("transcript mutex must not be poisoned");
            for row in 0..20 {
                transcript
                    .append_bytes(format!("split-row-{row:02} stable content\r\n").as_bytes());
            }
            transcript.append_bytes(b"prompt> ");
        }

        let mut renderer =
            LivePaneRender::new_from_transcript(transcript.clone(), session, options, pane)
                .expect("initial render snapshot");

        transcript
            .lock()
            .expect("transcript mutex must not be poisoned")
            .append_bytes(b"x");

        let PaneRenderDelta::Incremental(delta) =
            renderer.render_interactive_frame_from_transcript()
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

        transcript
            .lock()
            .expect("transcript mutex must not be poisoned")
            .append_bytes(b"\x1b[0m");
        assert!(matches!(
            renderer.render_interactive_frame_from_transcript(),
            PaneRenderDelta::Incremental(_)
        ));
        assert!(renderer.can_forward_plain_bytes(b"X"));
    }
}
