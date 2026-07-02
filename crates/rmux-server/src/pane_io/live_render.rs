use crate::pane_transcript::SharedPaneTranscript;
use crate::renderer::{PaneRenderDelta, PaneRenderSnapshot};
use rmux_core::{OptionStore, Pane, Session};

#[derive(Debug)]
pub(crate) struct LivePaneRender {
    transcript: SharedPaneTranscript,
    session: Session,
    options: OptionStore,
    pane: Pane,
    snapshot: PaneRenderSnapshot,
}

impl LivePaneRender {
    pub(crate) fn new_from_transcript(
        transcript: SharedPaneTranscript,
        session: Session,
        options: OptionStore,
        pane: Pane,
    ) -> Option<Box<Self>> {
        let snapshot = {
            let transcript_guard = transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned");
            PaneRenderSnapshot::capture_unstyled_transcript_reusing(
                &session,
                &options,
                &pane,
                &transcript_guard,
                None,
            )
            .or_else(|| {
                PaneRenderSnapshot::capture(&session, &options, &pane, transcript_guard.screen())
            })?
        };
        Some(Box::new(Self {
            transcript,
            session,
            options,
            pane,
            snapshot,
        }))
    }

    pub(crate) fn render_frame_from_transcript(&mut self, replaceable: bool) -> PaneRenderDelta {
        let Some(next) = self.capture_snapshot_from_transcript() else {
            return PaneRenderDelta::RequiresFullRefresh;
        };
        if replaceable {
            let cursor_style = (self.snapshot.cursor_style() != next.cursor_style())
                .then_some(next.cursor_style());
            let frame = next.full_frame();
            self.snapshot = next;
            return PaneRenderDelta::Incremental(crate::renderer::PaneRenderDeltaFrame::new(
                frame,
                cursor_style,
            ));
        }
        let delta = self.snapshot.diff_to(&next);
        if matches!(delta, PaneRenderDelta::Incremental(_)) {
            self.snapshot = next;
        }
        delta
    }

    pub(crate) fn render_interactive_frame_from_transcript(&mut self) -> PaneRenderDelta {
        self.render_frame_from_transcript(false)
    }

    pub(crate) fn can_forward_plain_bytes(&self, bytes: &[u8]) -> bool {
        self.snapshot.can_forward_plain_bytes(bytes)
    }

    pub(crate) fn positioned_plain_echo_frame(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        self.snapshot.positioned_plain_echo_frame(bytes)
    }

    pub(crate) fn positioned_plain_output_frame(&mut self, bytes: &[u8]) -> Option<Vec<u8>> {
        self.snapshot.positioned_plain_output_frame(bytes)
    }

    pub(crate) fn apply_forwarded_plain_bytes(&mut self, bytes: &[u8]) -> bool {
        self.snapshot.apply_forwarded_plain_bytes(bytes)
    }

    fn capture_snapshot_from_transcript(&self) -> Option<PaneRenderSnapshot> {
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
                return Some(snapshot);
            }
            transcript.clone_screen()
        };
        PaneRenderSnapshot::capture(&self.session, &self.options, &self.pane, &screen)
    }
}

#[cfg(test)]
mod tests {
    use rmux_core::{OptionStore, Session};
    use rmux_proto::{SessionName, TerminalSize};

    use crate::pane_transcript::PaneTranscript;
    use crate::renderer::PaneRenderDelta;

    use super::LivePaneRender;

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
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
}
