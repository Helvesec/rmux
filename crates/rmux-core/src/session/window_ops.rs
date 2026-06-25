use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};

use rmux_proto::{RmuxError, RotateWindowDirection, TerminalSize};

use super::target_error::{invalid_window_target, invalid_window_target_with_reason};
use super::Session;
use crate::{Pane, PaneId, Window};

#[path = "window_ops/navigation.rs"]
mod navigation;

impl Session {
    /// Creates a new window at the lowest available index and returns its window index together with the initial pane ID.
    pub fn create_window(&mut self, size: TerminalSize) -> Result<(u32, PaneId), RmuxError> {
        self.create_window_at_or_above(size, 0)
    }

    /// Creates a new window at the lowest available index at or above `minimum_index`.
    pub fn create_window_at_or_above(
        &mut self,
        size: TerminalSize,
        minimum_index: u32,
    ) -> Result<(u32, PaneId), RmuxError> {
        let pane_id = self.allocate_pane_id();
        self.create_window_at_or_above_with_pane_id(size, minimum_index, pane_id)
    }

    /// Creates a new window at the lowest available index at or above `minimum_index`
    /// using the provided initial pane identity.
    pub fn create_window_at_or_above_with_pane_id(
        &mut self,
        size: TerminalSize,
        minimum_index: u32,
        pane_id: PaneId,
    ) -> Result<(u32, PaneId), RmuxError> {
        let window_index = self.lowest_available_window_index_at_or_above(minimum_index)?;
        let window_id = self.allocate_window_id();
        self.windows.insert(
            window_index,
            Window::new_with_initial_pane(size, pane_id, window_id),
        );
        self.winlink_alert_flags
            .insert(window_index, crate::AlertFlags::empty());
        Ok((window_index, pane_id))
    }

    /// Inserts a window with its initial pane at the provided window index.
    pub fn insert_window_with_initial_pane(
        &mut self,
        window_index: u32,
        size: TerminalSize,
    ) -> Result<(), RmuxError> {
        let pane_id = self.allocate_pane_id();
        self.insert_window_with_initial_pane_with_id(window_index, size, pane_id)
    }

    /// Inserts a window with its initial pane at the provided window index using the
    /// supplied pane identity.
    pub fn insert_window_with_initial_pane_with_id(
        &mut self,
        window_index: u32,
        size: TerminalSize,
        pane_id: PaneId,
    ) -> Result<(), RmuxError> {
        if self.windows.contains_key(&window_index) {
            return Err(invalid_window_target_with_reason(
                &self.name,
                window_index,
                "window index already exists in session",
            ));
        }

        let window_id = self.allocate_window_id();
        self.windows.insert(
            window_index,
            Window::new_with_initial_pane(size, pane_id, window_id),
        );
        self.winlink_alert_flags
            .insert(window_index, crate::AlertFlags::empty());
        Ok(())
    }

    /// Inserts an existing window at the provided window index without rewriting its identities.
    pub fn insert_existing_window(
        &mut self,
        window_index: u32,
        window: Window,
    ) -> Result<(), RmuxError> {
        if self.windows.contains_key(&window_index) {
            return Err(invalid_window_target_with_reason(
                &self.name,
                window_index,
                "window index already exists in session",
            ));
        }

        self.bump_allocators_for_window(&window);
        self.windows.insert(window_index, window);
        self.winlink_alert_flags
            .insert(window_index, crate::AlertFlags::empty());
        Ok(())
    }

    /// Opens a destination slot for link-window style insertion by shifting existing winlinks upward.
    pub fn make_room_for_window(&mut self, window_index: u32) -> Result<(), RmuxError> {
        let first_gap = self.lowest_available_window_index_at_or_above(window_index)?;
        if first_gap == window_index {
            return Ok(());
        }

        for source_index in (window_index..first_gap).rev() {
            let destination_index = source_index.checked_add(1).ok_or_else(|| {
                RmuxError::Server(format!(
                    "window index space exhausted for session {}",
                    self.name
                ))
            })?;
            let _ = self.move_window(source_index, destination_index, false, false)?;
        }

        Ok(())
    }

