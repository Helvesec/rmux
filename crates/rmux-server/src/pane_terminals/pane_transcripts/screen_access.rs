//! Read access to a pane's transcript screens, copy-mode views and mode state.

use rmux_core::{GridRenderOptions, PaneId, Screen, ScreenCaptureRange};
use rmux_proto::{PaneTarget, RmuxError, SessionName};

use crate::pane_screen_state::PaneScreenState;
use crate::pane_terminal_lookup::missing_pane_terminal;
use crate::pane_terminals::HandlerState;
use crate::pane_transcript::{
    PaneTranscript, PaneTranscriptReach, PresentationCapture, SharedPaneTranscript,
};

impl HandlerState {
    pub(super) fn pane_transcript_for_id(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<&SharedPaneTranscript> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.transcripts.get(&runtime_session_name)?.get(&pane_id)
    }

    /// Formats observe every pane a listing, loop or tree reaches, most of
    /// which the request never named. A poisoned transcript observes as
    /// unavailable, so its variables expand empty and the request still
    /// answers. The pane a request names keeps failing fast, as its own
    /// operations do, and answers "pane %N unavailable".
    pub(crate) fn observe_pane_transcript<R>(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
        reach: PaneTranscriptReach,
        read: impl FnOnce(&PaneTranscript) -> R,
    ) -> Option<R> {
        let transcript = self.pane_transcript_for_id(session_name, pane_id)?;
        let transcript = transcript.lock_for_format(reach)?;
        Some(read(&transcript))
    }

    /// [`Self::observe_pane_transcript`] through the runtime session that owns
    /// the given window, as `pane_viewport_state` resolves it.
    pub(crate) fn observe_pane_transcript_in_window<R>(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_id: PaneId,
        reach: PaneTranscriptReach,
        read: impl FnOnce(&PaneTranscript) -> R,
    ) -> Option<R> {
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        let transcript = transcript.lock_for_format(reach)?;
        Some(read(&transcript))
    }

    /// The alternate-screen and copy-mode flags a resize reads for each pane
    /// it sizes, which spans the window or session whatever pane the request
    /// named. A poisoned transcript reads as neither, as formats observe it,
    /// instead of failing a request after its layout change is made. A
    /// request that names the poisoned pane checks it before committing.
    pub(crate) fn resized_pane_viewport_state(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_id: PaneId,
    ) -> (bool, bool) {
        self.observe_pane_transcript_in_window(
            session_name,
            window_index,
            pane_id,
            PaneTranscriptReach::Reached,
            |transcript| {
                (
                    transcript.is_alternate(),
                    transcript.copy_mode_state().is_some(),
                )
            },
        )
        .unwrap_or((false, false))
    }

    #[cfg(test)]
    pub(crate) fn pane_output_sequence(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<u64> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        Some(transcript.lock().output_sequence())
    }

    pub(crate) fn pane_screen_state(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<PaneScreenState> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        let transcript = transcript.lock();
        Some(PaneScreenState::from_screen(transcript.screen()))
    }

    /// Client renders read the panes of the window each client shows, for
    /// requests that never named them. This and the other render reads below
    /// skip a poisoned transcript, as a cross-pane sweep does, so the client
    /// is drawn without that pane instead of failing the request.
    pub(crate) fn pane_presentation_screen_state(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<PaneScreenState> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        let transcript = transcript.lock_for_cross_pane_sweep()?;
        Some(PaneScreenState::from_screen(
            transcript.presentation_screen(),
        ))
    }

