use super::{panes::layout_for_split, Window};
use crate::layout::{LayoutDirection, LayoutTree};
use crate::{Pane, PaneId};
use rmux_proto::{RmuxError, SplitDirection};

impl Window {
    pub(crate) fn move_pane_by_splitting_target(
        &mut self,
        source_position: usize,
        target_position: usize,
        final_insert_position: usize,
        direction: SplitDirection,
        insert_before_target: bool,
    ) -> Result<PaneId, RmuxError> {
        self.reconcile_borrowed_pane_owners();
        let pane_count = self.panes.len();
        if source_position >= pane_count {
            return Err(RmuxError::Server(format!(
                "cannot move missing pane at position {source_position}"
            )));
        }
        if target_position >= pane_count {
            return Err(RmuxError::Server(format!(
                "cannot split missing target pane at position {target_position}"
            )));
        }
        if self.is_floating_pane_id(self.panes[source_position].id())
            || self.is_floating_pane_id(self.panes[target_position].id())
        {
            return Err(RmuxError::Server(
                "cannot move a pane by splitting a floating placement".to_owned(),
            ));
        }
        if final_insert_position > pane_count.saturating_sub(1) {
            return Err(RmuxError::Server(format!(
                "cannot insert moved pane at position {final_insert_position} in a {}-pane window",
                pane_count.saturating_sub(1)
            )));
        }

        let mut tree = self.layout_tree.clone().ok_or_else(|| {
            RmuxError::Server("cannot move pane without a layout tree".to_owned())
        })?;
        if !tree.move_leaf(
            self.panes[source_position].id(),
            self.panes[target_position].id(),
            LayoutDirection::from_split_direction(direction),
            insert_before_target,
        ) {
            return Err(RmuxError::Server(format!(
                "cannot split target pane at position {target_position}"
            )));
        }

        self.auto_unzoom();
        self.layout = layout_for_split(direction);
        self.layout_tree = Some(tree);
        let moved_pane = self.panes.remove(source_position);
        let moved_pane_id = moved_pane.id();
        self.panes.insert(final_insert_position, moved_pane);
        self.apply_layout_tree();
        Ok(moved_pane_id)
    }

    /// Moves a floating pane of this window into the tiled layout by splitting
    /// a tiled target, keeping the pane identity, index order and process.
    ///
    /// The floating source is not a layout leaf, so this splits the target and
    /// drops the single floating placement instead of relocating a leaf. Every
    /// other placement keeps its rectangle and its stack order. Validation
    /// completes on clones so a refusal leaves the window, its zoom and its
    /// floating stack untouched.
    pub(crate) fn move_floating_pane_by_splitting_target(
        &mut self,
        source_position: usize,
        target_position: usize,
        final_insert_position: usize,
        direction: SplitDirection,
        insert_before_target: bool,
    ) -> Result<PaneId, RmuxError> {
        self.reconcile_borrowed_pane_owners();
        let pane_count = self.panes.len();
        if source_position >= pane_count {
            return Err(RmuxError::Server(format!(
                "cannot move missing pane at position {source_position}"
            )));
        }
        if target_position >= pane_count {
            return Err(RmuxError::Server(format!(
                "cannot split missing target pane at position {target_position}"
            )));
        }
        let source_pane_id = self.panes[source_position].id();
        let target_pane_id = self.panes[target_position].id();
        if !self.is_floating_pane_id(source_pane_id) {
            return Err(RmuxError::Server(format!(
                "pane id {} is not floating",
                source_pane_id.as_u32()
            )));
        }
        if self.is_floating_pane_id(target_pane_id) {
            return Err(RmuxError::Server(
                "cannot move a pane by splitting a floating placement".to_owned(),
            ));
        }
        if final_insert_position > pane_count.saturating_sub(1) {
            return Err(RmuxError::Server(format!(
                "cannot insert moved pane at position {final_insert_position} in a {}-pane window",
                pane_count.saturating_sub(1)
            )));
        }

        let mut tree = self.layout_tree.clone().ok_or_else(|| {
            RmuxError::Server("cannot move pane without a layout tree".to_owned())
        })?;
        if !tree.split_leaf(
            target_pane_id,
            source_pane_id,
            LayoutDirection::from_split_direction(direction),
            insert_before_target,
        ) {
            return Err(RmuxError::Server(format!(
                "cannot split target pane at position {target_position}"
            )));
        }
        let mut floating_stack = self.floating_stack.clone();
        if floating_stack.remove(source_pane_id).is_none() {
            // Fail closed: a pane left in both registries would own a layout
            // cell and a placement at once, which the scene and the snapshot
            // both reject. Nothing is committed yet, so this refusal is free.
            return Err(RmuxError::Server(format!(
                "pane id {} is not floating",
                source_pane_id.as_u32()
            )));
        }

        self.auto_unzoom();
        self.layout = layout_for_split(direction);
        self.layout_tree = Some(tree);
        self.floating_stack = floating_stack;
        let moved_pane = self.panes.remove(source_position);
        self.panes.insert(final_insert_position, moved_pane);
        self.apply_layout_tree();
        Ok(source_pane_id)
    }

