use std::borrow::Cow;

use rmux_proto::{RmuxError, SplitDirection};

use super::{panes::layout_for_split, Window};
use crate::layout::{LayoutDirection, LayoutTree};
use crate::{PaneGeometry, PaneId};

mod stack;

pub use stack::{FloatingPanePlacement, FloatingStack};

/// A placement whose position may still be missing on either axis.
///
/// The sizes are decided by the caller, because they never depend on where the
/// last float went. A missing position is filled by the window itself, which is
/// the only owner of that memory, at the moment it places the pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatingPaneRequest {
    x: Option<i32>,
    y: Option<i32>,
    cols: i32,
    rows: i32,
}

impl FloatingPaneRequest {
    /// Creates a request, refusing a non-positive size before anything is placed.
    pub fn new(x: Option<i32>, y: Option<i32>, cols: i32, rows: i32) -> Result<Self, RmuxError> {
        if cols < 1 || rows < 1 {
            return Err(RmuxError::Server(
                "floating pane dimensions must be positive".to_owned(),
            ));
        }
        Ok(Self { x, y, cols, rows })
    }

    /// The requested position on each axis, `None` where it was omitted.
    #[must_use]
    pub const fn position(self) -> (Option<i32>, Option<i32>) {
        (self.x, self.y)
    }

    /// The requested size.
    #[must_use]
    pub const fn size(self) -> (i32, i32) {
        (self.cols, self.rows)
    }
}

impl From<FloatingPaneRect> for FloatingPaneRequest {
    fn from(rect: FloatingPaneRect) -> Self {
        Self {
            x: Some(rect.x()),
            y: Some(rect.y()),
            cols: rect.cols(),
            rows: rect.rows(),
        }
    }
}

/// Signed content rectangle for a floating pane placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatingPaneRect {
    x: i32,
    y: i32,
    cols: i32,
    rows: i32,
}

impl FloatingPaneRect {
    /// Creates a signed floating content rectangle.
    ///
    /// Width and height must be positive. Coordinates may be negative because
    /// tmux-style floating placement can be offset outside the visible window.
    pub fn new(x: i32, y: i32, cols: i32, rows: i32) -> Result<Self, RmuxError> {
        if cols < 1 || rows < 1 {
            return Err(RmuxError::Server(
                "floating pane dimensions must be positive".to_owned(),
            ));
        }
        Ok(Self { x, y, cols, rows })
    }

    /// Returns the signed left content coordinate.
    #[must_use]
    pub const fn x(self) -> i32 {
        self.x
    }

    /// Returns the signed top content coordinate.
    #[must_use]
    pub const fn y(self) -> i32 {
        self.y
    }

    /// Returns the signed content width.
    #[must_use]
    pub const fn cols(self) -> i32 {
        self.cols
    }

    /// Returns the signed content height.
    #[must_use]
    pub const fn rows(self) -> i32 {
        self.rows
    }

    fn moved_to(self, x: i32, y: i32) -> Self {
        Self { x, y, ..self }
    }

    fn resized_to(self, cols: i32, rows: i32) -> Result<Self, RmuxError> {
        Self::new(self.x, self.y, cols, rows)
    }

    pub(super) fn pane_geometry(self) -> PaneGeometry {
        PaneGeometry::new(
            signed_to_u16(self.x),
            signed_to_u16(self.y),
            signed_to_u16(self.cols),
            signed_to_u16(self.rows),
        )
    }
}

impl Window {
    /// Returns the resolved floating pane stack in rear-to-front order.
    #[must_use]
    pub fn floating_stack(&self) -> FloatingStack {
        self.current_floating_stack().into_owned()
    }

    /// Converts a tiled pane into a frontmost floating pane.
    ///
    /// The pane's stable id and index are preserved. The pane is removed from
    /// the tiled layout tree only after all validation has succeeded.
    pub fn float_pane(&mut self, pane_index: u32, rect: FloatingPaneRect) -> Result<(), RmuxError> {
        self.reconcile_borrowed_pane_owners();
        let pane_id = self.require_pane_id(pane_index)?;
        if self.floating_stack.contains(pane_id) {
            return Err(RmuxError::Server(format!(
                "pane id {} is already floating",
                pane_id.as_u32()
            )));
        }
        let mut next_tree = self.layout_tree.clone().ok_or_else(|| {
            RmuxError::Server(format!("pane id {} is not tiled", pane_id.as_u32()))
        })?;
        if !next_tree.contains_leaf(pane_id) {
            return Err(RmuxError::Server(format!(
                "pane id {} is not tiled",
                pane_id.as_u32()
            )));
        }
        let next_tree = if next_tree.leaf_count() == 1 {
            None
        } else {
            let removed = next_tree.remove_leaf(pane_id);
            debug_assert!(removed, "validated tiled pane must be removable");
            Some(next_tree)
        };

        self.auto_unzoom();
        self.layout_tree = next_tree;
        self.floating_stack
            .push_frontmost(FloatingPanePlacement::new(pane_id, rect))?;
        self.apply_layout_tree();
        Ok(())
    }

