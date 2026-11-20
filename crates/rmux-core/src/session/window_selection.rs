use super::{target_error::invalid_pane_target, SessionStore};
use crate::Window;
use rmux_proto::{RmuxError, SelectPaneDirection, SessionName, Target, WindowTarget};

/// A selection operation over one exclusively borrowed stored window.
///
/// The window remains owned by its backing store. The context
/// consumes its borrow when selecting a pane and owns no window state.
pub struct WindowSelection<'a> {
    session_name: &'a SessionName,
    window_index: u32,
    window: &'a mut Window,
}

impl<'a> WindowSelection<'a> {
    pub(super) fn new(
        session_name: &'a SessionName,
        window_index: u32,
        window: &'a mut Window,
    ) -> Self {
        Self {
            session_name,
            window_index,
            window,
        }
    }

    /// Selects a local pane index and returns the active index in this window.
    ///
    /// A changed selection updates pane history and unzooms unless `preserve_zoom`
    /// is set. Reselecting the active pane leaves history and zoom unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::InvalidTarget`] if the pane index does not exist.
    pub fn select(self, pane_index: u32, preserve_zoom: bool) -> Result<u32, RmuxError> {
        if self.window.pane(pane_index).is_none() {
            return Err(invalid_pane_target(
                self.session_name,
                self.window_index,
                pane_index,
                "pane index does not exist in session",
            ));
        }
        if preserve_zoom {
            let _ = self.window.push_zoom(true);
        }
        let selected = self.window.select_pane(pane_index);
        if preserve_zoom {
            self.window.pop_zoom();
        }
        debug_assert!(selected, "validated pane target must be selectable");
        Ok(self.window.active_pane_index())
    }

    /// Selects the previously active pane and returns its local index.
    ///
    /// Like tmux 3.7b `last-pane` and `select-pane -l`, a window of exactly
    /// two panes with no recorded last pane (for example after
    /// `split-window -d`) selects the other pane.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::Server`] if this window has no previous pane.
    pub fn select_last(self, preserve_zoom: bool) -> Result<u32, RmuxError> {
        let pane_index = self
            .window
            .last_pane_or_two_pane_fallback()
            .ok_or_else(|| RmuxError::Server("no last pane".to_owned()))?;
        if preserve_zoom {
            let _ = self.window.push_zoom(true);
        }
        let selected = self.window.select_pane(pane_index);
        if preserve_zoom {
            self.window.pop_zoom();
        }
        debug_assert!(selected, "validated last pane must be selectable");
        Ok(pane_index)
    }

    /// Selects a pane adjacent to `pane_index` in `direction`.
    ///
    /// The window's zoom is restored when `preserve_zoom` is set or when there
    /// is no adjacent pane in the requested direction.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::InvalidTarget`] if the anchor pane does not exist.
    pub fn select_adjacent(
        self,
        pane_index: u32,
        direction: SelectPaneDirection,
        preserve_zoom: bool,
    ) -> Result<u32, RmuxError> {
        if self.window.pane(pane_index).is_none() {
            return Err(invalid_pane_target(
                self.session_name,
                self.window_index,
                pane_index,
                "pane index does not exist in session",
            ));
        }

        let was_zoomed = self.window.is_zoomed();
        let original_active_pane = self.window.active_pane_index();
        if was_zoomed {
            let _ = self.window.push_zoom(false);
        }
        let selected = self
            .window
            .select_adjacent_pane(pane_index, direction)
            .expect("validated adjacent pane anchor must be selectable");
        let moved = selected != original_active_pane;
        if was_zoomed && (preserve_zoom || !moved) {
            let _ = self.window.toggle_zoom(self.window.active_pane_index());
        }
        Ok(selected)
    }
}

impl SessionStore {
    /// Borrows the addressed window for one selection without exposing a [`Session`](super::Session).
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::SessionNotFound`] if the session does not exist, or
    /// [`RmuxError::InvalidTarget`] if the window index does not exist.
    pub fn window_selection_mut<'a>(
        &'a mut self,
        target: &'a WindowTarget,
    ) -> Result<WindowSelection<'a>, RmuxError> {
        let window = self.resolve_window_mut(&Target::Window(target.clone()))?;
        Ok(WindowSelection::new(
            target.session_name(),
            target.window_index(),
            window,
        ))
    }
}

#[cfg(test)]
#[path = "window_selection_tests.rs"]
mod tests;