    #[cfg(feature = "web")]
    pub(crate) fn pane_recovery_screen_state(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Result<Option<(PaneScreenState, bool)>, RmuxError> {
        Ok(self
            .pane_recovery_screen(session_name, pane_id)?
            .map(|(screen, complete)| (PaneScreenState::from_screen(&screen), complete)))
    }

    pub(crate) fn pane_copy_mode_summary(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<crate::copy_mode::CopyModeSummary> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        transcript.lock().copy_mode_summary()
    }

    pub(crate) fn pane_copy_mode_render_snapshot(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<crate::copy_mode::CopyModeRenderSnapshot> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        transcript
            .lock_for_cross_pane_sweep()?
            .copy_mode_render_snapshot()
    }

    /// Renders a copy-mode pane under the bounded Web recovery budgets.
    ///
    /// Metadata that does not fit the budgets (over-long titles, an oversized
    /// hyperlink table) is dropped from the bounded clone, exactly like the
    /// non-copy-mode [`Self::pane_recovery_screen`], and is never a reason to
    /// fail the render: the Web session view reports the gap through
    /// `PaneScrollbackView::metadata_complete`. Only the geometry cap, which
    /// no bounded frame can satisfy, is an error.
    #[cfg(feature = "web")]
    pub(crate) fn pane_copy_mode_recovery_snapshot(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Result<Option<crate::copy_mode::CopyModeRenderSnapshot>, RmuxError> {
        let Some(window_index) = self
            .sessions
            .session(session_name)
            .and_then(|session| session.window_index_for_pane_id(pane_id))
        else {
            return Ok(None);
        };
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let Some(transcript) = self
            .transcripts
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
        else {
            return Ok(None);
        };
        let transcript = transcript.lock();
        crate::pane_recovery::validate_recovery_geometry(transcript.screen())?;
        Ok(transcript.copy_mode_render_snapshot_bounded(
            crate::pane_recovery::MAX_RECOVERY_STRING_BYTES,
            crate::pane_recovery::MAX_RECOVERY_TITLE_STACK_BYTES,
            crate::pane_recovery::MAX_RECOVERY_HYPERLINK_ENTRY_BYTES,
            crate::pane_recovery::MAX_RECOVERY_HYPERLINK_TOTAL_BYTES,
        ))
    }

    /// Lends the pane's live terminal screen under its transcript lock. Web
    /// rendering was its first caller; the runtime formats are a second one, and
    /// two integers are not worth the `pane_screen` clone below. A client
    /// render read: a poisoned transcript lends nothing.
    pub(crate) fn with_pane_screen<R>(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
        render: impl FnOnce(&Screen) -> R,
    ) -> Option<R> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        let transcript = transcript.lock_for_cross_pane_sweep()?;
        Some(render(transcript.screen()))
    }

    #[cfg(test)]
    pub(crate) fn pane_screen(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<Screen> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        Some(transcript.lock().screen().clone())
    }

    /// The presentation screen together with the state of the read that
    /// produced it, so a frame rendered from it can carry its own provenance
    /// instead of being described by a later capture. A client render read:
    /// a poisoned transcript captures nothing.
    pub(crate) fn pane_presentation_capture(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<PresentationCapture> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        Some(
            transcript
                .lock_for_cross_pane_sweep()?
                .capture_presentation_screen(),
        )
    }

    #[cfg(feature = "web")]
    pub(crate) fn pane_recovery_screen(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Result<Option<(Screen, bool)>, RmuxError> {
        let Some(window_index) = self
            .sessions
            .session(session_name)
            .and_then(|session| session.window_index_for_pane_id(pane_id))
        else {
            return Ok(None);
        };
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let Some(transcript) = self
            .transcripts
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
        else {
            return Ok(None);
        };
        let transcript = transcript.lock();
        let screen = transcript.presentation_screen();
        crate::pane_recovery::validate_recovery_geometry(screen)?;
        Ok(Some(screen.clone_recovery_viewport_bounded(
            crate::pane_recovery::MAX_RECOVERY_STRING_BYTES,
            crate::pane_recovery::MAX_RECOVERY_TITLE_STACK_BYTES,
            crate::pane_recovery::MAX_RECOVERY_HYPERLINK_ENTRY_BYTES,
            crate::pane_recovery::MAX_RECOVERY_HYPERLINK_TOTAL_BYTES,
        )))
    }

    #[cfg(test)]
    pub(crate) fn pane_in_mode(&self, session_name: &SessionName, pane_id: PaneId) -> bool {
        let Some(window_index) = self
            .sessions
            .session(session_name)
            .and_then(|session| session.window_index_for_pane_id(pane_id))
        else {
            return false;
        };
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.transcripts
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .is_some_and(|transcript| transcript.lock().pane_in_mode())
    }

    #[cfg(test)]
    pub(crate) fn pane_mode_name(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<&'static str> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.transcripts
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .and_then(|transcript| transcript.lock().pane_mode_name())
    }

    /// Clock overlays sweep every visible pane of a refreshed session, so a
    /// poisoned pane is skipped as by any cross-pane sweep, never aborted on.
    pub(crate) fn pane_clock_mode_generation(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<u64> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.transcripts
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .and_then(|transcript| {
                transcript
                    .lock_for_cross_pane_sweep()?
                    .clock_mode_generation()
            })
    }

    pub(crate) fn pane_visible_lines(&self, target: &PaneTarget) -> Result<Vec<String>, RmuxError> {
        let transcript = self.transcript_handle(target)?;
        let transcript = transcript.lock();
        let rows = self
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .and_then(|window| window.pane(target.pane_index()))
            .map(|pane| usize::from(pane.geometry().rows()))
            .ok_or_else(|| {
                missing_pane_terminal(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                )
            })?;

        let render_options = GridRenderOptions {
            with_sequences: true,
            trim_spaces: false,
            ..GridRenderOptions::default()
        };
        let mut lines = Vec::with_capacity(rows);
        for row in 0..rows {
            let screen = transcript.capture_main(
                ScreenCaptureRange::new(Some(row as i64), Some(row as i64)),
                render_options,
            );
            let mut line = String::from_utf8_lossy(&screen).into_owned();
            if line.ends_with('\n') {
                line.pop();
            }
            lines.push(line);
        }
        Ok(lines)
    }
}