    /// Converts a tiled pane using its canonical, non-zoomed layout rectangle.
    ///
    /// The rectangle is captured before removal from the tiled tree. This is
    /// deliberately distinct from `Pane::geometry`, which may be the projected
    /// full-window rectangle while the window is zoomed.
    pub fn float_pane_at_tiled_geometry(&mut self, pane_index: u32) -> Result<(), RmuxError> {
        self.reconcile_borrowed_pane_owners();
        let pane_id = self.require_pane_id(pane_index)?;
        let geometry = self
            .current_layout_tree()
            .and_then(|tree| tree.pane_geometry(pane_id))
            .ok_or_else(|| {
                RmuxError::Server(format!("pane id {} is not tiled", pane_id.as_u32()))
            })?;
        let rect = FloatingPaneRect::new(
            i32::from(geometry.x()),
            i32::from(geometry.y()),
            i32::from(geometry.cols()),
            i32::from(geometry.rows()),
        )?;
        self.float_pane(pane_index, rect)
    }

    /// Converts a floating pane back into the tiled layout tree.
    ///
    /// If no tiled panes remain, `target_pane_index` must be `None` and the
    /// pane becomes the single tiled leaf. Otherwise the floating pane is
    /// inserted by splitting the requested tiled target.
    pub fn tile_floating_pane(
        &mut self,
        pane_index: u32,
        target_pane_index: Option<u32>,
        direction: SplitDirection,
        before: bool,
    ) -> Result<(), RmuxError> {
        self.reconcile_borrowed_pane_owners();
        let pane_id = self.require_pane_id(pane_index)?;
        if !self.floating_stack.contains(pane_id) {
            return Err(RmuxError::Server(format!(
                "pane id {} is not floating",
                pane_id.as_u32()
            )));
        }

        let mut next_stack = self.floating_stack.clone();
        let removed = next_stack.remove(pane_id).ok_or_else(|| {
            RmuxError::Server(format!("pane id {} is not floating", pane_id.as_u32()))
        })?;
        let next_tree = if self.tiled_pane_ids().is_empty() {
            if target_pane_index.is_some() {
                return Err(RmuxError::Server(
                    "cannot target a tiled pane when no tiled panes remain".to_owned(),
                ));
            }
            Some(LayoutTree::single(self.size, removed.pane_id))
        } else {
            let target_id = self.require_tiled_target_id(target_pane_index)?;
            let mut tree = self.layout_tree.clone().ok_or_else(|| {
                RmuxError::Server("cannot tile into a missing layout tree".to_owned())
            })?;
            if !tree.split_leaf(
                target_id,
                pane_id,
                LayoutDirection::from_split_direction(direction),
                before,
            ) {
                return Err(RmuxError::Server(format!(
                    "cannot split target pane id {}",
                    target_id.as_u32()
                )));
            }
            Some(tree)
        };

        self.auto_unzoom();
        self.layout = layout_for_split(direction);
        self.layout_tree = next_tree;
        self.floating_stack = next_stack;
        self.apply_layout_tree();
        Ok(())
    }

    /// Moves a floating pane to an absolute signed content coordinate.
    pub fn move_floating_pane(&mut self, pane_index: u32, x: i32, y: i32) -> Result<(), RmuxError> {
        let pane_id = self.require_floating_pane_id(pane_index)?;
        let rect = self
            .floating_stack
            .placement(pane_id)
            .expect("validated floating pane")
            .rect()
            .moved_to(x, y);
        self.auto_unzoom();
        let updated = self.floating_stack.set_rect(pane_id, rect);
        debug_assert!(updated, "validated floating pane must be movable");
        self.apply_floating_stack_to_panes();
        Ok(())
    }

    /// Changes selected absolute coordinates in the canonical floating placement.
    ///
    /// Unlike interactive movement, this preserves zoom. While zoomed, effective
    /// pane geometry stays untouched until the saved layout is restored.
    pub fn translate_floating_pane(
        &mut self,
        pane_index: u32,
        x: Option<i32>,
        y: Option<i32>,
    ) -> Result<bool, RmuxError> {
        let pane_id = self.require_floating_pane_id(pane_index)?;
        let current = self
            .floating_stack
            .placement(pane_id)
            .expect("validated floating pane")
            .rect();
        let moved = current.moved_to(x.unwrap_or(current.x()), y.unwrap_or(current.y()));
        if moved == current {
            return Ok(false);
        }
        let updated = self.floating_stack.set_rect(pane_id, moved);
        debug_assert!(updated, "validated floating pane must be translatable");
        if !self.is_zoomed() {
            self.apply_floating_stack_to_panes();
        }
        Ok(true)
    }