    /// Inserts a linked copy of an existing window at the destination slot.
    pub fn link_window(
        &mut self,
        window_index: u32,
        window: Window,
        kill_destination: bool,
        select_destination: bool,
    ) -> Result<Option<Window>, RmuxError> {
        if self.windows.contains_key(&window_index) && !kill_destination {
            return Err(invalid_window_target_with_reason(
                &self.name,
                window_index,
                "window index already exists in session",
            ));
        }

        let removed = if kill_destination {
            self.replace_window(window_index, window)?
        } else {
            self.insert_existing_window(window_index, window)?;
            return if select_destination {
                self.select_window(window_index)?;
                Ok(None)
            } else {
                Ok(None)
            };
        };

        if select_destination {
            self.select_window(window_index)?;
        }

        Ok(Some(removed))
    }

    /// Replaces an existing window at the provided index and returns the removed window.
    pub fn replace_window(
        &mut self,
        window_index: u32,
        window: Window,
    ) -> Result<Window, RmuxError> {
        if !self.windows.contains_key(&window_index) {
            return Err(invalid_window_target(&self.name, window_index));
        }

        self.bump_allocators_for_window(&window);
        Ok(self
            .windows
            .insert(window_index, window)
            .expect("replaced window must exist at the addressed index"))
    }

    /// Removes the addressed window and returns it, using tmux's last-then-previous-then-next active fallback when needed.
    pub fn remove_window(&mut self, window_index: u32) -> Result<Window, RmuxError> {
        if !self.windows.contains_key(&window_index) {
            return Err(invalid_window_target(&self.name, window_index));
        }

        if self.windows.len() == 1 {
            return Err(RmuxError::Server(format!(
                "cannot kill the only window in session {}",
                self.name
            )));
        }

        self.remove_window_allowing_empty(window_index)
    }

    pub(crate) fn remove_window_allowing_empty(
        &mut self,
        window_index: u32,
    ) -> Result<Window, RmuxError> {
        if !self.windows.contains_key(&window_index) {
            return Err(invalid_window_target(&self.name, window_index));
        }

        let next_active = if self.active_window == window_index {
            (self.windows.len() > 1).then(|| self.next_active_window_after_removal(window_index))
        } else {
            None
        };

        let removed = self
            .windows
            .remove(&window_index)
            .expect("window existence was checked before removal");
        self.winlink_alert_flags.remove(&window_index);

        if let Some(next_active) = next_active {
            self.select_window(next_active)
                .expect("replacement window must exist after removal");
        }

        if self.last_window == Some(window_index) {
            self.last_window = None;
        }

        Ok(removed)
    }

    /// Renames the addressed window and disables automatic renaming for it.
    pub fn rename_window(&mut self, window_index: u32, name: String) -> Result<(), RmuxError> {
        self.resolve_window_target_mut(window_index)?.set_name(name);
        Ok(())
    }

    /// Updates the addressed window name while preserving automatic renaming.
    pub fn set_automatic_window_name(
        &mut self,
        window_index: u32,
        name: String,
    ) -> Result<(), RmuxError> {
        let window = self.resolve_window_target_mut(window_index)?;
        if window.automatic_rename() {
            window.set_automatic_name(name);
        }
        Ok(())
    }

    /// Reindexes sparse window slots into a contiguous `0..n` range without rewriting the windows themselves.
    pub fn reindex_windows(&mut self) -> Result<BTreeMap<u32, u32>, RmuxError> {
        self.reindex_windows_from(0)
    }

