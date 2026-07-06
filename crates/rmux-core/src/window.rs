use std::time::{SystemTime, UNIX_EPOCH};

use rmux_proto::{LayoutName, RmuxError, RotateWindowDirection, SplitDirection, TerminalSize};

use crate::layout::{LayoutDirection, LayoutTree};
use crate::{Pane, PaneGeometry, PaneId, WindowId};

/// Runtime alert flag bitset shared by window queue state and winlink-visible state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AlertFlags(u8);

impl AlertFlags {
    /// Returns an empty alert bitset.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Returns whether all bits in `other` are present.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns whether any bit in `other` is present.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    /// Returns whether no alert bits are set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns the union of `self` and `other`.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Adds the bits from `other`.
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Clears the bits from `other`.
    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

/// Window alert queue bit for bells.
pub const WINDOW_BELL: AlertFlags = AlertFlags(0x1);
/// Window alert queue bit for activity.
pub const WINDOW_ACTIVITY: AlertFlags = AlertFlags(0x2);
/// Window alert queue bit for silence.
pub const WINDOW_SILENCE: AlertFlags = AlertFlags(0x4);
/// Combined window alert queue bits.
pub const WINDOW_ALERTFLAGS: AlertFlags =
    AlertFlags(WINDOW_BELL.0 | WINDOW_ACTIVITY.0 | WINDOW_SILENCE.0);

/// Persistent winlink alert bit for bells.
pub const WINLINK_BELL: AlertFlags = AlertFlags(0x1);
/// Persistent winlink alert bit for activity.
pub const WINLINK_ACTIVITY: AlertFlags = AlertFlags(0x2);
/// Persistent winlink alert bit for silence.
pub const WINLINK_SILENCE: AlertFlags = AlertFlags(0x4);
/// Combined persistent winlink alert bits.
pub const WINLINK_ALERTFLAGS: AlertFlags =
    AlertFlags(WINLINK_BELL.0 | WINLINK_ACTIVITY.0 | WINLINK_SILENCE.0);

/// A session-owned window whose pane order is independent from pane indices.
///
/// Pane order is preserved separately from pane indices so a split can insert a
/// new pane immediately after its split target while still assigning the next
/// sequential pane index. Stable pane IDs remain independent from those
/// window-local display indices, which may contain gaps after deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    id: WindowId,
    panes: Vec<Pane>,
    next_pane_index: u32,
    active_pane: u32,
    last_pane: Option<u32>,
    layout: LayoutName,
    last_layout: Option<LayoutName>,
    layout_tree: Option<LayoutTree>,
    custom_layout: bool,
    old_layout: Option<String>,
    size: TerminalSize,
    name: Option<String>,
    automatic_rename: bool,
    zoomed: bool,
    zoom_restore_pending: bool,
    alert_flags: AlertFlags,
    alerts_queued: bool,
    created_at: i64,
    activity_at: i64,
    // `resize-pane -x` sets an explicit main-pane width; otherwise layout derives it.
    requested_main_width: Option<u16>,
    // `resize-pane -y` sets an explicit main-pane height; otherwise layout derives it.
    requested_main_height: Option<u16>,
}

#[path = "window/layout_cycle.rs"]
mod layout_cycle;
#[path = "window/layout_ops.rs"]
mod layout_ops;
#[path = "window/panes.rs"]
mod panes;
#[path = "window/zoom.rs"]
mod zoom;

use panes::layout_for_split;

impl Window {
    /// Creates the single V1 window with its initial pane.
    #[must_use]
    pub fn new(size: TerminalSize) -> Self {
        Self::new_with_initial_pane(size, PaneId::new(0), WindowId::new(0))
    }

