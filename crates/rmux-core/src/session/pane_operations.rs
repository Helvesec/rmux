use super::{invalid_pane_target, invalid_window_target, Session};
use crate::{Pane, PaneGeometry, PaneId, Window};
use rmux_proto::{RmuxError, SessionName, SplitDirection};

/// Borrowed sequencing shared by standalone and canonical owners.
pub(super) struct PaneOperations<'a> {
    pub(super) name: &'a SessionName,
    pub(super) window_index: u32,
    pub(super) window: &'a mut Window,
}

pub(super) enum PaneRemoval {
    Pane(Pane),
    LastPane,
}

impl PaneOperations<'_> {
    pub(super) fn split(
        &mut self,
        pane_index: u32,
        pane_id: PaneId,
        direction: SplitDirection,
        before: bool,
        detached: bool,
    ) -> Result<u32, RmuxError> {
        let window = &mut *self.window;
        let position = window.pane_position(pane_index).ok_or_else(|| {
            invalid_pane_target(
                self.name,
                self.window_index,
                pane_index,
                "pane index does not exist in session",
            )
        })?;
        if !window.can_split_pane(pane_index, direction) {
            return Err(RmuxError::Message("no space for new pane".to_owned()));
        }
        Ok(window.split_at_position_with_id_and_direction_detached(
            position, pane_id, direction, before, detached,
        ))
    }
    pub(super) fn split_full_size(
        &mut self,
        pane_index: u32,
        pane_id: PaneId,
        direction: SplitDirection,
        before: bool,
        detached: bool,
    ) -> Result<u32, RmuxError> {
        let window = &mut *self.window;
        if window.pane(pane_index).is_none() {
            return Err(invalid_pane_target(
                self.name,
                self.window_index,
                pane_index,
                "pane index does not exist in session",
            ));
        }

        let previous_active_pane_id = window.active_pane().map(Pane::id);
        let previous_last_pane_id = window
            .last_pane_index()
            .and_then(|pane_index| window.pane(pane_index))
            .map(Pane::id);
        let new_index = window
            .panes()
            .iter()
            .map(Pane::index)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        window.insert_pane_full_size(
            Pane::new_with_id(pane_id, new_index, PaneGeometry::new(0, 0, 0, 0)),
            direction,
            before,
        )?;
        window.commit_split_renumber(
            pane_id,
            previous_active_pane_id,
            previous_last_pane_id,
            detached,
        );
        window
            .panes()
            .iter()
            .find(|pane| pane.id() == pane_id)
            .map(Pane::index)
            .ok_or_else(|| {
                RmuxError::Server(format!(
                    "pane id {} disappeared after full-size split",
                    pane_id.as_u32()
                ))
            })
    }
    pub(super) fn kill(&mut self, pane_index: u32) -> Result<PaneRemoval, RmuxError> {
        self.validate(pane_index)?;
        if self.window.pane_count() == 1 {
            return Ok(PaneRemoval::LastPane);
        }
        Ok(PaneRemoval::Pane(
            self.window
                .remove_pane(pane_index)
                .expect("validated multi-pane removal"),
        ))
    }

    pub(super) fn kill_others(&mut self, pane_index: u32) -> Result<Vec<PaneId>, RmuxError> {
        self.validate(pane_index)?;
        Ok(self
            .window
            .remove_other_panes(pane_index)
            .expect("validated pane"))
    }

    fn validate(&self, pane_index: u32) -> Result<(), RmuxError> {
        self.window.pane(pane_index).map(|_| ()).ok_or_else(|| {
            invalid_pane_target(
                self.name,
                self.window_index,
                pane_index,
                "pane index does not exist in session",
            )
        })
    }
}

impl Session {
    pub(super) fn pane_operations(
        &mut self,
        window_index: u32,
    ) -> Result<PaneOperations<'_>, RmuxError> {
        let window = self
            .windows
            .get_mut(&window_index)
            .ok_or_else(|| invalid_window_target(&self.name, window_index))?;
        Ok(PaneOperations {
            name: &self.name,
            window_index,
            window,
        })
    }
}
