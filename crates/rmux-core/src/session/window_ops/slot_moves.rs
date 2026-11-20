//! Relocation of windows between slots of one session, with tmux's
//! selection, return-window and visit-history tracking.

use rmux_proto::RmuxError;

use super::super::target_error::{invalid_window_target, invalid_window_target_with_reason};
use super::super::{cyclic_previous_window_index, Session};
use crate::Window;

impl Session {
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

    fn extract_window_for_move(&mut self, window_index: u32) -> Result<Window, RmuxError> {
        self.windows
            .remove(&window_index)
            .ok_or_else(|| invalid_window_target(&self.name, window_index))
    }

    /// Moves one winlink up a slot without destroying it.
    ///
    /// `winlink_shuffle_up` only increments the index of the links it moves
    /// (`window.c:1758-1785`), so the selection, the return and the visits travel
    /// with the slot instead of being detached the way a move is.
    pub(super) fn shift_window_slot(
        &mut self,
        source_index: u32,
        destination_index: u32,
    ) -> Result<(), RmuxError> {
        let moved_window = self.extract_window_for_move(source_index)?;
        let moved_alert_flags = self
            .winlink_alert_flags
            .remove(&source_index)
            .unwrap_or_else(crate::AlertFlags::empty);
        self.windows.insert(destination_index, moved_window);
        self.winlink_alert_flags
            .insert(destination_index, moved_alert_flags);
        if self.active_window == source_index {
            self.active_window = destination_index;
        }
        for visited in &mut self.window_stack {
            if *visited == source_index {
                *visited = destination_index;
            }
        }
        self.last_window =
            crate::session::visit_history::live_head(&self.window_stack, self.active_window);
        Ok(())
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

        // A move is a new link and an unlink here too: the occurrence that left
        // and the destination it replaced are gone from the history, the link
        // that appears has none of its own, and only a real selection adds one.
        crate::session::visit_history::forget_occurrence(&mut self.window_stack, source_index);
        crate::session::visit_history::forget_occurrence(&mut self.window_stack, destination_index);
        self.window_stack
            .retain(|visited| self.windows.contains_key(visited));
        if self.active_window != previous_active && self.windows.contains_key(&previous_active) {
            crate::session::visit_history::record_visit(
                &mut self.window_stack,
                previous_active,
                self.active_window,
            );
        }
        let active = self.active_window;
        self.window_stack.retain(|visited| *visited != active);
        // The return is the head of the history that is left, never the link this
        // move has just created.
        self.last_window = crate::session::visit_history::live_head(&self.window_stack, active);
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

        cyclic_previous_window_index(&self.windows, removed_index)
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
}