    pub(crate) fn new_with_initial_pane(size: TerminalSize, pane_id: PaneId, id: WindowId) -> Self {
        let now = current_unix_timestamp();
        let mut window = Self {
            id,
            panes: vec![Pane::new_with_id(
                pane_id,
                0,
                PaneGeometry::new(0, 0, size.cols, size.rows),
            )],
            next_pane_index: 1,
            active_pane: 0,
            last_pane: None,
            layout: LayoutName::MainVertical,
            last_layout: None,
            layout_tree: Some(LayoutTree::single(size)),
            custom_layout: false,
            old_layout: None,
            size,
            name: None,
            automatic_rename: true,
            zoomed: false,
            zoom_restore_pending: false,
            alert_flags: AlertFlags::empty(),
            alerts_queued: false,
            created_at: now,
            activity_at: now,
            requested_main_width: None,
            requested_main_height: None,
        };
        window.recalculate_geometry();
        window
    }

    /// Returns the stable internal window identity.
    #[must_use]
    pub const fn id(&self) -> WindowId {
        self.id
    }

    /// Returns the window creation timestamp as Unix seconds.
    #[must_use]
    pub const fn created_at(&self) -> i64 {
        self.created_at
    }

    /// Returns the last window activity timestamp as Unix seconds.
    #[must_use]
    pub const fn activity_at(&self) -> i64 {
        self.activity_at
    }

    /// Records output activity for a specific pane in this window.
    pub fn touch_activity_for_pane(&mut self, pane_index: u32) -> bool {
        let Some(position) = self
            .panes
            .iter()
            .position(|pane| pane.index() == pane_index)
        else {
            return false;
        };
        self.activity_at = current_unix_timestamp();
        self.panes[position].touch_activity();
        true
    }

    /// Returns the panes in window order.
    #[must_use]
    pub fn panes(&self) -> &[Pane] {
        &self.panes
    }