    /// Reindexes sparse window slots into a contiguous range starting at `first_index`.
    pub fn reindex_windows_from(
        &mut self,
        first_index: u32,
    ) -> Result<BTreeMap<u32, u32>, RmuxError> {
        if !self.windows.is_empty() {
            let last_offset = self.windows.len().saturating_sub(1) as u32;
            first_index.checked_add(last_offset).ok_or_else(|| {
                RmuxError::Server(format!(
                    "window index space exhausted for session {}",
                    self.name
                ))
            })?;
        }
        let previous_windows = std::mem::take(&mut self.windows);
        let mut reindexed = BTreeMap::new();
        let mut index_map = BTreeMap::new();
        let mut next_index = first_index;

        let window_count = previous_windows.len();
        for (position, (window_index, window)) in previous_windows.into_iter().enumerate() {
            index_map.insert(window_index, next_index);
            reindexed.insert(next_index, window);
            if position + 1 < window_count {
                next_index = next_index.checked_add(1).ok_or_else(|| {
                    RmuxError::Server(format!(
                        "window index space exhausted for session {}",
                        self.name
                    ))
                })?;
            }
        }

        self.windows = reindexed;
        self.winlink_alert_flags = self
            .winlink_alert_flags
            .iter()
            .filter_map(|(window_index, flags)| {
                index_map
                    .get(window_index)
                    .copied()
                    .map(|next_index| (next_index, *flags))
            })
            .collect();
        self.active_window = *index_map
            .get(&self.active_window)
            .expect("active window must survive reindexing");
        self.last_window = self
            .last_window
            .and_then(|window_index| index_map.get(&window_index).copied());

        Ok(index_map)
    }

    /// Moves one window to another slot within the same session, optionally removing an occupied destination.
    ///
    /// When `select_destination` is `true`, the destination slot becomes active
    /// using tmux's winlink-based selection semantics instead of following the
    /// moved window by identity.
    pub fn move_window(
        &mut self,
        source_index: u32,
        destination_index: u32,
        kill_destination: bool,
        select_destination: bool,
    ) -> Result<Option<Window>, RmuxError> {
        if !self.windows.contains_key(&source_index) {
            return Err(invalid_window_target(&self.name, source_index));
        }
        if source_index == destination_index {
            return Ok(None);
        }
        if self.windows.contains_key(&destination_index) && !kill_destination {
            return Err(invalid_window_target_with_reason(
                &self.name,
                destination_index,
                "window index already exists in session",
            ));
        }

        let previous_active = self.active_window;
        let previous_last = self.last_window;
        let moved_window = self.extract_window_for_move(source_index)?;
        let moved_alert_flags = self
            .winlink_alert_flags
            .remove(&source_index)
            .unwrap_or_else(crate::AlertFlags::empty);
        let removed_window = if kill_destination {
            let _ = self.winlink_alert_flags.remove(&destination_index);
            self.windows.remove(&destination_index)
        } else {
            None
        };

        self.windows.insert(destination_index, moved_window);
        self.winlink_alert_flags
            .insert(destination_index, moved_alert_flags);
        self.apply_move_tracking(
            source_index,
            destination_index,
            previous_active,
            previous_last,
            select_destination,
        );

        Ok(removed_window)
    }

    /// Swaps two window slots within the same session while preserving the underlying windows.
    pub fn swap_windows(
        &mut self,
        source_index: u32,
        destination_index: u32,
    ) -> Result<(), RmuxError> {
        if !self.windows.contains_key(&source_index) {
            return Err(invalid_window_target(&self.name, source_index));
        }
        if !self.windows.contains_key(&destination_index) {
            return Err(invalid_window_target(&self.name, destination_index));
        }
        if source_index == destination_index {
            return Ok(());
        }

        let source_window = self
            .windows
            .remove(&source_index)
            .expect("source window must exist for swap");
        let destination_window = self
            .windows
            .remove(&destination_index)
            .expect("destination window must exist for swap");
        let source_flags = self
            .winlink_alert_flags
            .remove(&source_index)
            .unwrap_or_else(crate::AlertFlags::empty);
        let destination_flags = self
            .winlink_alert_flags
            .remove(&destination_index)
            .unwrap_or_else(crate::AlertFlags::empty);

        self.windows.insert(source_index, destination_window);
        self.windows.insert(destination_index, source_window);
        self.winlink_alert_flags
            .insert(source_index, destination_flags);
        self.winlink_alert_flags
            .insert(destination_index, source_flags);
        Ok(())
    }

