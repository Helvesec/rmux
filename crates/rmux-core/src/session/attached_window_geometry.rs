use rmux_proto::{RmuxError, TerminalSize, WindowTarget};

use super::{target_error::invalid_window_target, Session, SessionStore};
use crate::Window;

/// Geometry-only access to one exclusively borrowed stored window.
///
/// The window remains owned by its backing store. Resizing an active occurrence
/// updates both canonical content size and the addressed session's outer
/// terminal size; resizing an inactive occurrence updates only content size.
pub struct AttachedWindowGeometry<'a> {
    pub(super) terminal_size: Option<&'a mut TerminalSize>,
    pub(super) window: &'a mut Window,
}

impl AttachedWindowGeometry<'_> {
    /// Retains the explicit baseline while control limits constrain manual size.
    pub fn mark_manual_size_constrained(&mut self) {
        self.window.mark_manual_size_constrained();
    }

    /// Applies the selected outer terminal and window content dimensions.
    pub fn resize(self, terminal_size: TerminalSize, content_size: TerminalSize) {
        if let Some(stored_terminal_size) = self.terminal_size {
            *stored_terminal_size = terminal_size;
        }
        self.window.set_size(content_size);
    }
}

impl Session {
    fn attached_window_geometry_mut(
        &mut self,
        window_index: u32,
    ) -> Result<AttachedWindowGeometry<'_>, RmuxError> {
        if !self.windows.contains_key(&window_index) {
            return Err(invalid_window_target(&self.name, window_index));
        }
        let terminal_size = (self.active_window == window_index).then_some(&mut self.terminal_size);
        let window = self
            .windows
            .get_mut(&window_index)
            .expect("validated attached window must exist");
        Ok(AttachedWindowGeometry {
            terminal_size,
            window,
        })
    }
}

impl SessionStore {
    /// Borrows only the stored geometry addressed by `target`.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::SessionNotFound`] if the session does not exist, or
    /// [`RmuxError::InvalidTarget`] if the window index does not exist.
    pub fn attached_window_geometry_mut(
        &mut self,
        target: &WindowTarget,
    ) -> Result<AttachedWindowGeometry<'_>, RmuxError> {
        self.session_mut(target.session_name())
            .ok_or_else(|| RmuxError::SessionNotFound(target.session_name().to_string()))?
            .attached_window_geometry_mut(target.window_index())
    }

    /// Records an attach and then borrows only the stored geometry addressed
    /// by `target`.
    ///
    /// This keeps the session-owned timestamp and recency update adjacent to
    /// geometry mutation without lending the caller the whole session.
    ///
    /// # Errors
    ///
    /// Returns [`RmuxError::SessionNotFound`] if the session does not exist, or
    /// [`RmuxError::InvalidTarget`] if the window index does not exist.
    pub fn touched_attached_window_geometry_mut(
        &mut self,
        target: &WindowTarget,
    ) -> Result<AttachedWindowGeometry<'_>, RmuxError> {
        let session = self
            .session_mut(target.session_name())
            .ok_or_else(|| RmuxError::SessionNotFound(target.session_name().to_string()))?;
        session.touch_attached();
        session.attached_window_geometry_mut(target.window_index())
    }
}

#[cfg(test)]
#[path = "attached_window_geometry_tests.rs"]
mod tests;
