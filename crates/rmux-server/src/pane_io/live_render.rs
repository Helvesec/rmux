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

    pub(crate) fn render_delta_from_transcript(&mut self) -> PaneRenderDelta {
        let screen = {
            let transcript = self
                .transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned");
            transcript.clone_screen()
        };
        let Some(next) =
            PaneRenderSnapshot::capture(&self.session, &self.options, &self.pane, &screen)
        else {
            return PaneRenderDelta::RequiresFullRefresh;
        };
        let delta = self.snapshot.diff_to(&next);
        if matches!(delta, PaneRenderDelta::Incremental(_)) {
            self.snapshot = next;
        }
        delta
    }
}
