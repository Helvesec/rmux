use super::*;
use crate::window::WindowCheckpoint;

/// Undo data for a nonstructural mutation of one canonical window and occurrence.
/// The registered owner stays in its original allocation throughout rollback.
pub struct ManagedWindowCheckpoint {
    session_id: SessionId,
    window_id: WindowId,
    window: WindowCheckpoint,
    terminal_size: TerminalSize,
    active_window: u32,
    last_window: Option<u32>,
    window_stack: Vec<u32>,
    group_initial_window_id: Option<WindowId>,
    alerts: BTreeMap<u32, AlertFlags>,
    activity_at: i64,
    last_attached_at: Option<i64>,
    recency: SessionRecency,
}

impl ManagedSessionStore {
    /// Captures bounded undo data for layout, selection or attached geometry.
    pub fn checkpoint_window(
        &self,
        target: &WindowTarget,
    ) -> Result<ManagedWindowCheckpoint, RmuxError> {
        let window = self.window(target)?;
        let session = &self.sessions[target.session_name()];
        Ok(ManagedWindowCheckpoint {
            session_id: session.id,
            window_id: window.id(),
            window: window.checkpoint(),
            terminal_size: session.terminal_size,
            active_window: session.active_window,
            last_window: session.last_window,
            window_stack: session.window_stack.clone(),
            group_initial_window_id: session.group_initial_window_id,
            alerts: session.winlink_alert_flags.clone(),
            activity_at: session.activity_at,
            last_attached_at: session.last_attached_at,
            recency: session.recency,
        })
    }

    /// Restores nonstructural undo data after validating both stable identities.
    /// Structural mutation must use its own validated commit/rollback outcome.
    pub fn restore_window_checkpoint(
        &mut self,
        target: &WindowTarget,
        checkpoint: ManagedWindowCheckpoint,
    ) -> Result<(), RmuxError> {
        let link = self.target_link(target)?;
        if link.session_id != checkpoint.session_id
            || self.window(target)?.id() != checkpoint.window_id
        {
            return Err(RmuxError::Server(
                "window checkpoint identity changed".to_owned(),
            ));
        }
        self.windows
            .resolve_mut(link)
            .expect("validated link")
            .restore_checkpoint(checkpoint.window);
        let session = self
            .sessions
            .get_mut(target.session_name())
            .expect("validated session");
        session.terminal_size = checkpoint.terminal_size;
        session.active_window = checkpoint.active_window;
        session.last_window = checkpoint.last_window;
        session.window_stack = checkpoint.window_stack;
        session.group_initial_window_id = checkpoint.group_initial_window_id;
        session.winlink_alert_flags = checkpoint.alerts;
        session.activity_at = checkpoint.activity_at;
        session.last_attached_at = checkpoint.last_attached_at;
        session.recency = checkpoint.recency;
        Ok(())
    }
}
