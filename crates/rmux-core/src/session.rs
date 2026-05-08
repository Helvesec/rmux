use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rmux_proto::{ResizePaneAdjustment, RmuxError, SessionName, SplitDirection, TerminalSize};

use crate::{AlertFlags, Pane, PaneId, SessionId, Window, WindowId};

#[path = "session/accessors.rs"]
mod accessors;
#[path = "session/layout_cycle.rs"]
mod layout_cycle;
#[path = "session/pane_transfer.rs"]
mod pane_transfer;
#[path = "session/pane_transfer_cross.rs"]
mod pane_transfer_cross;
#[path = "session/pane_transfer_shared.rs"]
mod pane_transfer_shared;
#[path = "session/store.rs"]
mod store;
#[path = "session/target_error.rs"]
mod target_error;
#[path = "session/types.rs"]
mod types;
#[path = "session/window_ops.rs"]
mod window_ops;

pub use store::SessionStore;
use target_error::{invalid_pane_target, invalid_window_target};
pub(crate) use types::WindowIdAllocator;
pub use types::{
    BreakPaneOptions, KillPaneOutcome, PaneJoinOptions, PaneSwapOptions, SessionPaneTarget,
};

/// A single detached RMUX session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    id: SessionId,
    name: SessionName,
    group_name: Option<SessionName>,
    windows: BTreeMap<u32, Window>,
    winlink_alert_flags: BTreeMap<u32, AlertFlags>,
    active_window: u32,
    last_window: Option<u32>,
    next_pane_id: u32,
    next_window_id: WindowIdAllocator,
    created_at: i64,
    activity_at: i64,
    last_attached_at: Option<i64>,
    cwd: Option<PathBuf>,
}

impl Session {
    /// Creates a new session with its initial pane active.
    #[must_use]
    pub fn new(name: SessionName, size: TerminalSize) -> Self {
        Self::new_with_initial_window(name, size, 0, PaneId::new(0), WindowId::new(0))
    }

    /// Creates a new session with an explicitly seeded initial window and pane identity.
    #[must_use]
    pub(crate) fn new_with_initial_window(
        name: SessionName,
        size: TerminalSize,
        window_index: u32,
        pane_id: PaneId,
        window_id: WindowId,
    ) -> Self {
        let now = current_unix_timestamp();
        Self {
            id: SessionId::new(0),
            name,
            group_name: None,
            windows: BTreeMap::from([(
                window_index,
                Window::new_with_initial_pane(size, pane_id, window_id),
            )]),
            winlink_alert_flags: BTreeMap::from([(window_index, AlertFlags::empty())]),
            active_window: window_index,
            last_window: None,
            next_pane_id: pane_id.as_u32().saturating_add(1),
            next_window_id: WindowIdAllocator::new(window_id.as_u32().saturating_add(1)),
            created_at: now,
            activity_at: now,
            last_attached_at: None,
            cwd: None,
        }
    }

    /// Splits the current active pane and returns the new pane index, making the new pane active.
    pub fn split_active_pane(&mut self) -> Result<u32, RmuxError> {
        self.split_active_pane_with_direction(SplitDirection::Vertical)
    }

    /// Splits the current active pane in the requested direction.
    pub fn split_active_pane_with_direction(
        &mut self,
        direction: SplitDirection,
    ) -> Result<u32, RmuxError> {
        self.split_pane_with_direction(self.active_pane_index(), direction)
    }

    /// Splits the addressed pane, inserting the new pane immediately after the split target in window order.
    pub fn split_pane(&mut self, pane_index: u32) -> Result<u32, RmuxError> {
        self.split_pane_with_direction(pane_index, SplitDirection::Vertical)
    }

    /// Splits the addressed pane in the active window using the requested direction.
    pub fn split_pane_with_direction(
        &mut self,
        pane_index: u32,
        direction: SplitDirection,
    ) -> Result<u32, RmuxError> {
        self.split_pane_in_window_with_direction(self.active_window, pane_index, direction)
    }

