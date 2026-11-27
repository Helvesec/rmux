use super::*;

impl ManagedSessionStore {
    /// Projects the replacement pane without changing the canonical owner.
    pub fn preview_respawn_window(
        &self,
        target: &WindowTarget,
        pane_id: PaneId,
    ) -> Result<Window, RmuxError> {
        let mut window = self.window(target)?.clone();
        validate_retained_pane(&window, pane_id)?;
        window.respawn(pane_id);
        Ok(window)
    }

    /// Resets the existing canonical allocation, retaining a pane identity from
    /// that window. Selection is a separate session-local operation.
    pub fn respawn_window(
        &mut self,
        target: &WindowTarget,
        pane_id: PaneId,
    ) -> Result<(), RmuxError> {
        let link = self.target_link(target)?;
        let window = self.windows.resolve_mut(link).expect("validated link");
        validate_retained_pane(window, pane_id)?;
        window.respawn(pane_id);
        Ok(())
    }
}

fn validate_retained_pane(window: &Window, pane_id: PaneId) -> Result<(), RmuxError> {
    if !window.panes().iter().any(|pane| pane.id() == pane_id) {
        return Err(RmuxError::Server(
            "respawn pane identity is not in the addressed window".to_owned(),
        ));
    }
    Ok(())
}