    /// Resizes a floating pane to an absolute signed content size.
    pub fn resize_floating_pane(
        &mut self,
        pane_index: u32,
        cols: i32,
        rows: i32,
    ) -> Result<(), RmuxError> {
        let pane_id = self.require_floating_pane_id(pane_index)?;
        let rect = self
            .floating_stack
            .placement(pane_id)
            .expect("validated floating pane")
            .rect()
            .resized_to(cols, rows)?;
        self.auto_unzoom();
        let updated = self.floating_stack.set_rect(pane_id, rect);
        debug_assert!(updated, "validated floating pane must be resizable");
        self.apply_floating_stack_to_panes();
        Ok(())
    }

    /// Sets a floating pane's visibility without changing its z-order.
    pub fn set_floating_pane_visible(
        &mut self,
        pane_index: u32,
        visible: bool,
    ) -> Result<(), RmuxError> {
        let pane_id = self.require_floating_pane_id(pane_index)?;
        let updated = self.floating_stack.set_visible(pane_id, visible);
        debug_assert!(updated, "validated floating pane visibility must update");
        Ok(())
    }

    /// Raises a floating pane to the front without changing active selection.
    pub fn raise_floating_pane(&mut self, pane_index: u32) -> Result<(), RmuxError> {
        let pane_id = self.require_floating_pane_id(pane_index)?;
        let raised = self.floating_stack.raise(pane_id);
        debug_assert!(raised, "validated floating pane must be raisable");
        Ok(())
    }

    /// Selects a floating pane and raises it to the front of the stack.
    pub fn select_floating_pane(&mut self, pane_index: u32) -> Result<(), RmuxError> {
        let _ = self.require_floating_pane_id(pane_index)?;
        let selected = self.select_pane(pane_index);
        debug_assert!(selected, "validated floating pane must be selectable");
        Ok(())
    }

    pub(super) fn current_floating_stack(&self) -> Cow<'_, FloatingStack> {
        let replacements = self.borrowed_pane_replacements();
        if replacements.is_empty() {
            Cow::Borrowed(&self.floating_stack)
        } else {
            let mut stack = self.floating_stack.clone();
            stack.remap_ids(&replacements);
            Cow::Owned(stack)
        }
    }

    pub(super) fn apply_floating_stack_to_panes(&mut self) {
        let placements = self.floating_stack.placements.clone();
        for placement in placements {
            let Some(pane) = self
                .panes
                .iter_mut()
                .find(|pane| pane.id() == placement.pane_id)
            else {
                debug_assert!(false, "floating placement must reference a live pane");
                continue;
            };
            pane.set_geometry(placement.rect.pane_geometry());
        }
    }

    pub(crate) fn is_floating_pane_id(&self, pane_id: PaneId) -> bool {
        self.current_floating_stack().contains(pane_id)
    }

    pub(super) fn is_tiled_pane_id(&self, pane_id: PaneId) -> bool {
        self.current_layout_tree()
            .is_some_and(|tree| tree.contains_leaf(pane_id))
    }

    pub(super) fn tiled_pane_ids(&self) -> Vec<PaneId> {
        self.panes
            .iter()
            .map(crate::Pane::id)
            .filter(|pane_id| !self.is_floating_pane_id(*pane_id))
            .collect()
    }

    pub(super) fn remove_floating_pane_id(&mut self, pane_id: PaneId) -> bool {
        let removed = self.floating_stack.remove(pane_id).is_some();
        self.sync_resize_topology();
        removed
    }

    pub(super) fn remap_floating_pane_ids(&mut self, replacements: &[(PaneId, PaneId)]) {
        self.floating_stack.remap_ids(replacements);
        for id in &mut self.resize_topology.floating_attachment_order {
            if let Some((_, new)) = replacements.iter().find(|(old, _)| old == id) {
                *id = *new;
            }
        }
    }

    pub(super) fn require_pane_id(&self, pane_index: u32) -> Result<PaneId, RmuxError> {
        self.pane_id(pane_index).ok_or_else(|| {
            RmuxError::Server(format!(
                "pane index {pane_index} does not exist in window {}",
                self.id
            ))
        })
    }

    fn require_floating_pane_id(&mut self, pane_index: u32) -> Result<PaneId, RmuxError> {
        self.reconcile_borrowed_pane_owners();
        let pane_id = self.require_pane_id(pane_index)?;
        if !self.floating_stack.contains(pane_id) {
            return Err(RmuxError::Server(format!(
                "pane id {} is not floating",
                pane_id.as_u32()
            )));
        }
        Ok(pane_id)
    }

    fn require_tiled_target_id(&self, target_pane_index: Option<u32>) -> Result<PaneId, RmuxError> {
        let target_pane_index = target_pane_index
            .ok_or_else(|| RmuxError::Server("target tiled pane is required".to_owned()))?;
        let target_id = self.require_pane_id(target_pane_index)?;
        if !self.is_tiled_pane_id(target_id) {
            return Err(RmuxError::Server(format!(
                "pane id {} is not tiled",
                target_id.as_u32()
            )));
        }
        Ok(target_id)
    }
}

fn signed_to_u16(value: i32) -> u16 {
    value.clamp(0, i32::from(u16::MAX)) as u16
}
