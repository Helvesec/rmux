use rmux_proto::{RmuxError, TerminalSize, WindowTarget};

use super::HandlerState;

/// One canonical owner's selected geometry, prepared before any batch mutation.
pub(crate) struct AttachedWindowResize {
    pub(crate) target: WindowTarget,
    pub(crate) terminal_size: TerminalSize,
    pub(crate) content_size: TerminalSize,
    pub(crate) constrained_manual: bool,
}

impl HandlerState {
    /// Apply a size declaration's windows through the existing native chokepoint.
    /// A later failure restores earlier owners in place and retracts their events.
    /// Native compensation remains best effort, as for a single-window resize.
    pub(crate) fn resize_attached_windows(
        &mut self,
        resizes: &[AttachedWindowResize],
    ) -> Result<(), RmuxError> {
        let checkpoints = resizes
            .iter()
            .map(|resize| self.sessions.checkpoint_window(&resize.target))
            .collect::<Result<Vec<_>, _>>()?;
        let previous_events = self.applied_window_resizes.clone();
        for (index, resize) in resizes.iter().enumerate() {
            let session = self
                .sessions
                .session(resize.target.session_name())
                .ok_or_else(|| super::session_not_found(resize.target.session_name()))?;
            let session_id = session.id();
            let is_active = session.active_window_index() == resize.target.window_index();
            let mutate = |mut geometry: rmux_core::AttachedWindowGeometry<'_>| {
                if resize.constrained_manual {
                    geometry.mark_manual_size_constrained();
                }
                geometry.resize(resize.terminal_size, resize.content_size);
                Ok(())
            };
            let applied = if is_active {
                self.mutate_touched_attached_window_geometry_and_resize_terminal(
                    &resize.target,
                    session_id,
                    mutate,
                )
            } else {
                self.mutate_attached_window_geometry_and_resize_terminal(&resize.target, mutate)
            };
            if let Err(original) = applied {
                for (resize, checkpoint) in resizes.iter().zip(checkpoints).rev() {
                    self.sessions
                        .restore_window_checkpoint(&resize.target, checkpoint)?;
                }
                self.applied_window_resizes = previous_events;
                let mut compensation_error = None;
                for resize in &resizes[..index] {
                    if let Err(error) = self.resize_window_terminal_runtime(
                        resize.target.session_name(),
                        resize.target.window_index(),
                    ) {
                        compensation_error.get_or_insert(RmuxError::from(error));
                    }
                    for name in self.window_linked_session_family_list(
                        resize.target.session_name(),
                        resize.target.window_index(),
                    ) {
                        self.sync_pane_lifecycle_dimensions_for_session(&name);
                    }
                }
                return Err(match compensation_error {
                    Some(error) => RmuxError::Server(format!(
                        "failed to roll back control window sizes after {original}: {error}"
                    )),
                    None => original,
                });
            }
        }
        Ok(())
    }
}
