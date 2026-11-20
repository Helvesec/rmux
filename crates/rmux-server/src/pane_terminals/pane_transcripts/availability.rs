//! Pane availability checked before a layout change commits.

use rmux_core::PaneId;
use rmux_proto::{PaneTarget, RmuxError, SessionName};

use crate::pane_terminal_lookup::pane_id_for_target;
use crate::pane_terminals::HandlerState;

impl HandlerState {
    /// Refuses a split in `session_name` while one of its panes has a poisoned
    /// transcript, before anything changes. Once committed, the split resizes
    /// and refreshes every pane of the session through fail-fast transcript
    /// reads, so its client would read "pane %N unavailable" for a split
    /// already made. Panes are found as that resize finds them;
    /// a missing session is left to the request's own lookup.
    pub(crate) fn ensure_session_panes_available(
        &self,
        session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        let Some(session) = self.sessions.session(session_name) else {
            return Ok(());
        };
        for (window_index, window) in session.windows() {
            let runtime_session_name =
                self.runtime_session_name_for_window(session_name, window_index);
            let Some(transcripts) = self.transcripts.get(&runtime_session_name) else {
                continue;
            };
            for pane in window.panes() {
                if let Some(transcript) = transcripts.get(&pane.id()) {
                    transcript
                        .ensure_available()
                        .map_err(|unavailable| RmuxError::Message(unavailable.to_string()))?;
                }
            }
        }
        Ok(())
    }

    /// Refuses a request that names `pane_id`, a pane of `session_name`'s
    /// window `window_index`, while its transcript is poisoned. The resize that
    /// follows a layout change reads the window's other panes under the sweep
    /// policy, so the pane a request names fails fast here, before the change
    /// it made stays committed.
    pub(crate) fn ensure_pane_id_available(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_id: PaneId,
    ) -> Result<(), RmuxError> {
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let Some(transcript) = self
            .transcripts
            .get(&runtime_session_name)
            .and_then(|transcripts| transcripts.get(&pane_id))
        else {
            return Ok(());
        };
        transcript
            .ensure_available()
            .map_err(|unavailable| RmuxError::Message(unavailable.to_string()))
    }

    /// [`Self::ensure_pane_id_available`] for the pane `pane` names; a missing
    /// pane is left to the request's own lookup.
    pub(crate) fn ensure_pane_available(&self, pane: &PaneTarget) -> Result<(), RmuxError> {
        let Ok(pane_id) = pane_id_for_target(
            &self.sessions,
            pane.session_name(),
            pane.window_index(),
            pane.pane_index(),
        ) else {
            return Ok(());
        };
        self.ensure_pane_id_available(pane.session_name(), pane.window_index(), pane_id)
    }
}
