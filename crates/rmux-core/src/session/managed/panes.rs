use super::creation::checked_id;
use super::*;
use crate::session::pane_operations::{PaneOperations, PaneRemoval};
use rmux_proto::SplitDirection;

/// Split controls, applied by the same algorithms as standalone sessions.
#[derive(Debug, Clone, Copy)]
pub struct ManagedPaneSplit {
    /// Axis of the split.
    pub direction: SplitDirection,
    /// Insert before the addressed pane (or window root).
    pub before: bool,
    /// Preserve active and last pane identities.
    pub detached: bool,
    /// Split the full window root instead of the addressed leaf.
    pub full_size: bool,
}

/// Newly created canonical pane, visible through all window links.
#[derive(Debug)]
#[must_use]
pub struct ManagedPaneCreation {
    /// Canonical owner identity.
    pub window_id: WindowId,
    /// Globally allocated pane identity.
    pub pane_id: PaneId,
    /// Pane index after positional renumbering.
    pub pane_index: u32,
}

/// Pane cleanup identities and, for a last-pane kill, the final original owner.
#[derive(Debug)]
#[must_use]
pub struct ManagedPaneRemoval {
    /// Mutated or destroyed canonical owner.
    pub window_id: WindowId,
    /// Removed runtime pane identities, each exactly once.
    pub pane_ids: Vec<PaneId>,
    /// Global link/session cleanup, only when killing a window's final pane.
    pub destroyed_window: Option<ManagedWindowRemoval>,
}

impl ManagedSessionStore {
    /// Creates a frontmost floating pane without mutating the tiled layout.
    pub fn new_floating_pane(
        &mut self,
        target: &WindowTarget,
        request: crate::FloatingPaneRequest,
        detached: bool,
    ) -> Result<ManagedPaneCreation, RmuxError> {
        let link = self.target_link(target)?;
        let pane_id = PaneId::new(checked_id(self.next_pane_id, "pane")?);
        let window = self.windows.resolve_mut(link).expect("validated link");
        let window_id = window.id();
        let pane_index = window.insert_floating_pane(pane_id, request, detached)?;
        self.next_pane_id += 1;
        Ok(ManagedPaneCreation {
            window_id,
            pane_id,
            pane_index,
        })
    }

    /// Splits an addressed canonical pane. Rejections consume no global IDs.
    pub fn split_pane(
        &mut self,
        target: &WindowTarget,
        pane_index: u32,
        options: ManagedPaneSplit,
    ) -> Result<ManagedPaneCreation, RmuxError> {
        let link = self.target_link(target)?;
        let pane_id = PaneId::new(checked_id(self.next_pane_id, "pane")?);
        let window = self.windows.resolve_mut(link).expect("validated link");
        let window_id = window.id();
        let mut operation = PaneOperations {
            name: target.session_name(),
            window_index: link.window_index,
            window,
        };
        let pane_index = if options.full_size {
            operation.split_full_size(
                pane_index,
                pane_id,
                options.direction,
                options.before,
                options.detached,
            )?
        } else {
            operation.split(
                pane_index,
                pane_id,
                options.direction,
                options.before,
                options.detached,
            )?
        };
        self.next_pane_id += 1;
        Ok(ManagedPaneCreation {
            window_id,
            pane_id,
            pane_index,
        })
    }

    /// Kills a pane; its final pane destroys the canonical window across all links.
    pub fn kill_pane(
        &mut self,
        target: &WindowTarget,
        pane_index: u32,
    ) -> Result<ManagedPaneRemoval, RmuxError> {
        let link = self.target_link(target)?;
        let window = self.windows.resolve_mut(link).expect("validated link");
        let window_id = window.id();
        let removal = PaneOperations {
            name: target.session_name(),
            window_index: link.window_index,
            window,
        }
        .kill(pane_index)?;
        let (pane_ids, destroyed_window) = match removal {
            PaneRemoval::Pane(pane) => (vec![pane.id()], None),
            PaneRemoval::LastPane => {
                let removal = self.destroy_pane_window(target)?;
                let pane_ids = removal
                    .window
                    .as_ref()
                    .expect("final canonical owner")
                    .panes()
                    .iter()
                    .map(crate::Pane::id)
                    .collect();
                (pane_ids, Some(removal))
            }
        };
        Ok(ManagedPaneRemoval {
            window_id,
            pane_ids,
            destroyed_window,
        })
    }

    /// Keeps the addressed pane and uses existing selection/zoom/renumbering rules.
    pub fn kill_other_panes(
        &mut self,
        target: &WindowTarget,
        pane_index: u32,
    ) -> Result<ManagedPaneRemoval, RmuxError> {
        let link = self.target_link(target)?;
        let window = self.windows.resolve_mut(link).expect("validated link");
        let window_id = window.id();
        let pane_ids = PaneOperations {
            name: target.session_name(),
            window_index: link.window_index,
            window,
        }
        .kill_others(pane_index)?;
        Ok(ManagedPaneRemoval {
            window_id,
            pane_ids,
            destroyed_window: None,
        })
    }
}
