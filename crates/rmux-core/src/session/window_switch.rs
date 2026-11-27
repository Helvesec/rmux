use rmux_proto::{PaneId, RmuxError, SessionName, TerminalSize, WindowTarget};

use super::target_error::{invalid_pane_target, invalid_window_target};
use super::window_ops::navigation::select_existing_window;
use super::{Session, SessionStore};
use crate::{AlertFlags, Window, WindowId};
use std::collections::BTreeMap;

/// A switch operation over one exclusively borrowed stored window.
///
/// The window remains owned by its backing store. The operation borrows only the
/// session metadata needed to select that window and apply its post-selection
/// geometry; it exposes no owning session value.
pub struct WindowSwitch<'a> {
    pub(super) session_name: &'a SessionName,
    pub(super) window_index: u32,
    pub(super) terminal_size: &'a mut TerminalSize,
    pub(super) window: &'a mut Window,
    pub(super) winlink_alert_flags: &'a mut BTreeMap<u32, AlertFlags>,
    pub(super) active_window: &'a mut u32,
    pub(super) last_window: &'a mut Option<u32>,
    pub(super) window_stack: &'a mut Vec<u32>,
    pub(super) group_initial_window_id: &'a mut Option<WindowId>,
}

impl WindowSwitch<'_> {
    /// Returns the stable identity of the addressed window.
    #[must_use]
    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    /// Returns the stable identity of a local pane index when it exists.
    #[must_use]
    pub fn pane_id(&self, pane_index: u32) -> Option<PaneId> {
        self.window.pane(pane_index).map(crate::Pane::id)
    }

    /// Selects the addressed window with standalone [`Session`] semantics.
    pub fn select_window(&mut self) {
        select_existing_window(
            self.window_index,
            self.active_window,
            self.last_window,
            self.window_stack,
            self.group_initial_window_id,
            self.winlink_alert_flags,
        );
    }

    /// Selects the addressed window and pane, restoring zoom when requested.
    ///
    /// The pane is validated before any mutation. For a zoom-preserving switch,
    /// the window is unzoomed before either selection and rezoomed on the newly
    /// active pane afterwards.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::InvalidTarget`] if the pane index does not exist.
    pub fn select_window_and_pane(
        &mut self,
        pane_index: u32,
        preserve_zoom: bool,
    ) -> Result<(), RmuxError> {
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
        self.select_window();
        let selected = self.window.select_pane(pane_index);
        if preserve_zoom {
            self.window.pop_zoom();
        }
        debug_assert!(selected, "validated pane target must be selectable");
        Ok(())
    }

    /// Retains the explicit baseline while control limits constrain manual size.
    pub fn mark_manual_size_constrained(&mut self) {
        self.window.mark_manual_size_constrained();
    }

    /// Applies outer terminal and content geometry to the addressed window.
    ///
    /// Whether the outer terminal size changes is decided from the active
    /// window after any selection performed by this operation.
    pub fn resize(&mut self, terminal_size: TerminalSize, content_size: TerminalSize) {
        if *self.active_window == self.window_index {
            *self.terminal_size = terminal_size;
        }
        self.window.set_size(content_size);
    }
}

impl Session {
    fn window_switch_mut(&mut self, window_index: u32) -> Result<WindowSwitch<'_>, RmuxError> {
        let Self {
            name,
            terminal_size,
            windows,
            winlink_alert_flags,
            active_window,
            last_window,
            window_stack,
            group_initial_window_id,
            ..
        } = self;
        let window = windows
            .get_mut(&window_index)
            .ok_or_else(|| invalid_window_target(name, window_index))?;
        Ok(WindowSwitch {
            session_name: name,
            window_index,
            terminal_size,
            window,
            winlink_alert_flags,
            active_window,
            last_window,
            window_stack,
            group_initial_window_id,
        })
    }
}

impl SessionStore {
    /// Borrows one stored window and only the session metadata needed to switch
    /// to it and apply post-selection geometry.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::SessionNotFound`] if the session does not exist, or
    /// [`RmuxError::InvalidTarget`] if the window index does not exist.
    pub fn window_switch_mut(
        &mut self,
        target: &WindowTarget,
    ) -> Result<WindowSwitch<'_>, RmuxError> {
        self.session_mut(target.session_name())
            .ok_or_else(|| RmuxError::SessionNotFound(target.session_name().to_string()))?
            .window_switch_mut(target.window_index())
    }

    /// Records an attach and borrows the addressed switch operation.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::SessionNotFound`] if the session does not exist, or
    /// [`RmuxError::InvalidTarget`] if the window index does not exist.
    pub fn touched_window_switch_mut(
        &mut self,
        target: &WindowTarget,
    ) -> Result<WindowSwitch<'_>, RmuxError> {
        let session = self
            .session_mut(target.session_name())
            .ok_or_else(|| RmuxError::SessionNotFound(target.session_name().to_string()))?;
        session.touch_attached();
        session.window_switch_mut(target.window_index())
    }
}

#[cfg(test)]
#[path = "window_switch_tests.rs"]
mod tests;
