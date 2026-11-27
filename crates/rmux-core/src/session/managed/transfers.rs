use super::*;
use crate::session::{pane_transfer_borrowed, pane_transfer_join, pane_transfer_shared};
use crate::{Pane, PaneJoinOptions, PaneSwapOptions, SessionPaneTarget};

/// Canonical transfer and runtime cleanup. Moved panes never appear in cleanup.
#[derive(Debug)]
#[must_use]
pub struct ManagedPaneTransfer {
    /// Original canonical owner.
    pub source_window_id: WindowId,
    /// Resulting canonical owner.
    pub target_window_id: WindowId,
    /// Pane identity that moved from the source.
    pub pane_id: PaneId,
    /// Resulting index in the destination.
    pub pane_index: u32,
    /// Removed source links/sessions and, for a join, its empty owner.
    /// A single-pane break retains the owner at its new occurrence.
    pub destroyed_window: Option<ManagedWindowRemoval>,
    /// Final occurrence/navigation changes for surviving sessions.
    pub change: ManagedIndexChange,
}

impl ManagedSessionStore {
    /// Swaps panes, resolving aliases of one owner as a same-window operation.
    pub fn swap_panes(
        &mut self,
        source: &WindowTarget,
        source_index: u32,
        target: &WindowTarget,
        target_index: u32,
        options: PaneSwapOptions,
    ) -> Result<ManagedPaneTransfer, RmuxError> {
        let snapshot = super::transfer_indices::TransferIndices::capture(self);
        let source_link = self.target_link(source)?;
        let incoming = self.transfer_pane(source, source_index)?.clone();
        let target_link = self.target_link(target)?;
        let source_id = self.window(source)?.id();
        let target_id = self.window(target)?.id();
        let outgoing = self.transfer_pane(target, target_index)?.clone();
        let pane_id = incoming.id();
        if source_id == target_id {
            pane_transfer_borrowed::swap_within(
                self.windows
                    .resolve_mut(source_link)
                    .expect("validated link"),
                source.session_name(),
                source.window_index(),
                source_index,
                target_index,
                options,
            )?;
        } else {
            let (source_window, target_window) =
                self.windows.transfer_pair_mut(source_id, target_id);
            pane_transfer_shared::validate_swap_destination(
                source_window,
                &outgoing,
                source_index,
            )?;
            pane_transfer_shared::validate_swap_destination(
                target_window,
                &incoming,
                target_index,
            )?;
            pane_transfer_shared::apply_swap_between_windows(
                source_window,
                pane_transfer_shared::SwapPaneEntry {
                    index: source_index,
                    pane: incoming,
                },
                target_window,
                pane_transfer_shared::SwapPaneEntry {
                    index: target_index,
                    pane: outgoing,
                },
                options,
            )?;
        }
        let pane_index = self
            .windows
            .resolve(target_link)
            .expect("validated link")
            .panes()
            .iter()
            .find(|p| p.id() == pane_id)
            .expect("transferred pane")
            .index();
        Ok(ManagedPaneTransfer {
            source_window_id: source_id,
            target_window_id: target_id,
            pane_id,
            pane_index,
            destroyed_window: None,
            change: snapshot.finish(self, &HashMap::new()),
        })
    }

    /// Joins panes through borrowed owners; final-pane moves destroy all source aliases.
    pub fn join_pane(
        &mut self,
        source: &WindowTarget,
        source_index: u32,
        target: &WindowTarget,
        target_index: u32,
        options: PaneJoinOptions,
    ) -> Result<ManagedPaneTransfer, RmuxError> {
        self.join_pane_with_session_teardown(
            source,
            source_index,
            target,
            target_index,
            options,
            false,
        )
    }

    /// Keeps empty session metadata until the server reaches native runtime teardown.
    pub fn join_pane_for_runtime_transfer(
        &mut self,
        source: &WindowTarget,
        source_index: u32,
        target: &WindowTarget,
        target_index: u32,
        options: PaneJoinOptions,
    ) -> Result<ManagedPaneTransfer, RmuxError> {
        self.join_pane_with_session_teardown(
            source,
            source_index,
            target,
            target_index,
            options,
            true,
        )
    }

    fn join_pane_with_session_teardown(
        &mut self,
        source: &WindowTarget,
        source_index: u32,
        target: &WindowTarget,
        target_index: u32,
        options: PaneJoinOptions,
        defer_session_removal: bool,
    ) -> Result<ManagedPaneTransfer, RmuxError> {
        let snapshot = super::transfer_indices::TransferIndices::capture(self);
        let source_link = self.target_link(source)?;
        let source_id = self.window(source)?.id();
        if source_index == target_index
            && self
                .window(target)
                .is_ok_and(|window| window.id() == source_id)
        {
            return Err(RmuxError::Server(
                "source and target panes must be different".to_owned(),
            ));
        }
        let pane_id = self.transfer_pane(source, source_index)?.id();
        let target_id = self.window(target)?.id();
        self.transfer_pane(target, target_index)?;
        self.validate_transfer_groups()?;
        let source_pane = SessionPaneTarget::new(source.window_index(), source_index);
        let target_pane = SessionPaneTarget::new(target.window_index(), target_index);
        let empty = if source_id == target_id {
            pane_transfer_borrowed::join_within(
                self.windows
                    .resolve_mut(source_link)
                    .expect("validated link"),
                source.session_name(),
                source_pane,
                target_pane,
                options,
            )?;
            false
        } else {
            let (source_window, target_window) =
                self.windows.transfer_pair_mut(source_id, target_id);
            let moved_index = pane_transfer_join::join_between(
                source_window,
                source.session_name(),
                source_pane,
                target_window,
                target.session_name(),
                target_pane,
                options,
            )?;
            if !options.detached && source.session_name() != target.session_name() {
                target_window.select_pane(moved_index);
            }
            source_window.pane_count() == 0
        };
        // Same-owner joins only affect pane selection, even across session aliases.
        if source_id != target_id && !options.detached {
            self.select_window(target)?;
        }
        let destroyed_window = if empty {
            Some(
                self.destroy_pane_window_with_session_teardown(source, defer_session_removal)
                    .expect("prevalidated groups"),
            )
        } else {
            None
        };
        let pane_index = self
            .window(target)?
            .panes()
            .iter()
            .find(|p| p.id() == pane_id)
            .expect("transferred pane")
            .index();
        Ok(ManagedPaneTransfer {
            source_window_id: source_id,
            target_window_id: target_id,
            pane_id,
            pane_index,
            destroyed_window,
            change: snapshot.finish(self, &HashMap::new()),
        })
    }

    pub(super) fn transfer_pane(
        &self,
        target: &WindowTarget,
        index: u32,
    ) -> Result<&Pane, RmuxError> {
        self.window(target)?.pane(index).ok_or_else(|| {
            crate::session::target_error::invalid_pane_target(
                target.session_name(),
                target.window_index(),
                index,
                "pane index does not exist in session",
            )
        })
    }

    pub(super) fn validate_transfer_groups(&self) -> Result<(), RmuxError> {
        for name in self.sessions.keys() {
            let links = self.index_links(name)?;
            for member in self.session_group_members(name) {
                if self.index_links(&member)? != links {
                    return Err(RmuxError::Server(
                        "managed group link tables differ".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }
}