    /// Splits the addressed pane in the addressed window and returns the new pane index.
    pub fn split_pane_in_window(
        &mut self,
        window_index: u32,
        pane_index: u32,
    ) -> Result<u32, RmuxError> {
        self.split_pane_in_window_with_direction(window_index, pane_index, SplitDirection::Vertical)
    }

    /// Splits the addressed pane in the addressed window using the requested direction.
    pub fn split_pane_in_window_with_direction(
        &mut self,
        window_index: u32,
        pane_index: u32,
        direction: SplitDirection,
    ) -> Result<u32, RmuxError> {
        let pane_id = self.allocate_pane_id();
        self.split_pane_in_window_with_id_and_direction(
            window_index,
            pane_index,
            pane_id,
            direction,
        )
    }

    /// Splits the addressed pane in the addressed window using the provided pane identity.
    pub fn split_pane_in_window_with_id_and_direction(
        &mut self,
        window_index: u32,
        pane_index: u32,
        pane_id: PaneId,
        direction: SplitDirection,
    ) -> Result<u32, RmuxError> {
        let window = self
            .window_at(window_index)
            .ok_or_else(|| invalid_window_target(&self.name, window_index))?;
        let position = window.pane_position(pane_index).ok_or_else(|| {
            invalid_pane_target(
                &self.name,
                window_index,
                pane_index,
                "pane index does not exist in session",
            )
        })?;
        if !window.can_split_pane(pane_index, direction) {
            return Err(RmuxError::Message("no space for new pane".to_owned()));
        }
        Ok(self
            .window_at_mut(window_index)
            .expect("addressed session window must exist")
            .split_after_position_with_id_and_direction(position, pane_id, direction))
    }

    /// Removes the addressed pane in the active window.
    pub fn kill_pane(&mut self, pane_index: u32) -> Result<KillPaneOutcome, RmuxError> {
        self.kill_pane_in_window(self.active_window, pane_index)
    }

    /// Removes the addressed pane or destroys its window when it is the last pane there.
    pub fn kill_pane_in_window(
        &mut self,
        window_index: u32,
        pane_index: u32,
    ) -> Result<KillPaneOutcome, RmuxError> {
        let window = self
            .window_at(window_index)
            .ok_or_else(|| invalid_window_target(&self.name, window_index))?;
        let pane_id = window.pane_id(pane_index).ok_or_else(|| {
            invalid_pane_target(
                &self.name,
                window_index,
                pane_index,
                "pane index does not exist in session",
            )
        })?;

        if window.pane_count() == 1 {
            let removed_window = self.remove_window(window_index)?;
            let removed_pane_ids = removed_window.panes().iter().map(Pane::id).collect();
            return Ok(KillPaneOutcome::new(removed_pane_ids, true));
        }

        let removed_pane = self
            .window_at_mut(window_index)
            .expect("addressed session window must exist")
            .remove_pane(pane_index)
            .expect("prevalidated pane removal must succeed");
        debug_assert_eq!(removed_pane.id(), pane_id);

        Ok(KillPaneOutcome::new(vec![removed_pane.id()], false))
    }

    /// Removes every pane except the addressed pane, matching `kill-pane -a`.
    pub fn kill_other_panes_in_window(
        &mut self,
        window_index: u32,
        pane_index: u32,
    ) -> Result<KillPaneOutcome, RmuxError> {
        let window = self
            .window_at(window_index)
            .ok_or_else(|| invalid_window_target(&self.name, window_index))?;
        if window.pane(pane_index).is_none() {
            return Err(invalid_pane_target(
                &self.name,
                window_index,
                pane_index,
                "pane index does not exist in session",
            ));
        }

        let removed_pane_ids = self
            .window_at_mut(window_index)
            .expect("addressed session window must exist")
            .remove_other_panes(pane_index)
            .expect("prevalidated pane removal must succeed");

        Ok(KillPaneOutcome::new(removed_pane_ids, false))
    }

    /// Selects the active pane for the session.
    pub fn select_pane(&mut self, pane_index: u32) -> Result<(), RmuxError> {
        self.select_pane_in_window(self.active_window, pane_index)
    }

