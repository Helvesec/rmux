use rmux_core::PaneId;
use rmux_proto::{PaneTarget, RmuxError, SessionName};

use super::HandlerState;
use crate::pane_terminal_lookup::pane_id_for_target;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MarkedPane {
    pub(super) target: PaneTarget,
    pub(super) pane_id: PaneId,
}

impl HandlerState {
    pub(crate) fn marked_pane_target(&self) -> Option<PaneTarget> {
        let marked = self.marked_pane.as_ref()?;
        self.pane_target_for_id_in_window(
            marked.target.session_name(),
            marked.target.window_index(),
            marked.pane_id,
        )
        .or_else(|| self.pane_target_for_id(marked.pane_id))
    }

    pub(crate) fn pane_is_marked(&self, target: &PaneTarget) -> bool {
        let Some(marked) = self.marked_pane.as_ref() else {
            return false;
        };
        pane_id_for_target(
            &self.sessions,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
            == Some(marked.pane_id)
    }

    pub(crate) fn session_has_marked_pane(&self, session_name: &SessionName) -> bool {
        self.marked_pane_target()
            .is_some_and(|target| target.session_name() == session_name)
    }

    pub(crate) fn window_has_marked_pane(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> bool {
        self.marked_pane_target().is_some_and(|target| {
            target.session_name() == session_name && target.window_index() == window_index
        })
    }

    pub(crate) fn clear_marked_pane(&mut self) {
        self.marked_pane = None;
    }

    pub(crate) fn toggle_marked_pane(&mut self, target: &PaneTarget) -> Result<bool, RmuxError> {
        let pane_id = pane_id_for_target(
            &self.sessions,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        if self
            .marked_pane
            .as_ref()
            .is_some_and(|marked| marked.pane_id == pane_id)
        {
            self.marked_pane = None;
            Ok(false)
        } else {
            self.marked_pane = Some(MarkedPane {
                target: target.clone(),
                pane_id,
            });
            Ok(true)
        }
    }

    pub(in crate::pane_terminals) fn clear_marked_pane_if_id(&mut self, pane_id: PaneId) {
        if self
            .marked_pane
            .as_ref()
            .is_some_and(|marked| marked.pane_id == pane_id)
        {
            self.marked_pane = None;
        }
    }

    fn pane_target_for_id_in_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_id: PaneId,
    ) -> Option<PaneTarget> {
        let session = self.sessions.session(session_name)?;
        let pane_index = session
            .window_at(window_index)?
            .panes()
            .iter()
            .find(|pane| pane.id() == pane_id)
            .map(|pane| pane.index())?;
        Some(PaneTarget::with_window(
            session_name.clone(),
            window_index,
            pane_index,
        ))
    }

    fn pane_target_for_id(&self, pane_id: PaneId) -> Option<PaneTarget> {
        self.sessions.iter().find_map(|(session_name, session)| {
            let window_index = session.window_index_for_pane_id(pane_id)?;
            self.pane_target_for_id_in_window(session_name, window_index, pane_id)
        })
    }
}
