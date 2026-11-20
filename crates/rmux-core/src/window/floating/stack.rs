//! Floating pane placements and their rear-to-front stack.

use rmux_proto::RmuxError;

use super::FloatingPaneRect;
use crate::PaneId;

/// One floating pane placement entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatingPanePlacement {
    pub(super) pane_id: PaneId,
    pub(super) rect: FloatingPaneRect,
    visible: bool,
}

impl FloatingPanePlacement {
    /// Creates a visible floating placement for `pane_id`.
    #[must_use]
    pub const fn new(pane_id: PaneId, rect: FloatingPaneRect) -> Self {
        Self::new_with_visibility(pane_id, rect, true)
    }

    /// Creates a floating placement for `pane_id` with explicit visibility.
    #[must_use]
    pub const fn new_with_visibility(
        pane_id: PaneId,
        rect: FloatingPaneRect,
        visible: bool,
    ) -> Self {
        Self {
            pane_id,
            rect,
            visible,
        }
    }

    /// Returns the placed pane identity.
    #[must_use]
    pub const fn pane_id(self) -> PaneId {
        self.pane_id
    }

    /// Returns the signed content rectangle.
    #[must_use]
    pub const fn rect(self) -> FloatingPaneRect {
        self.rect
    }

    /// Returns whether this floating pane is visible.
    #[must_use]
    pub const fn visible(self) -> bool {
        self.visible
    }
}

/// Ordered stack of floating pane placements from rear to front.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FloatingStack {
    pub(super) placements: Vec<FloatingPanePlacement>,
}

impl FloatingStack {
    /// Returns whether the stack is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }

    /// Returns the number of floating placements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.placements.len()
    }

    /// Returns floating placements in rear-to-front order.
    #[must_use]
    pub fn placements(&self) -> &[FloatingPanePlacement] {
        &self.placements
    }

    /// Returns the placement for `pane_id`, when it is floating.
    #[must_use]
    pub fn placement(&self, pane_id: PaneId) -> Option<FloatingPanePlacement> {
        self.placements
            .iter()
            .copied()
            .find(|placement| placement.pane_id == pane_id)
    }

    /// Returns whether `pane_id` has a floating placement.
    #[must_use]
    pub fn contains(&self, pane_id: PaneId) -> bool {
        self.placement(pane_id).is_some()
    }

    pub(crate) fn push_frontmost(
        &mut self,
        placement: FloatingPanePlacement,
    ) -> Result<(), RmuxError> {
        if self.contains(placement.pane_id) {
            return Err(RmuxError::Server(format!(
                "pane id {} is already floating",
                placement.pane_id.as_u32()
            )));
        }
        self.placements.push(placement);
        Ok(())
    }

    pub(crate) fn from_placements(
        placements: Vec<FloatingPanePlacement>,
    ) -> Result<Self, RmuxError> {
        let mut stack = Self::default();
        for placement in placements {
            stack.push_frontmost(placement)?;
        }
        Ok(stack)
    }

    pub(crate) fn remove(&mut self, pane_id: PaneId) -> Option<FloatingPanePlacement> {
        let position = self.position(pane_id)?;
        Some(self.placements.remove(position))
    }

    pub(crate) fn raise(&mut self, pane_id: PaneId) -> bool {
        let Some(position) = self.position(pane_id) else {
            return false;
        };
        if position + 1 == self.placements.len() {
            return true;
        }
        let placement = self.placements.remove(position);
        self.placements.push(placement);
        true
    }

    pub(crate) fn set_rect(&mut self, pane_id: PaneId, rect: FloatingPaneRect) -> bool {
        let Some(position) = self.position(pane_id) else {
            return false;
        };
        self.placements[position].rect = rect;
        true
    }

    pub(crate) fn set_visible(&mut self, pane_id: PaneId, visible: bool) -> bool {
        let Some(position) = self.position(pane_id) else {
            return false;
        };
        self.placements[position].visible = visible;
        true
    }

    pub(crate) fn remap_ids(&mut self, replacements: &[(PaneId, PaneId)]) {
        for placement in &mut self.placements {
            if let Some((_, new)) = replacements
                .iter()
                .find(|(old, _)| *old == placement.pane_id)
            {
                placement.pane_id = *new;
            }
        }
    }

    pub(crate) fn retain_pane_ids(&mut self, pane_ids: &[PaneId]) {
        self.placements
            .retain(|placement| pane_ids.contains(&placement.pane_id));
    }

    fn position(&self, pane_id: PaneId) -> Option<usize> {
        self.placements
            .iter()
            .position(|placement| placement.pane_id == pane_id)
    }
}