    /// Selects the active pane for the addressed window.
    pub fn select_pane_in_window(
        &mut self,
        window_index: u32,
        pane_index: u32,
    ) -> Result<(), RmuxError> {
        if self.window_at(window_index).is_none() {
            return Err(invalid_window_target(&self.name, window_index));
        }

        if self
            .window_at_mut(window_index)
            .expect("addressed session window must exist")
            .select_pane(pane_index)
        {
            Ok(())
        } else {
            Err(invalid_pane_target(
                &self.name,
                window_index,
                pane_index,
                "pane index does not exist in session",
            ))
        }
    }

    /// Selects the pane adjacent to the addressed pane in the requested direction.
    pub fn select_adjacent_pane_in_window(
        &mut self,
        window_index: u32,
        pane_index: u32,
        direction: rmux_proto::SelectPaneDirection,
    ) -> Result<u32, RmuxError> {
        if self.window_at(window_index).is_none() {
            return Err(invalid_window_target(&self.name, window_index));
        }

        self.window_at_mut(window_index)
            .expect("addressed session window must exist")
            .select_adjacent_pane(pane_index, direction)
            .ok_or_else(|| {
                invalid_pane_target(
                    &self.name,
                    window_index,
                    pane_index,
                    "pane index does not exist in session",
                )
            })
    }

    /// Applies the supported resize adjustment to the session layout by updating the `main-vertical` main-pane width.
    pub fn resize_pane(
        &mut self,
        pane_index: u32,
        adjustment: ResizePaneAdjustment,
    ) -> Result<(), RmuxError> {
        self.resize_pane_in_window(self.active_window, pane_index, adjustment)
    }

    /// Applies the supported resize adjustment to the addressed window.
    pub fn resize_pane_in_window(
        &mut self,
        window_index: u32,
        pane_index: u32,
        adjustment: ResizePaneAdjustment,
    ) -> Result<(), RmuxError> {
        if adjustment == ResizePaneAdjustment::Zoom {
            return self.toggle_zoom_in_window(window_index, pane_index);
        }

        if self.window_at(window_index).is_none() {
            return Err(invalid_window_target(&self.name, window_index));
        }

        if self
            .window_at(window_index)
            .and_then(|window| window.pane(pane_index))
            .is_none()
        {
            return Err(invalid_pane_target(
                &self.name,
                window_index,
                pane_index,
                "pane index does not exist in session",
            ));
        }

        let window = self
            .window_at_mut(window_index)
            .expect("addressed session window must exist");

        match adjustment {
            ResizePaneAdjustment::NoOp => {}
            ResizePaneAdjustment::AbsoluteWidth { columns } => {
                let _ = window.resize_pane_width(pane_index, columns);
            }
            ResizePaneAdjustment::AbsoluteHeight { rows } => {
                let _ = window.resize_pane_height(pane_index, rows);
            }
            ResizePaneAdjustment::Up { cells } => {
                let _ = window.resize_pane_by(pane_index, ResizePaneAdjustment::Up { cells });
            }
            ResizePaneAdjustment::Down { cells } => {
                let _ = window.resize_pane_by(pane_index, ResizePaneAdjustment::Down { cells });
            }
            ResizePaneAdjustment::Left { cells } => {
                let _ = window.resize_pane_by(pane_index, ResizePaneAdjustment::Left { cells });
            }
            ResizePaneAdjustment::Right { cells } => {
                let _ = window.resize_pane_by(pane_index, ResizePaneAdjustment::Right { cells });
            }
            ResizePaneAdjustment::Zoom => unreachable!("zoom returned early"),
        }
        Ok(())
    }

