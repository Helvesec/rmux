use super::*;
use crate::AttachedWindowGeometry;

impl ManagedSessionStore {
    /// Borrows canonical content geometry and the addressed session's outer
    /// geometry. An inactive occurrence does not change outer geometry.
    pub fn attached_window_geometry_mut(
        &mut self,
        target: &WindowTarget,
    ) -> Result<AttachedWindowGeometry<'_>, RmuxError> {
        let link = self.target_link(target)?;
        let session = self
            .sessions
            .get_mut(target.session_name())
            .expect("validated session");
        Ok(AttachedWindowGeometry {
            terminal_size: (session.active_window == link.window_index)
                .then_some(&mut session.terminal_size),
            window: self.windows.resolve_mut(link).expect("validated link"),
        })
    }

    /// Validates the occurrence before recording an attach and borrowing its
    /// geometry. Rejected targets leave recency and timestamps untouched.
    pub fn touched_attached_window_geometry_mut(
        &mut self,
        target: &WindowTarget,
    ) -> Result<AttachedWindowGeometry<'_>, RmuxError> {
        self.target_link(target)?;
        self.touch_attached(target.session_name())?;
        self.attached_window_geometry_mut(target)
    }

    /// Validates the occurrence before recording an attach and borrowing its
    /// selection and geometry operation. Window ownership stays canonical.
    pub fn touched_window_switch_mut(
        &mut self,
        target: &WindowTarget,
    ) -> Result<WindowSwitch<'_>, RmuxError> {
        self.target_link(target)?;
        self.touch_attached(target.session_name())?;
        self.window_switch_mut(target)
    }
}

#[cfg(test)]
mod tests;
