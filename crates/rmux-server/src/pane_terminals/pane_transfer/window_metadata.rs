use rmux_proto::{PaneTarget, RmuxError, WindowId, WindowTarget};

use super::super::{session_not_found, HandlerState};

/// Stable pre-mutation identity for window-scoped metadata touched by a pane transfer.
///
/// Window options and automatic-name tracking are stored by winlink slot, while pane
/// transfers operate on stable window identities. Capturing both lets the transfer either
/// discard metadata with a destroyed source window or move it with a surviving window.
pub(super) struct PaneTransferWindowMetadata {
    source: WindowTarget,
    source_aliases: Vec<WindowTarget>,
    window_id: WindowId,
    single_pane: bool,
}

impl PaneTransferWindowMetadata {
    pub(super) fn capture(state: &HandlerState, source: &PaneTarget) -> Result<Self, RmuxError> {
        let session = state
            .sessions
            .session(source.session_name())
            .ok_or_else(|| session_not_found(source.session_name()))?;
        let window = session.window_at(source.window_index()).ok_or_else(|| {
            RmuxError::invalid_target(
                format!("{}:{}", source.session_name(), source.window_index()),
                "window index does not exist in session",
            )
        })?;
        let source =
            WindowTarget::with_window(source.session_name().clone(), source.window_index());
        let mut source_aliases =
            state.window_linked_window_targets(source.session_name(), source.window_index());
        if !source_aliases.contains(&source) {
            source_aliases.push(source.clone());
        }

        Ok(Self {
            source,
            source_aliases,
            window_id: window.id(),
            single_pane: window.pane_count() == 1,
        })
    }

    /// Moves slot-keyed metadata when `break-pane` moved the complete source window.
    pub(super) fn move_to_surviving_window(
        &self,
        state: &mut HandlerState,
        destination: &WindowTarget,
    ) -> Result<(), RmuxError> {
        if !self.single_pane || self.source == *destination {
            return Ok(());
        }
        let destination_window_id = state
            .sessions
            .session(destination.session_name())
            .and_then(|session| session.window_at(destination.window_index()))
            .map(rmux_core::Window::id)
            .ok_or_else(|| {
                RmuxError::invalid_target(
                    destination.to_string(),
                    "break-pane destination window does not exist",
                )
            })?;
        if destination_window_id != self.window_id {
            return Err(RmuxError::Server(format!(
                "break-pane destination {} does not contain moved window @{}",
                destination,
                self.window_id.as_u32()
            )));
        }

        state
            .options
            .move_window_overrides(&self.source, destination);
        state.move_auto_named_window_slot(
            self.source.session_name(),
            self.source.window_index(),
            destination.session_name(),
            destination.window_index(),
        );
        Ok(())
    }

    /// Removes slot-keyed metadata from aliases which no longer name the captured window.
    pub(super) fn prune_removed_aliases(&self, state: &mut HandlerState) {
        for target in &self.source_aliases {
            let occupant = state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .map(rmux_core::Window::id);
            match occupant {
                Some(id) if id == self.window_id => continue,
                // A different window occupies the pre-mutation index: an
                // insertion or renumber shifted it here and the index remap
                // already rewrote the slot's options and auto-name tracking
                // for the new occupant. Deleting by the stale index would
                // destroy that unrelated window's metadata.
                Some(_) => continue,
                // Vacated slot: the captured window is gone and nothing took
                // its index, so the slot-keyed metadata is stale.
                None => {}
            }
            let _ = state.options.remove_window(target);
            state.clear_auto_named_window(target.session_name(), target.window_index());
        }
    }
}