    /// Toggles zoom for the addressed pane's window.
    pub fn toggle_zoom_in_window(
        &mut self,
        window_index: u32,
        pane_index: u32,
    ) -> Result<(), RmuxError> {
        if self.window_at(window_index).is_none() {
            return Err(invalid_window_target(&self.name, window_index));
        }

        if self
            .window_at(window_index)
            .and_then(|window| window.pane(pane_index))
            .is_none()
        {
            return Err(invalid_pane_target(
                &self.name,
                window_index,
                pane_index,
                "pane index does not exist in session",
            ));
        }

        self.window_at_mut(window_index)
            .expect("addressed session window must exist")
            .toggle_zoom(pane_index);
        Ok(())
    }

    /// Updates the backing terminal size and recalculates pane geometry for all windows.
    pub fn resize_terminal(&mut self, size: TerminalSize) {
        for window in self.windows.values_mut() {
            window.set_size(size);
        }
    }

    fn resolve_window_target_mut(&mut self, window_index: u32) -> Result<&mut Window, RmuxError> {
        if !self.windows.contains_key(&window_index) {
            return Err(invalid_window_target(&self.name, window_index));
        }

        Ok(self
            .window_at_mut(window_index)
            .expect("addressed session window must exist"))
    }

    pub(crate) fn lowest_available_window_index_at_or_above(
        &self,
        minimum_index: u32,
    ) -> Result<u32, RmuxError> {
        let mut next_index = minimum_index;

        for window_index in self.windows.keys().copied() {
            if window_index < next_index {
                continue;
            }
            if window_index > next_index {
                break;
            }

            if window_index == next_index {
                next_index = next_index.checked_add(1).ok_or_else(|| {
                    RmuxError::Server(format!(
                        "window index space exhausted for session {}",
                        self.name
                    ))
                })?;
            }
        }

        Ok(next_index)
    }

    fn next_active_window_after_removal(&self, removed_index: u32) -> u32 {
        if let Some(last_window) = self.last_window {
            if last_window != removed_index && self.windows.contains_key(&last_window) {
                return last_window;
            }
        }

        if let Some((window_index, _)) = self.windows.range(..removed_index).next_back() {
            return *window_index;
        }

        self.windows
            .range((Excluded(removed_index), Unbounded))
            .next()
            .map(|(window_index, _)| *window_index)
            .expect("a non-empty session must have a replacement window")
    }

    fn allocate_pane_id(&mut self) -> PaneId {
        let mut next_pane_id = self.next_pane_id;

        loop {
            let pane_id = PaneId::new(next_pane_id);
            if !self.contains_pane_id(pane_id) {
                self.next_pane_id = next_pane_id.saturating_add(1);
                return pane_id;
            }

            assert_ne!(next_pane_id, u32::MAX, "pane id space exhausted");
            next_pane_id += 1;
        }
    }

    fn contains_pane_id(&self, pane_id: PaneId) -> bool {
        self.windows
            .values()
            .flat_map(Window::panes)
            .any(|pane| pane.id() == pane_id)
    }

    /// Resolves the owning window index for the given pane identity when present.
    pub fn window_index_for_pane_id(&self, pane_id: PaneId) -> Option<u32> {
        self.windows.iter().find_map(|(window_index, window)| {
            window
                .panes()
                .iter()
                .any(|pane| pane.id() == pane_id)
                .then_some(*window_index)
        })
    }

    fn allocate_window_id(&self) -> WindowId {
        self.next_window_id.allocate()
    }
}

fn synchronized_active_window(
    windows: &BTreeMap<u32, Window>,
    previous_active: u32,
    previous_last: Option<u32>,
) -> u32 {
    if windows.contains_key(&previous_active) {
        return previous_active;
    }

    if let Some(last_window) = previous_last {
        if last_window != previous_active && windows.contains_key(&last_window) {
            return last_window;
        }
    }

    if let Some((window_index, _)) = windows.range(..previous_active).next_back() {
        return *window_index;
    }

    windows
        .range((Excluded(previous_active), Unbounded))
        .next()
        .map(|(window_index, _)| *window_index)
        .or_else(|| windows.keys().next().copied())
        .expect("group synchronization requires at least one window")
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "session/zoom_tests.rs"]
mod zoom_tests;

#[cfg(test)]
#[path = "session/layout_tests.rs"]
mod layout_tests;
