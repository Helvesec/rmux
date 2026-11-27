use std::borrow::Cow;

use super::Window;
use crate::{layout::LayoutTree, Pane, PaneId};

impl Window {
    /// Managed field edits cannot replace a pane, so they keep identity ownership.
    pub(super) fn managed_pane_mut(&mut self, pane_index: u32) -> Option<&mut Pane> {
        self.panes
            .iter_mut()
            .find(|pane| pane.index() == pane_index)
    }

    pub(super) fn borrowed_pane_replacements(&self) -> Vec<(PaneId, PaneId)> {
        self.borrowed_pane_owners
            .as_ref()
            .map_or_else(Vec::new, |owners| {
                debug_assert_eq!(owners.len(), self.panes.len());
                owners
                    .iter()
                    .copied()
                    .zip(self.panes.iter().map(Pane::id))
                    .filter(|(old, new)| old != new)
                    .collect()
            })
    }

    /// An `&mut Pane` has no drop callback. Until the next layout mutation,
    /// resolve its occupied cell against the current value in the borrowed slot.
    /// Keep the original owners across consecutive borrows so exchanges that
    /// temporarily duplicate an ID are remapped simultaneously, not cascaded.
    pub(crate) fn current_layout_tree(&self) -> Option<Cow<'_, LayoutTree>> {
        let tree = self.layout_tree.as_ref()?;
        let replacements = self.borrowed_pane_replacements();
        if replacements.is_empty() {
            Some(Cow::Borrowed(tree))
        } else {
            let mut tree = tree.clone();
            tree.remap_ids(&replacements);
            Some(Cow::Owned(tree))
        }
    }

    /// Seal raw replacement before changing pane order or layout ownership.
    pub(super) fn reconcile_borrowed_pane_owners(&mut self) {
        let replacements = self.borrowed_pane_replacements();
        if !replacements.is_empty() {
            if let Some(tree) = &mut self.layout_tree {
                tree.remap_ids(&replacements);
            }
            self.remap_floating_pane_ids(&replacements);
        }
        self.borrowed_pane_owners = None;
        self.sync_resize_topology();
    }
}

// Borrow bookkeeping is not observable window state. Compare resolved ownership
// so a no-op borrow or checkpoint round trip preserves public equality.
impl PartialEq for Window {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.panes == other.panes
            && self.next_pane_index == other.next_pane_index
            && self.active_pane == other.active_pane
            && self.last_pane == other.last_pane
            && self.layout == other.layout
            && self.last_layout == other.last_layout
            && self.current_layout_tree() == other.current_layout_tree()
            && self.current_floating_stack() == other.current_floating_stack()
            && self.current_resize_topology() == other.current_resize_topology()
            && self.custom_layout == other.custom_layout
            && self.old_layout == other.old_layout
            && self.size == other.size
            && self.manual_size == other.manual_size
            && self.name == other.name
            && self.automatic_rename == other.automatic_rename
            && self.zoomed == other.zoomed
            && self.zoom_restore_pending == other.zoom_restore_pending
            && self.alert_flags == other.alert_flags
            && self.alerts_queued == other.alerts_queued
            && self.created_at == other.created_at
            && self.activity_at == other.activity_at
            && self.requested_main_width == other.requested_main_width
            && self.requested_main_height == other.requested_main_height
            && self.tiled_max_columns == other.tiled_max_columns
    }
}
