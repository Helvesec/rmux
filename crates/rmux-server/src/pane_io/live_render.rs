use crate::pane_transcript::{FramePresentation, PresentationCapture, SharedPaneTranscript};
use crate::renderer::{PaneRenderDelta, PaneRenderInputs, PaneRenderSnapshot};
use rmux_core::{OptionStore, Pane};

#[derive(Debug)]
pub(crate) struct LivePaneRender {
    transcript: SharedPaneTranscript,
    inputs: PaneRenderInputs,
    snapshot: PaneRenderSnapshot,
    plain_output_forwarding_safe: bool,
    /// The presentation state of the frame this client is currently showing.
    ///
    /// Seeded with the state of the prebuilt frame the target handed the
    /// client, then replaced — under the same transcript lock as the capture
    /// it describes — every time this renderer publishes a frame of its own.
    /// A later capture that produces no frame may only widen this state to
    /// frozen; it never claims that the client received a converged frame.
    delivered_frame: FramePresentation,
}

impl LivePaneRender {
    /// Builds the initial diff baseline and delivered-frame provenance from
    /// the immutable presentation capture used by the target's prebuilt
    /// frame. The constructor cannot accept a standalone provenance flag, so
    /// those two pieces cannot describe different transcript reads.
    pub(crate) fn new_from_presentation_capture(
        transcript: SharedPaneTranscript,
        session: rmux_core::ManagedSessionView<'_>,
        options: &OptionStore,
        pane: &Pane,
        capture: PresentationCapture,
    ) -> Option<Box<Self>> {
        let inputs = PaneRenderInputs::resolve(session, options, pane)?;
        let snapshot = PaneRenderSnapshot::capture_with_inputs(&inputs, capture.screen())?;
        let plain_output_forwarding_safe =
            capture.plain_output_forwarding_safe() && inputs.is_unstyled();
        let delivered_frame = capture.presentation();
        Some(Box::new(Self {
            transcript,
            inputs,
            snapshot,
            plain_output_forwarding_safe,
            delivered_frame,
        }))
    }

    /// Test fixtures that do not prebuild a real attach frame still start from
    /// one internally consistent current capture. Production target builders
    /// must use `new_from_presentation_capture` with their retained capture.
    #[cfg(test)]
    pub(crate) fn new_from_transcript(
        transcript: SharedPaneTranscript,
        session: rmux_core::ManagedSessionView<'_>,
        options: OptionStore,
        pane: Pane,
    ) -> Option<Box<Self>> {
        let capture = transcript.lock().capture_presentation_screen();
        Self::new_from_presentation_capture(transcript, session, &options, &pane, capture)
    }

    pub(crate) fn render_frame_from_transcript(&mut self, replaceable: bool) -> PaneRenderDelta {
        let Some((next, plain_output_forwarding_safe, presentation)) =
            self.capture_snapshot_from_transcript()
        else {
            return PaneRenderDelta::RequiresFullRefresh;
        };
        if replaceable {
            let cursor_style = (self.snapshot.cursor_style_state() != next.cursor_style_state())
                .then_some(next.cursor_style_state());
            let frame = next.full_frame();
            self.adopt(next, plain_output_forwarding_safe, presentation);
            return PaneRenderDelta::Incremental(
                crate::renderer::PaneRenderDeltaFrame::new_with_cursor_state(frame, cursor_style),
            );
        }
        let delta = self.snapshot.diff_to(&next);
        let PaneRenderDelta::Incremental(frame) = &delta else {
            return delta;
        };
        if !frame.frame().is_empty() || frame.cursor_style().is_some() {
            self.adopt(next, plain_output_forwarding_safe, presentation);
            return delta;
        }

        // An empty diff does not replace any bytes the client received. It may
        // conservatively widen the current frame to frozen (which keeps
        // positioned work held), but it must never claim that an older frozen
        // frame converged. When convergence changes no cells, publish a full
        // pane frame so that advancing the provenance still corresponds to an
        // actual client-visible frame.
        if self.delivered_frame.is_frozen() && !presentation.is_frozen() {
            let cursor_style = (self.snapshot.cursor_style_state() != next.cursor_style_state())
                .then_some(next.cursor_style_state());
            let frame = next.full_frame();
            self.adopt(next, plain_output_forwarding_safe, presentation);
            return PaneRenderDelta::Incremental(
                crate::renderer::PaneRenderDeltaFrame::new_with_cursor_state(frame, cursor_style),
            );
        }
        self.delivered_frame = self.delivered_frame.merge(presentation);
        // Parser safety is not client-frame provenance and may change without
        // drawing cells (for example, SGR reset). Re-enable the fast path only
        // while the delivered presentation remains converged.
        self.plain_output_forwarding_safe =
            !self.delivered_frame.is_frozen() && plain_output_forwarding_safe;
        delta
    }

