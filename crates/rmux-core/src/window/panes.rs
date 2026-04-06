use rmux_proto::{LayoutName, RmuxError, SelectPaneDirection, SplitDirection, TerminalSize};

use super::Window;
use crate::layout::{minimum_axis_size_for_panes, LayoutDirection, LayoutTree};
use crate::{Pane, PaneGeometry, PaneId};

impl Window {
    pub(crate) fn pane_position(&self, pane_index: u32) -> Option<usize> {
        self.panes
            .iter()
            .position(|pane| pane.index() == pane_index)
    }

    pub(crate) fn can_split_pane(&self, pane_index: u32, direction: SplitDirection) -> bool {
        let Some(pane) = self.pane(pane_index) else {
            return false;
        };
        let geometry = pane.geometry();
        let axis_size = match direction {
            SplitDirection::Vertical => geometry.cols(),
            SplitDirection::Horizontal => geometry.rows(),
        };
        u32::from(axis_size) >= minimum_axis_size_for_panes(2)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn split_after_position(&mut self, position: usize) -> u32 {
        self.split_after_position_with_id(position, self.next_generated_pane_id())
    }

    pub(crate) fn split_after_position_with_id(&mut self, position: usize, pane_id: PaneId) -> u32 {
        self.split_after_position_with_id_and_direction(position, pane_id, SplitDirection::Vertical)
    }

    pub(crate) fn split_after_position_with_id_and_direction(
        &mut self,
        position: usize,
        pane_id: PaneId,
        direction: SplitDirection,
    ) -> u32 {
        assert!(
            position < self.panes.len(),
            "split target position must reference an existing pane"
        );
        assert!(
            self.panes.iter().all(|pane| pane.id() != pane_id),
            "pane id must be unique within a window"
        );
        self.auto_unzoom();
        self.layout = layout_for_split(direction);
        let previous_active_pane_id = self.active_pane().map(Pane::id);
        let new_index = self.allocate_pane_index();
        let insert_at = position + 1;
        self.panes.insert(
            insert_at,
            Pane::new_with_id(pane_id, new_index, PaneGeometry::new(0, 0, 0, 0)),
        );
        let split = self.layout_tree.as_mut().is_some_and(|tree| {
            tree.split_leaf(
                position,
                LayoutDirection::from_split_direction(direction),
                false,
            )
        });
        if !split {
            self.rebuild_named_layout_tree(self.layout);
        } else {
            self.apply_layout_tree();
        }
        self.renumber_panes_by_position(pane_id, previous_active_pane_id);
        self.mark_pane_active(self.active_pane);
        self.active_pane
    }

    pub(crate) fn extract_pane(&mut self, pane_index: u32) -> Option<Pane> {
        let position = self.pane_position(pane_index)?;
        self.auto_unzoom();
        let previous_active_pane_id = self.active_pane().map(Pane::id)?;
        let previous_last_pane_id = self
            .last_pane
            .and_then(|last_pane| self.pane(last_pane).map(Pane::id));
        let next_active_pane_id = if self.active_pane == pane_index && self.panes.len() > 1 {
            Some(self.next_active_pane_after_removal(position, pane_index))
        } else {
            Some(previous_active_pane_id)
        };
        let removed = self.panes.remove(position);
        let next_last_pane_id = if removed.index() == self.active_pane {
            None
        } else {
            previous_last_pane_id.filter(|pane_id| *pane_id != removed.id())
        };

        if self.panes.is_empty() {
            self.layout_tree = None;
            self.custom_layout = false;
        } else {
            let removed = self
                .layout_tree
                .as_mut()
                .is_some_and(|tree| tree.remove_leaf(position));
            if !removed {
                self.rebuild_named_layout_tree(self.layout);
            } else {
                self.apply_layout_tree();
            }
        }
        if self.panes.len() == 1 {
            self.renumber_single_pane_to_zero();
        } else if !self.panes.is_empty() {
            self.renumber_panes_by_position(
                next_active_pane_id.expect("multi-pane removal should preserve an active pane id"),
                next_last_pane_id,
            );
        } else {
            self.active_pane = removed.index();
            self.last_pane = None;
        }
        Some(removed)
    }

    pub(crate) fn remove_pane(&mut self, pane_index: u32) -> Option<Pane> {
        if self.panes.len() <= 1 {
            return None;
        }

        self.extract_pane(pane_index)
    }

    pub(crate) fn remove_other_panes(&mut self, pane_index: u32) -> Option<Vec<PaneId>> {
        let keep_pane_id = self.pane(pane_index)?.id();
        let removed_pane_ids = self
            .panes
            .iter()
            .filter(|pane| pane.id() != keep_pane_id)
            .map(Pane::id)
            .collect::<Vec<_>>();
        if removed_pane_ids.is_empty() {
            self.select_pane(pane_index);
            return Some(removed_pane_ids);
        }

        self.auto_unzoom();
        self.panes.retain(|pane| pane.id() == keep_pane_id);
        self.renumber_single_pane_to_zero();
        Some(removed_pane_ids)
    }

    pub(crate) fn select_pane(&mut self, pane_index: u32) -> bool {
        if self.pane(pane_index).is_none() {
            return false;
        }

        if self.active_pane != pane_index {
            self.auto_unzoom();
            self.last_pane = Some(self.active_pane);
            self.active_pane = pane_index;
            self.mark_pane_active(pane_index);
        }

        true
    }

    pub(crate) fn select_pane_by_id(&mut self, pane_id: PaneId) -> bool {
        self.pane_index_for_id(pane_id)
            .is_some_and(|pane_index| self.select_pane(pane_index))
    }

    pub(crate) fn select_adjacent_pane(
        &mut self,
        pane_index: u32,
        direction: SelectPaneDirection,
    ) -> Option<u32> {
        let source = self.pane(pane_index)?.geometry();
        let mut candidate = None;
        for pane in self.panes.iter().filter(|pane| pane.index() != pane_index) {
            if adjacent_pane_score(source, pane.geometry(), direction, self.size).is_none() {
                continue;
            }
            match candidate {
                Some((best_active_point, _)) if pane.active_point() <= best_active_point => {}
                _ => candidate = Some((pane.active_point(), pane.index())),
            }
        }
        let candidate = candidate.map(|(_, index)| index);

        if let Some(candidate) = candidate {
            let selected = self.select_pane(candidate);
            debug_assert!(selected, "adjacent pane candidate must be selectable");
            Some(candidate)
        } else {
            Some(self.active_pane)
        }
    }

    pub(crate) fn clear_last_pane_reference(&mut self, pane_index: u32) {
        if self.last_pane == Some(pane_index) {
            self.last_pane = None;
        }
    }

    pub(super) fn mark_pane_active(&mut self, pane_index: u32) {
        let next_active_point = self
            .panes
            .iter()
            .map(Pane::active_point)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        if let Some(pane) = self.pane_mut(pane_index) {
            pane.set_active_point(next_active_point);
        }
    }

    pub(crate) fn ensure_accepts_pane(
        &self,
        pane: &Pane,
        ignored_position: Option<usize>,
    ) -> Result<(), RmuxError> {
        for (position, existing) in self.panes.iter().enumerate() {
            if Some(position) == ignored_position {
                continue;
            }
            if existing.id() == pane.id() {
                return Err(RmuxError::Server(format!(
                    "pane id {} already exists in window {}",
                    pane.id().as_u32(),
                    self.id
                )));
            }
            if existing.index() == pane.index() {
                return Err(RmuxError::Server(format!(
                    "pane index {} already exists in window {}",
                    pane.index(),
                    self.id
                )));
            }
        }

        Ok(())
    }

    pub(crate) fn renumber_single_pane_to_zero(&mut self) {
        if self.panes.len() != 1 {
            return;
        }

        let pane_id = self.panes[0].id();
        self.panes[0] = Pane::new_with_id(
            pane_id,
            0,
            PaneGeometry::new(0, 0, self.size.cols, self.size.rows),
        );
        self.next_pane_index = 1;
        self.active_pane = 0;
        self.last_pane = None;
        self.layout = LayoutName::MainVertical;
        self.last_layout = None;
        self.layout_tree = Some(LayoutTree::single(self.size));
        self.custom_layout = false;
        self.old_layout = None;
        self.zoomed = false;
        self.zoom_restore_pending = false;
        self.requested_main_width = None;
        self.requested_main_height = None;
        self.recalculate_geometry();
    }

    fn next_active_pane_after_removal(&self, position: usize, removed_pane_index: u32) -> PaneId {
        if let Some(last_pane) = self
            .last_pane
            .filter(|last_pane| *last_pane != removed_pane_index)
        {
            if let Some(pane_id) = self.pane(last_pane).map(Pane::id) {
                return pane_id;
            }
        }

        if position > 0 {
            return self.panes[position - 1].id();
        }

        if position + 1 < self.panes.len() {
            return self.panes[position + 1].id();
        }

        unreachable!("removing one pane from a multi-pane window must leave a survivor");
    }

    pub(super) fn pane_index_for_id(&self, pane_id: PaneId) -> Option<u32> {
        self.panes
            .iter()
            .find(|pane| pane.id() == pane_id)
            .map(Pane::index)
    }

    pub(crate) fn renumber_panes_by_position(
        &mut self,
        active_pane_id: PaneId,
        last_pane_id: Option<PaneId>,
    ) {
        for (index, pane) in self.panes.iter_mut().enumerate() {
            pane.set_index(index as u32);
        }
        self.next_pane_index = self.panes.len() as u32;
        self.active_pane = self
            .pane_index_for_id(active_pane_id)
            .expect("active pane id must survive pane renumbering");
        self.last_pane = last_pane_id
            .and_then(|pane_id| self.pane_index_for_id(pane_id))
            .filter(|pane_index| *pane_index != self.active_pane);
    }

    fn allocate_pane_index(&mut self) -> u32 {
        let next_pane_index = self.next_pane_index;
        assert_ne!(next_pane_index, u32::MAX, "pane index space exhausted");
        self.next_pane_index = next_pane_index + 1;
        next_pane_index
    }

    pub(super) fn bump_next_pane_index(&mut self, pane_index: u32) {
        self.next_pane_index = self.next_pane_index.max(pane_index.saturating_add(1));
    }

    fn next_generated_pane_id(&self) -> PaneId {
        PaneId::new(
            self.panes
                .iter()
                .map(|pane| pane.id().as_u32())
                .max()
                .map_or(0, |id| id.saturating_add(1)),
        )
    }
}

pub(super) fn layout_for_split(direction: SplitDirection) -> LayoutName {
    match direction {
        SplitDirection::Vertical => LayoutName::MainVertical,
        SplitDirection::Horizontal => LayoutName::MainHorizontal,
    }
}

fn adjacent_pane_score(
    source: PaneGeometry,
    candidate: PaneGeometry,
    direction: SelectPaneDirection,
    window_size: TerminalSize,
) -> Option<(u32, u32)> {
    let source_left = u32::from(source.x());
    let source_cols = u32::from(source.cols());
    let source_top = u32::from(source.y());
    let source_rows = u32::from(source.rows());
    let source_center_x = source_left + u32::from(source.cols()) / 2;
    let source_center_y = source_top + u32::from(source.rows()) / 2;

    let candidate_left = u32::from(candidate.x());
    let candidate_cols = u32::from(candidate.cols());
    let candidate_top = u32::from(candidate.y());
    let candidate_rows = u32::from(candidate.rows());
    let candidate_center_x = candidate_left + u32::from(candidate.cols()) / 2;
    let candidate_center_y = candidate_top + u32::from(candidate.rows()) / 2;

    match direction {
        SelectPaneDirection::Right => {
            let mut edge = source_left + source_cols + 1;
            if edge >= u32::from(window_size.cols) {
                edge = 0;
            }
            candidate_left == edge
                && tmux_axis_overlap(source_top, source_rows, candidate_top, candidate_rows)
        }
        .then(|| (0, source_center_y.abs_diff(candidate_center_y))),
        SelectPaneDirection::Left => {
            let mut edge = source_left;
            if edge == 0 {
                edge = u32::from(window_size.cols) + 1;
            }
            candidate_left + candidate_cols + 1 == edge
                && tmux_axis_overlap(source_top, source_rows, candidate_top, candidate_rows)
        }
        .then(|| (0, source_center_y.abs_diff(candidate_center_y))),
        SelectPaneDirection::Down => {
            let mut edge = source_top + source_rows + 1;
            if edge >= u32::from(window_size.rows) {
                edge = 0;
            }
            candidate_top == edge
                && tmux_axis_overlap(source_left, source_cols, candidate_left, candidate_cols)
        }
        .then(|| (0, source_center_x.abs_diff(candidate_center_x))),
        SelectPaneDirection::Up => {
            let mut edge = source_top;
            if edge == 0 {
                edge = u32::from(window_size.rows) + 1;
            }
            candidate_top + candidate_rows + 1 == edge
                && tmux_axis_overlap(source_left, source_cols, candidate_left, candidate_cols)
        }
        .then(|| (0, source_center_x.abs_diff(candidate_center_x))),
    }
}

fn tmux_axis_overlap(
    source_start: u32,
    source_size: u32,
    candidate_start: u32,
    candidate_size: u32,
) -> bool {
    let source_end = source_start + source_size;
    let candidate_end = candidate_start + candidate_size.saturating_sub(1);

    (candidate_start < source_start && candidate_end > source_end)
        || (candidate_start >= source_start && candidate_start <= source_end)
        || (candidate_end >= source_start && candidate_end <= source_end)
}
