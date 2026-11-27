use std::time::{SystemTime, UNIX_EPOCH};

use rmux_proto::{LayoutName, RotateWindowDirection, TerminalSize};

use crate::layout::LayoutTree;
use crate::{Pane, PaneGeometry, PaneId, WindowId};

mod manual_size;
mod size_limit;
pub use manual_size::ManualWindowSize;
pub use size_limit::{
    cap_window_size, parse_window_dimension, validate_explicit_window_size, WINDOW_MAXIMUM,
    WINDOW_MINIMUM,
};

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
#[derive(Debug, Clone, Eq)]
pub struct Window {
    id: WindowId,
    panes: Vec<Pane>,
    next_pane_index: u32,
    active_pane: u32,
    last_pane: Option<u32>,
    layout: LayoutName,
    last_layout: Option<LayoutName>,
    layout_tree: Option<LayoutTree>,
    floating_stack: FloatingStack,
    resize_topology: rmux_proto::RmuxWindowResizeTopology,
    // Slot owners captured only while the public API permits whole-pane replacement.
    borrowed_pane_owners: Option<Vec<PaneId>>,
    custom_layout: bool,
    old_layout: Option<String>,
    size: TerminalSize,
    manual_size: ManualWindowSize,
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
    // `tiled-layout-max-columns` as resolved when the layout was selected, kept
    // so later rebuilds of the same named layout use the same bound.
    // `None` is unbounded, which is also what the option's `0` means.
    tiled_max_columns: Option<u16>,
    // Where the last float this window placed for itself was put, which is what
    // an omitted `-X`/`-Y` walks forward from (`layout.c:1399-1417`). Zero is
    // the source's own "nothing placed yet". The pair belongs to the window, so
    // every link of it shares one cascade and another window starts its own.
    // It is history, not layout: the source assigns it in the omitted branch
    // before anything is placed and never puts it back, so it stays out of
    // `WindowCheckpoint` and an opening that fails afterwards still moves the
    // cascade on — without which a window whose next step this crate refuses
    // would propose that same step for ever.
    last_new_pane_x: i32,
    last_new_pane_y: i32,
}

mod activity;
mod checkpoint;
mod floating;
mod mutable_pane;
mod new_pane;
pub(crate) use checkpoint::WindowCheckpoint;
pub use floating::{FloatingPanePlacement, FloatingPaneRect, FloatingPaneRequest, FloatingStack};
pub(crate) use pane_insertion::PaneInsertionTarget;
mod layout_cycle;
mod layout_ops;
pub(crate) mod pane_insertion;
mod pane_transfer;
mod panes;
mod resize_topology;
mod snapshot;
mod zoom;

pub use activity::WindowPaneActivity;

impl Window {
    /// Creates the single V1 window with its initial pane.
    #[must_use]
    pub fn new(size: TerminalSize) -> Self {
        Self::new_with_initial_pane(size, PaneId::new(0), WindowId::new(0))
    }