    /// Rotates pane positions in the addressed window without renumbering the panes themselves.
    pub fn rotate_window(
        &mut self,
        window_index: u32,
        direction: RotateWindowDirection,
    ) -> Result<(), RmuxError> {
        self.resolve_window_target_mut(window_index)?
            .rotate_panes(direction);
        Ok(())
    }

    /// Rotates pane positions with zoom save/restore via `-Z`.
    pub fn rotate_window_with_zoom(
        &mut self,
        window_index: u32,
        direction: RotateWindowDirection,
        restore_zoom: bool,
    ) -> Result<(), RmuxError> {
        self.resolve_window_target_mut(window_index)?
            .rotate_panes_with_zoom(direction, restore_zoom);
        Ok(())
    }

    /// Sets the explicit size of the addressed window.
    pub fn resize_window(
        &mut self,
        window_index: u32,
        size: TerminalSize,
    ) -> Result<(), RmuxError> {
        self.resolve_window_target_mut(window_index)?.set_size(size);
        Ok(())
    }

    /// Respawns the addressed window, replacing it with a single pane.
    /// Returns the pane ID retained for the respawned pane.
    pub fn respawn_window(&mut self, window_index: u32) -> Result<PaneId, RmuxError> {
        let pane_id = self
            .window_at(window_index)
            .ok_or_else(|| invalid_window_target(&self.name, window_index))?
            .panes()
            .first()
            .map(Pane::id)
            .ok_or_else(|| RmuxError::Server("window has no panes".to_owned()))?;
        self.respawn_window_with_pane_id(window_index, pane_id)
    }

    /// Respawns the addressed window with an explicitly provided pane identity.
    pub fn respawn_window_with_pane_id(
        &mut self,
        window_index: u32,
        pane_id: PaneId,
    ) -> Result<PaneId, RmuxError> {
        let window = self.resolve_window_target_mut(window_index)?;
        window.respawn(pane_id);
        Ok(pane_id)
    }

    fn extract_window_for_move(&mut self, window_index: u32) -> Result<Window, RmuxError> {
        self.windows
            .remove(&window_index)
            .ok_or_else(|| invalid_window_target(&self.name, window_index))
    }

    fn apply_move_tracking(
        &mut self,
        source_index: u32,
        destination_index: u32,
        previous_active: u32,
        previous_last: Option<u32>,
        select_destination: bool,
    ) {
        let source_was_active = previous_active == source_index;
        let destination_was_active = previous_active == destination_index;

        self.active_window = if source_was_active {
            if select_destination {
                destination_index
            } else {
                self.next_active_window_after_detach(source_index, previous_last)
            }
        } else if select_destination {
            destination_index
        } else {
            previous_active
        };

        self.last_window = if source_was_active {
            if select_destination {
                self.preserved_last_after_selecting_destination(
                    source_index,
                    destination_index,
                    previous_last,
                )
            } else {
                None
            }
        } else if select_destination {
            if destination_was_active {
                self.preserved_last_after_selecting_destination(
                    source_index,
                    destination_index,
                    previous_last,
                )
            } else {
                Some(previous_active)
            }
        } else if previous_last == Some(source_index) {
            None
        } else {
            previous_last.filter(|window_index| self.windows.contains_key(window_index))
        };

        if self.last_window == Some(self.active_window) {
            self.last_window = None;
        }
    }

    fn next_active_window_after_detach(
        &self,
        removed_index: u32,
        previous_last: Option<u32>,
    ) -> u32 {
        if let Some(last_window) = previous_last {
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

    fn preserved_last_after_selecting_destination(
        &self,
        source_index: u32,
        destination_index: u32,
        previous_last: Option<u32>,
    ) -> Option<u32> {
        previous_last.filter(|window_index| {
            *window_index != source_index
                && *window_index != destination_index
                && self.windows.contains_key(window_index)
        })
    }

    fn bump_allocators_for_window(&mut self, window: &Window) {
        self.next_window_id
            .bump_to(window.id().as_u32().saturating_add(1));
        self.next_pane_id = self.next_pane_id.max(
            window
                .panes()
                .iter()
                .map(|pane| pane.id().as_u32().saturating_add(1))
                .max()
                .unwrap_or(self.next_pane_id),
        );
    }
}