    /// Returns the pane with the given stable pane index.
    #[must_use]
    pub fn pane(&self, pane_index: u32) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.index() == pane_index)
    }

    /// Returns a mutable pane reference for the given stable pane index.
    #[must_use]
    pub fn pane_mut(&mut self, pane_index: u32) -> Option<&mut Pane> {
        self.panes
            .iter_mut()
            .find(|pane| pane.index() == pane_index)
    }

    /// Returns the active pane index owned by the window.
    #[must_use]
    pub const fn active_pane_index(&self) -> u32 {
        self.active_pane
    }

    /// Returns the previously active pane index when one exists.
    #[must_use]
    pub const fn last_pane_index(&self) -> Option<u32> {
        self.last_pane
    }

    /// Returns the active pane when the window invariant is satisfied.
    #[must_use]
    pub fn active_pane(&self) -> Option<&Pane> {
        self.pane(self.active_pane)
    }

    /// Returns the stable internal pane identity for a display index.
    #[must_use]
    pub fn pane_id(&self, pane_index: u32) -> Option<PaneId> {
        self.pane(pane_index).map(Pane::id)
    }

    /// Returns the last selected named layout for the window.
    #[must_use]
    pub const fn layout(&self) -> LayoutName {
        self.layout
    }

    /// Returns the terminal size currently backing the window.
    #[must_use]
    pub const fn size(&self) -> TerminalSize {
        self.size
    }

    /// Returns the tmux-compatible serialized layout tree for the window.
    #[must_use]
    pub fn layout_dump(&self) -> String {
        self.layout_tree
            .as_ref()
            .map_or_else(String::new, |tree| tree.dump(&self.panes))
    }

    /// Saves the current serialized layout as the tmux-compatible old layout.
    pub fn save_old_layout(&mut self) {
        self.old_layout = Some(self.layout_dump());
    }

    /// Returns the previously saved serialized layout when one exists.
    #[must_use]
    pub fn old_layout(&self) -> Option<&str> {
        self.old_layout.as_deref()
    }

    /// Returns the explicit user-supplied window name when one exists.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns whether automatic renaming remains enabled for the window.
    #[must_use]
    pub const fn automatic_rename(&self) -> bool {
        self.automatic_rename
    }

    /// Returns any queued runtime alert flags for the window.
    #[must_use]
    pub const fn alert_flags(&self) -> AlertFlags {
        self.alert_flags
    }

    /// Returns whether alert processing is already queued for this window.
    #[must_use]
    pub const fn alerts_queued(&self) -> bool {
        self.alerts_queued
    }

    /// Returns the number of panes in the window.
    #[must_use]
    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// Queues alert flags for later server-side processing.
    pub fn queue_alerts(&mut self, flags: AlertFlags) {
        self.alert_flags.insert(flags);
    }

    /// Drains and returns all queued alert flags.
    pub fn take_alert_flags(&mut self) -> AlertFlags {
        let flags = self.alert_flags;
        self.alert_flags = AlertFlags::empty();
        flags
    }

    /// Clears the provided queued alert flags.
    pub fn clear_alert_flags(&mut self, flags: AlertFlags) {
        self.alert_flags.remove(flags);
    }

    /// Sets whether alert processing is already queued.
    pub fn set_alerts_queued(&mut self, queued: bool) {
        self.alerts_queued = queued;
    }

    /// Resets this window to a single pane with a fresh pane ID, preserving the window id,
    /// name, and size. Returns the new pane ID.
    pub(crate) fn respawn(&mut self, pane_id: PaneId) -> PaneId {
        let size = self.size;
        self.panes = vec![Pane::new_with_id(
            pane_id,
            0,
            PaneGeometry::new(0, 0, size.cols, size.rows),
        )];
        self.next_pane_index = 1;
        self.active_pane = 0;
        self.last_pane = None;
        self.layout = LayoutName::MainVertical;
        self.last_layout = None;
        self.layout_tree = Some(LayoutTree::single(size));
        self.custom_layout = false;
        self.old_layout = None;
        self.automatic_rename = true;
        self.zoomed = false;
        self.zoom_restore_pending = false;
        self.alert_flags = AlertFlags::empty();
        self.alerts_queued = false;
        self.requested_main_width = None;
        self.requested_main_height = None;
        pane_id
    }

    pub(crate) fn set_size(&mut self, size: TerminalSize) {
        self.size = size;
        if self.zoomed {
            self.apply_zoom_geometry();
        } else {
            self.recalculate_geometry();
        }
    }

    pub(crate) fn set_name(&mut self, name: String) {
        self.name = Some(name);
        self.automatic_rename = false;
    }

    /// Re-enables runtime automatic renaming for this window.
    pub fn enable_automatic_rename(&mut self) {
        self.automatic_rename = true;
    }

    /// Updates the runtime window name while preserving automatic renaming.
    pub fn set_automatic_name(&mut self, name: String) {
        self.name = Some(name);
        self.automatic_rename = true;
    }

    pub(crate) fn rotate_panes(&mut self, direction: RotateWindowDirection) {
        self.rotate_panes_with_zoom(direction, false);
    }

    pub(crate) fn rotate_panes_with_zoom(
        &mut self,
        direction: RotateWindowDirection,
        restore_zoom: bool,
    ) {
        if self.panes.len() <= 1 {
            return;
        }

        self.push_zoom(restore_zoom);
        let previous_active_pane_id = self
            .active_pane()
            .expect("active pane must exist before pane rotation")
            .id();
        let active_position = self
            .panes
            .iter()
            .position(|pane| pane.index() == self.active_pane)
            .expect("active pane must exist in window order");

        match direction {
            RotateWindowDirection::Down => self.panes.rotate_right(1),
            RotateWindowDirection::Up => self.panes.rotate_left(1),
        }
        for (index, pane) in self.panes.iter_mut().enumerate() {
            pane.set_index(index as u32);
        }

        self.apply_layout_tree();

        // tmux keeps the selected slot stable while pane contents rotate, then
        // tracks last-pane as the pane identity that was active before rotation.
        self.active_pane = active_position as u32;
        self.last_pane = self
            .pane_index_for_id(previous_active_pane_id)
            .filter(|pane_index| *pane_index != self.active_pane);
        self.mark_pane_active(self.active_pane);

        self.pop_zoom();
    }

    pub(crate) fn insert_pane_at_position(
        &mut self,
        position: usize,
        pane: Pane,
        direction: SplitDirection,
    ) -> Result<(), RmuxError> {
        if position > self.panes.len() {
            return Err(RmuxError::Server(format!(
                "cannot insert pane at position {position} in a {}-pane window",
                self.panes.len()
            )));
        }

        self.ensure_accepts_pane(&pane, None)?;
        self.auto_unzoom();
        self.layout = layout_for_split(direction);
        self.bump_next_pane_index(pane.index());
        let inserted_index = pane.index();
        self.panes.insert(position, pane);
        if self.panes.len() == 1 {
            self.active_pane = inserted_index;
            self.last_pane = None;
        }
        if self.panes.len() == 1 {
            self.layout_tree = Some(LayoutTree::single(self.size));
            self.apply_layout_tree();
            return Ok(());
        }

        let (target_leaf, insert_before_target) = if position == 0 {
            (0, true)
        } else {
            (position - 1, false)
        };
        let inserted = self.layout_tree.as_mut().is_some_and(|tree| {
            tree.split_leaf(
                target_leaf,
                LayoutDirection::from_split_direction(direction),
                insert_before_target,
            )
        });
        if !inserted {
            self.rebuild_named_layout_tree(self.layout);
        } else {
            self.apply_layout_tree();
        }
        Ok(())
    }

    pub(crate) fn move_pane_by_splitting_target(
        &mut self,
        source_position: usize,
        target_position: usize,
        final_insert_position: usize,
        direction: SplitDirection,
        insert_before_target: bool,
    ) -> Result<PaneId, RmuxError> {
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
        if final_insert_position > pane_count.saturating_sub(1) {
            return Err(RmuxError::Server(format!(
                "cannot insert moved pane at position {final_insert_position} in a {}-pane window",
                pane_count.saturating_sub(1)
            )));
        }

        self.auto_unzoom();
        self.layout = layout_for_split(direction);

        let split_insert_position = if insert_before_target {
            target_position
        } else {
            target_position + 1
        };
        let source_leaf_after_split = if split_insert_position <= source_position {
            source_position + 1
        } else {
            source_position
        };
        let tree = self.layout_tree.as_mut().ok_or_else(|| {
            RmuxError::Server("cannot move pane without a layout tree".to_owned())
        })?;
        if !tree.split_leaf(
            target_position,
            LayoutDirection::from_split_direction(direction),
            insert_before_target,
        ) {
            return Err(RmuxError::Server(format!(
                "cannot split target pane at position {target_position}"
            )));
        }
        if !tree.remove_leaf(source_leaf_after_split) {
            return Err(RmuxError::Server(format!(
                "cannot remove source pane leaf at position {source_leaf_after_split}"
            )));
        }

        let moved_pane = self.panes.remove(source_position);
        let moved_pane_id = moved_pane.id();
        self.panes.insert(final_insert_position, moved_pane);
        self.apply_layout_tree();
        Ok(moved_pane_id)
    }

    pub(crate) fn insert_pane_full_size(
        &mut self,
        pane: Pane,
        direction: SplitDirection,
        insert_before_target: bool,
    ) -> Result<(), RmuxError> {
        self.ensure_accepts_pane(&pane, None)?;
        self.auto_unzoom();
        self.layout = layout_for_split(direction);
        self.bump_next_pane_index(pane.index());

        if insert_before_target {
            self.panes.insert(0, pane);
        } else {
            self.panes.push(pane);
        }

        let split = self.layout_tree.as_mut().is_some_and(|tree| {
            tree.split_root(
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
        self.panes[position] = pane;
        self.apply_layout_tree();
        Ok(())
    }

    pub(crate) fn swap_panes(&mut self, source_pane_index: u32, target_pane_index: u32) -> bool {
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
        self.panes.swap(source_position, target_position);
        self.apply_layout_tree();
        self.renumber_panes_by_position(active_pane_id, last_pane_id);
        true
    }
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "window/tests.rs"]
mod tests;
