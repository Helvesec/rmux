use super::creation::checked_id;
use super::*;
use crate::session::pane_operations::{PaneOperations, PaneRemoval};
use crate::{KillPaneOutcome, PaneGeometry};
use rmux_proto::SplitDirection;

impl ManagedSessionStore {
    /// Projects a split on only the addressed window without allocating an ID
    /// or changing canonical state. Uses the same pane operation as commit.
    pub fn preview_split_pane(
        &self,
        target: &WindowTarget,
        pane_index: u32,
        direction: SplitDirection,
        before: bool,
    ) -> Result<(u32, PaneGeometry), RmuxError> {
        let mut window = self.window(target)?.clone();
        let pane_id = PaneId::new(checked_id(self.next_pane_id, "pane")?);
        let index = PaneOperations {
            name: target.session_name(),
            window_index: target.window_index(),
            window: &mut window,
        }
        .split(pane_index, pane_id, direction, before, false)?;
        Ok((
            index,
            window.pane(index).expect("projected split pane").geometry(),
        ))
    }

    /// Validates pane removal and returns cleanup identities without changing
    /// live owners, links, selection or layout.
    pub fn preview_kill_pane(
        &self,
        target: &WindowTarget,
        pane_index: u32,
        kill_others: bool,
    ) -> Result<KillPaneOutcome, RmuxError> {
        let mut window = self.window(target)?.clone();
        let mut operation = PaneOperations {
            name: target.session_name(),
            window_index: target.window_index(),
            window: &mut window,
        };
        if kill_others {
            return Ok(KillPaneOutcome::new(
                operation.kill_others(pane_index)?,
                false,
            ));
        }
        match operation.kill(pane_index)? {
            PaneRemoval::Pane(pane) => Ok(KillPaneOutcome::new(vec![pane.id()], false)),
            PaneRemoval::LastPane => Ok(KillPaneOutcome::new(
                window.panes().iter().map(crate::Pane::id).collect(),
                true,
            )),
        }
    }

    /// Sizes a newly split pane against its original target cell.
    pub fn resize_new_split_pane_to(
        &mut self,
        target: &WindowTarget,
        pane_index: u32,
        direction: SplitDirection,
        size: u32,
        before: bool,
    ) -> Result<(), RmuxError> {
        let link = self.target_link(target)?;
        let window = self.windows.resolve_mut(link).expect("validated link");
        if window.pane(pane_index).is_none() {
            return Err(crate::session::invalid_pane_target(
                target.session_name(),
                target.window_index(),
                pane_index,
                "pane index does not exist in session",
            ));
        }
        let _ = window.resize_new_split_pane_to(pane_index, direction, size, before);
        Ok(())
    }
}
