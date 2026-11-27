//! Locked application of the ordered pane window-rename sequence.

use rmux_core::LifecycleEvent;
use rmux_proto::WindowTarget;

use super::super::super::{
    prepare_unsequenced_lifecycle_event, sequence_prepared_lifecycle_event, RequestHandler,
};
use super::AppliedWindowRenames;
use crate::pane_rename_queue::QueuedWindowRename;

impl RequestHandler {
    /// Applies the ordered rename sequence, in the order it was observed in,
    /// inside the transaction that extracted it.
    ///
    /// Both the ordinary flush and a pane exit call this while they still hold
    /// the dispatch and state locks they took the requests with, so the names
    /// are mutated and their lifecycle events sequenced before either can
    /// release anything. Applying after unlocking would let the other path
    /// mutate first and reserve the earlier publication ranks, and no later
    /// sequencing can repair an order decided that way.
    ///
    /// Each request carries the window that owned its pane when the request
    /// was accepted, and that owner is what decides where it lands: a request
    /// accepted in one window renames nothing once its pane has been moved to
    /// another, even though the transfer preserved the pane id and the output
    /// generation. The pane's live identity is re-resolved for every request
    /// too, so a respawned or destroyed pane renames nothing either.
    pub(super) fn apply_window_rename_sequence_locked(
        state: &mut crate::pane_terminals::HandlerState,
        requests: Vec<QueuedWindowRename>,
    ) -> AppliedWindowRenames {
        let mut applied_renames = AppliedWindowRenames::default();
        if requests.is_empty() {
            return applied_renames;
        }
        let mut applied = 0_usize;
        let mut rejected = 0_usize;
        for request in requests {
            let Some(window_target) = Self::resolve_rename_owner_locked(state, &request) else {
                rejected += 1;
                continue;
            };
            let outcome = match state.apply_window_rename_request(&window_target, &request.name) {
                Ok(outcome) => outcome,
                Err(error) => {
                    rejected += 1;
                    tracing::warn!(
                        session = %window_target.session_name(),
                        window = window_target.window_index(),
                        pane_id = request.pane_id.as_u32(),
                        "failed to apply a pane window-rename request: {error}"
                    );
                    continue;
                }
            };
            applied += 1;
            applied_renames
                .refresh_sessions
                .extend(outcome.refresh_sessions);
            if !outcome.renamed {
                continue;
            }
            let lifecycle_event = prepare_unsequenced_lifecycle_event(
                state,
                &LifecycleEvent::WindowRenamed {
                    target: window_target,
                },
            );
            applied_renames
                .lifecycle_events
                .push(sequence_prepared_lifecycle_event(state, lifecycle_event));
        }
        tracing::debug!(
            applied,
            rejected,
            "applied a batch of the pane window-rename sequence"
        );
        applied_renames
    }

    /// Resolves where an accepted request may still be applied.
    ///
    /// The pane must still exist under the runtime session and generation the
    /// request was published with, and the window it is in now must be the one
    /// that owned it when the request was accepted.
    fn resolve_rename_owner_locked(
        state: &crate::pane_terminals::HandlerState,
        request: &QueuedWindowRename,
    ) -> Option<WindowTarget> {
        let runtime_session = state.resolve_pane_event_runtime_session(
            &request.session_name,
            request.pane_id,
            request.generation,
        )?;
        let pane_target = state.pane_target_for_runtime_pane(&runtime_session, request.pane_id)?;
        let window = state
            .sessions
            .session(pane_target.session_name())?
            .window_at(pane_target.window_index())?;
        if window.id() != request.owner {
            return None;
        }
        window.pane(pane_target.pane_index())?;
        Some(WindowTarget::with_window(
            pane_target.session_name().clone(),
            pane_target.window_index(),
        ))
    }
}