    pub(crate) fn insert_pane_full_size(
        &mut self,
        pane: Pane,
        direction: SplitDirection,
        insert_before_target: bool,
    ) -> Result<(), RmuxError> {
        self.reconcile_borrowed_pane_owners();
        self.ensure_accepts_pane(&pane, None)?;
        self.auto_unzoom();
        self.layout = layout_for_split(direction);
        self.bump_next_pane_index(pane.index());

        let inserted_id = pane.id();
        if insert_before_target {
            self.panes.insert(0, pane);
        } else {
            self.panes.push(pane);
        }

        if self.layout_tree.is_none() {
            self.layout_tree = Some(LayoutTree::single(self.size, inserted_id));
            self.apply_layout_tree();
            return Ok(());
        }

        let split = self.layout_tree.as_mut().is_some_and(|tree| {
            tree.split_root(
                inserted_id,
                LayoutDirection::from_split_direction(direction),
                insert_before_target,
            )
        });
        if !split {
            self.rebuild_named_layout_tree(self.layout);
        } else {
            self.apply_layout_tree();
        }
        Ok(())
    }

    pub(crate) fn replace_pane_for_swap(
        &mut self,
        pane_index: u32,
        pane: Pane,
    ) -> Result<(), RmuxError> {
        self.reconcile_borrowed_pane_owners();
        let position = self.pane_position(pane_index).ok_or_else(|| {
            RmuxError::Server(format!(
                "cannot replace missing pane index {pane_index} in window {}",
                self.id
            ))
        })?;
        for (existing_position, existing) in self.panes.iter().enumerate() {
            if existing_position != position && existing.id() == pane.id() {
                return Err(RmuxError::Server(format!(
                    "pane id {} already exists in window {}",
                    pane.id().as_u32(),
                    self.id
                )));
            }
        }
        self.bump_next_pane_index(pane.index());
        if self.is_floating_pane_id(self.panes[position].id()) {
            self.remap_floating_pane_ids(&[(self.panes[position].id(), pane.id())]);
        } else if let Some(tree) = &mut self.layout_tree {
            tree.remap_ids(&[(self.panes[position].id(), pane.id())]);
        }
        self.panes[position] = pane;
        self.apply_layout_tree();
        Ok(())
    }

    pub(crate) fn swap_panes(&mut self, source_pane_index: u32, target_pane_index: u32) -> bool {
        self.reconcile_borrowed_pane_owners();
        let Some(source_position) = self.pane_position(source_pane_index) else {
            return false;
        };
        let Some(target_position) = self.pane_position(target_pane_index) else {
            return false;
        };
        if source_position == target_position {
            return true;
        }

        self.auto_unzoom();
        let active_pane_id = self
            .active_pane()
            .expect("active pane must exist before pane swap")
            .id();
        let last_pane_id = self
            .last_pane
            .and_then(|pane_index| self.pane(pane_index).map(Pane::id));
        if let Some(tree) = &mut self.layout_tree {
            let source = self.panes[source_position].id();
            let target = self.panes[target_position].id();
            tree.remap_ids(&[(source, target), (target, source)]);
        }
        let source = self.panes[source_position].id();
        let target = self.panes[target_position].id();
        self.remap_floating_pane_ids(&[(source, target), (target, source)]);
        self.panes.swap(source_position, target_position);
        self.apply_layout_tree();
        self.renumber_panes_by_position(active_pane_id, last_pane_id);
        true
    }
}