    /// Takes ownership of a capture that is about to become the client's
    /// frame. The state travels with the snapshot it was read with, so a
    /// capture whose delta is discarded cannot describe what was published.
    fn adopt(
        &mut self,
        snapshot: PaneRenderSnapshot,
        plain_output_forwarding_safe: bool,
        presentation: FramePresentation,
    ) {
        self.snapshot = snapshot;
        self.plain_output_forwarding_safe = plain_output_forwarding_safe;
        self.delivered_frame = presentation;
    }

    pub(crate) fn render_interactive_frame_from_transcript(&mut self) -> PaneRenderDelta {
        self.render_frame_from_transcript(false)
    }

    pub(crate) fn can_forward_plain_bytes(&self, bytes: &[u8]) -> bool {
        !self.delivered_frame.is_frozen()
            && self.plain_output_forwarding_safe
            && self.snapshot.can_forward_plain_bytes(bytes)
    }

    pub(crate) fn positioned_plain_output_frame(&mut self, bytes: &[u8]) -> Option<Vec<u8>> {
        (!self.delivered_frame.is_frozen() && self.plain_output_forwarding_safe)
            .then(|| self.snapshot.positioned_plain_output_frame(bytes))
            .flatten()
    }

    pub(crate) fn apply_forwarded_plain_bytes(&mut self, bytes: &[u8]) -> bool {
        !self.delivered_frame.is_frozen()
            && self.plain_output_forwarding_safe
            && self.snapshot.apply_forwarded_plain_bytes(bytes)
    }

    /// Reports the state of the frame this client is currently showing. A
    /// passthrough carrying an absolute cursor position must not be emitted
    /// against a frozen frame: the converged render that follows repaints —
    /// and erases — the cells the passthrough drew into.
    pub(crate) fn delivered_frame(&self) -> FramePresentation {
        self.delivered_frame
    }

    /// Repaint the already delivered pane without admitting deferred output.
    pub(crate) fn delivered_screen_frame(&self) -> Vec<u8> {
        self.snapshot.full_frame()
    }

    fn capture_snapshot_from_transcript(
        &self,
    ) -> Option<(PaneRenderSnapshot, bool, FramePresentation)> {
        let capture = {
            let transcript = self.transcript.lock();
            let presentation = transcript.read_presentation();
            if let Some(snapshot) = PaneRenderSnapshot::capture_presentation_with_inputs(
                &self.inputs,
                &presentation,
                Some(&self.snapshot),
            ) {
                #[cfg(test)]
                crate::pane_transcript::presentation_test_clock::after_optimized_capture();
                let safe = presentation.plain_output_forwarding_safe() && self.inputs.is_unstyled();
                return Some((snapshot, safe, presentation.presentation()));
            }
            presentation.capture()
        };
        PaneRenderSnapshot::capture_with_inputs(&self.inputs, capture.screen())
            .map(|snapshot| (snapshot, false, capture.presentation()))
    }
}

#[cfg(test)]
#[path = "live_render/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "live_render/synchronized_output_tests.rs"]
mod synchronized_output_tests;

#[cfg(test)]
#[path = "live_render/capture_provenance_tests.rs"]
mod capture_provenance_tests;

#[cfg(test)]
#[path = "live_render/issue29_tests.rs"]
mod issue29_tests;
