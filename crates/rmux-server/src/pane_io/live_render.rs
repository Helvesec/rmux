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
    pub(crate) fn new(
        transcript: SharedPaneTranscript,
        session: Session,
        options: OptionStore,
        pane: Pane,
        screen: &rmux_core::Screen,
    ) -> Option<Box<Self>> {
        let snapshot = PaneRenderSnapshot::capture(&session, &options, &pane, screen)?;
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
    use rmux_core::{input::InputParser, OptionStore, Screen, Session};
    use rmux_proto::{SessionName, TerminalSize};

    use crate::pane_transcript::PaneTranscript;
    use crate::renderer::PaneRenderDelta;

    use super::LivePaneRender;

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
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

        let mut screen = Screen::new(TerminalSize { cols: 10, rows: 3 }, 100);
        InputParser::new().parse(b"abc", &mut screen);
        let mut renderer = LivePaneRender::new(transcript.clone(), session, options, pane, &screen)
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
}