    pub(crate) fn new_with_initial_pane(size: TerminalSize, pane_id: PaneId, id: WindowId) -> Self {
        let size = cap_window_size(size);
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
            layout_tree: Some(LayoutTree::single(size, pane_id)),
            floating_stack: FloatingStack::default(),
            resize_topology: rmux_proto::RmuxWindowResizeTopology::default(),
            borrowed_pane_owners: None,
            custom_layout: false,
            old_layout: None,
            size,
            manual_size: ManualWindowSize::new(size),
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
            tiled_max_columns: None,
            // Nothing has been placed yet, which is the source's own zero.
            last_new_pane_x: 0,
            last_new_pane_y: 0,
        };
        window.recalculate_geometry();
        window
    }

    /// Returns the stable internal window identity.
    #[must_use]
    pub const fn id(&self) -> WindowId {
        self.id
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
    ///
    /// Replacing the value transfers its occupied layout cell to the new pane.
    #[must_use]
    pub fn pane_mut(&mut self, pane_index: u32) -> Option<&mut Pane> {
        let position = self.pane_position(pane_index)?;
        self.borrowed_pane_owners
            .get_or_insert_with(|| self.panes.iter().map(Pane::id).collect());
        Some(&mut self.panes[position])
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

    /// Returns the pane `last-pane` and `select-pane -l` address: the recorded
    /// last pane, else, in a window of exactly two panes (floating panes
    /// included, as tmux 3.7b counts them), the pane that is not active.
    ///
    /// Resolving the pane changes nothing, so `-d`/`-e` can toggle its input
    /// without selecting it.
    #[must_use]
    pub fn last_pane_or_two_pane_fallback(&self) -> Option<u32> {
        if let Some(last) = self.last_pane {
            return Some(last);
        }
        if self.pane_count() != 2 {
            return None;
        }
        self.panes
            .iter()
            .map(Pane::index)
            .find(|&index| index != self.active_pane)
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

    /// Returns the fixed window viewport. Canonical tiled content and pane PTYs
    /// can exceed this extent after a root-axis transfer; scenes clip separately.
    #[must_use]
    pub const fn size(&self) -> TerminalSize {
        self.size
    }

    /// Returns the tmux-compatible serialized layout tree for the window.
    #[must_use]
    pub fn layout_dump(&self) -> String {
        self.current_layout_tree()
            .map_or_else(String::new, |tree| tree.dump())
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
        self.reconcile_borrowed_pane_owners();
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
        self.layout_tree = Some(LayoutTree::single(size, pane_id));
        self.floating_stack = FloatingStack::default();
        self.resize_topology = rmux_proto::RmuxWindowResizeTopology::default();
        self.custom_layout = false;
        self.old_layout = None;
        self.automatic_rename = true;
        self.zoomed = false;
        self.zoom_restore_pending = false;
        self.alert_flags = AlertFlags::empty();
        self.alerts_queued = false;
        self.requested_main_width = None;
        self.requested_main_height = None;
        self.tiled_max_columns = None;
        pane_id
    }

    /// Last explicit size, retained while clients constrain the visible geometry.
    #[must_use]
    pub const fn manual_size(&self) -> ManualWindowSize {
        self.manual_size
    }

    pub(crate) fn mark_manual_size_constrained(&mut self) {
        self.manual_size.constrained = true;
    }

    pub(crate) fn set_explicit_size(&mut self, size: TerminalSize) {
        let size = cap_window_size(size);
        self.manual_size.size = size;
        self.set_size(size);
    }

    pub(crate) fn set_size(&mut self, size: TerminalSize) {
        self.size = cap_window_size(size);
        self.recalculate_geometry();
        if self.zoomed {
            self.apply_zoom_geometry();
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
        self.reconcile_borrowed_pane_owners();
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

        let old_ids: Vec<_> = self.panes.iter().map(Pane::id).collect();
        match direction {
            RotateWindowDirection::Down => self.panes.rotate_right(1),
            RotateWindowDirection::Up => self.panes.rotate_left(1),
        }
        for (index, pane) in self.panes.iter_mut().enumerate() {
            pane.set_index(index as u32);
        }

        let replacements: Vec<_> = old_ids
            .into_iter()
            .zip(self.panes.iter().map(Pane::id))
            .collect();
        if let Some(tree) = &mut self.layout_tree {
            tree.remap_ids(&replacements);
        }
        self.remap_floating_pane_ids(&replacements);
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
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "window/tcol_tests.rs"]
mod tcol_tests;

#[cfg(test)]
#[path = "window/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "window/identity_tests.rs"]
mod identity_tests;

#[cfg(test)]
#[path = "window/mutable_pane_tests.rs"]
mod mutable_pane_tests;

#[cfg(test)]
#[path = "window/floating_tests.rs"]
mod floating_tests;

#[cfg(test)]
mod floating_resize_tests;

#[cfg(test)]
#[path = "window/floating_selection_tests.rs"]
mod floating_selection_tests;
#[cfg(test)]
#[path = "window/snapshot_tests.rs"]
mod snapshot_tests;
