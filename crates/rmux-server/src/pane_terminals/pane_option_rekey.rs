use std::collections::HashMap;

use rmux_core::{PaneId, Session};
use rmux_proto::{PaneTarget, RmuxError, SessionName};

use super::{session_not_found, HandlerState};

pub(crate) type PaneSlotSnapshot = HashMap<PaneId, PaneTarget>;

impl HandlerState {
    pub(crate) fn pane_option_slots_for_session(
        &self,
        session_name: &SessionName,
    ) -> Result<PaneSlotSnapshot, RmuxError> {
        let session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        Ok(pane_option_slots(session))
    }

    pub(crate) fn rekey_pane_options_after_session_change(
        &mut self,
        before: &PaneSlotSnapshot,
        session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        let after = self
            .sessions
            .session(session_name)
            .map(pane_option_slots)
            .unwrap_or_default();
        self.rekey_pane_options_between_snapshots(before, &after)
    }

    pub(crate) fn rekey_pane_options_between_snapshots(
        &mut self,
        before: &PaneSlotSnapshot,
        after: &PaneSlotSnapshot,
    ) -> Result<(), RmuxError> {
        let mappings = before
            .iter()
            .filter_map(|(pane_id, source)| match after.get(pane_id) {
                Some(target) if target != source => Some((source.clone(), Some(target.clone()))),
                None => Some((source.clone(), None)),
                Some(_) => None,
            })
            .collect::<Vec<_>>();
        self.options.rekey_pane_overrides(&mappings)
    }
}

fn pane_option_slots(session: &Session) -> PaneSlotSnapshot {
    session
        .windows()
        .iter()
        .flat_map(|(window_index, window)| {
            window.panes().iter().map(move |pane| {
                (
                    pane.id(),
                    PaneTarget::with_window(session.name().clone(), *window_index, pane.index()),
                )
            })
        })
        .collect()
}
