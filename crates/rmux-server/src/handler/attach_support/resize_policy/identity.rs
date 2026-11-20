use std::sync::atomic::Ordering;

use rmux_proto::{OptionName, RmuxError, SessionId, WindowId, WindowTarget};

use crate::pane_terminals::HandlerState;

use super::super::super::{QueuedLifecycleEvent, RequestHandler};
use super::{
    attached_size_candidates, control_size_candidates, linked_session_statuses,
    policy_from_option_value, prepare_applied_window_resize_events, selected_client_size,
    AttachedSizeSelection, ATTACHED_SIZE_RECONCILE_ATTEMPTS,
};

impl RequestHandler {
    pub(in crate::handler) async fn reconcile_attached_session_identity_size_and_emit(
        &self,
        session_id: SessionId,
    ) -> Result<(), RmuxError> {
        let window_ids = {
            let state = self.state.lock().await;
            let ids = state
                .sessions
                .iter()
                .find(|session| session.id() == session_id)
                .map(|session| {
                    session
                        .windows()
                        .map(|(_, window)| window.id())
                        .collect::<std::collections::HashSet<_>>()
                })
                .unwrap_or_default();
            ids
        };
        let mut window_ids = window_ids.into_iter().collect::<Vec<_>>();
        window_ids.sort();
        for window_id in window_ids {
            self.reconcile_attached_window_identity_size_and_emit(session_id, window_id)
                .await?;
        }
        Ok(())
    }

    pub(in crate::handler) async fn reconcile_attached_window_identity_size_and_emit(
        &self,
        session_id: SessionId,
        window_id: WindowId,
    ) -> Result<(), RmuxError> {
        let applied = self
            .reconcile_attached_window_identity_size(session_id, window_id)
            .await?;
        if applied.is_empty() {
            return Ok(());
        }
        self.pause_before_window_lifecycle_emit().await;
        for event in applied {
            self.emit_prepared(event).await;
        }
        Ok(())
    }

    async fn reconcile_attached_window_identity_size(
        &self,
        session_id: SessionId,
        window_id: WindowId,
    ) -> Result<Vec<QueuedLifecycleEvent>, RmuxError> {
        for _ in 0..ATTACHED_SIZE_RECONCILE_ATTEMPTS {
            let Some((target, selection)) = self
                .selected_attached_window_identity_size(session_id, window_id)
                .await
            else {
                return Ok(Vec::new());
            };
            self.pause_after_attached_size_selection().await;

            let mut state = self.state.lock().await;
            if window_target_for_identity(&state, session_id, window_id).as_ref() != Some(&target) {
                continue;
            }
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            if !self.attached_size_selection_is_current(
                &state,
                &active_attach,
                &active_control,
                target.session_name(),
                &selection,
                false,
            ) {
                continue;
            }
            if selection.selected_size().is_none() {
                return Ok(Vec::new());
            }
            let session = state
                .sessions
                .session(target.session_name())
                .expect("stable resize session identity was revalidated");
            if selection.matches_window(session, target.window_index()) {
                return Ok(Vec::new());
            }
            self.pause_before_attached_size_apply().await;
            state.mutate_attached_window_geometry_and_resize_terminal(&target, |geometry| {
                selection.apply_to_attached_geometry(geometry);
                Ok(())
            })?;
            drop(active_control);
            drop(active_attach);
            // tmux 3.7b, measured 2026-07-25: a reconcile that actually resizes
            // the window notifies the window's control clients with
            // `%layout-change` and runs `window-layout-changed` before
            // `window-resized`. Reserve the tickets the geometry chokepoint just
            // recorded under the same state lock so the published order matches.
            return Ok(prepare_applied_window_resize_events(&mut state));
        }
        Ok(Vec::new())
    }

    async fn selected_attached_window_identity_size(
        &self,
        session_id: SessionId,
        window_id: WindowId,
    ) -> Option<(WindowTarget, AttachedSizeSelection)> {
        let (target, policy, status, aggressive_resize, linked_sessions, manual_size) = {
            let state = self.state.lock().await;
            let target = window_target_for_identity(&state, session_id, window_id)?;
            let policy = policy_from_option_value(state.options.resolve_for_window(
                target.session_name(),
                target.window_index(),
                OptionName::WindowSize,
            ));
            let aggressive_resize = state.options.resolve_for_window(
                target.session_name(),
                target.window_index(),
                OptionName::AggressiveResize,
            ) == Some("on");
            let linked_sessions = linked_session_statuses(
                &state,
                target.session_name(),
                target.window_index(),
                aggressive_resize,
            );
            let status = state
                .options
                .resolve(Some(target.session_name()), OptionName::Status)
                .map(str::to_owned);
            let manual_size = state
                .sessions
                .session(target.session_name())?
                .window_at(target.window_index())?
                .manual_size();
            (
                target,
                policy,
                status,
                aggressive_resize,
                linked_sessions,
                manual_size,
            )
        };
        let (candidates, control_candidates, active_attach_epoch) = {
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            let candidates = attached_size_candidates(&active_attach, &linked_sessions, None, None);
            let control_candidates =
                control_size_candidates(&active_control, &linked_sessions, None, window_id);
            (
                candidates,
                control_candidates,
                self.active_attach_epoch.load(Ordering::Acquire),
            )
        };
        let selection = AttachedSizeSelection {
            selected_size: selected_client_size(
                policy,
                candidates,
                &control_candidates,
                manual_size,
            ),
            session_id,
            active_window_index: target.window_index(),
            active_window_id: window_id,
            policy,
            manual_size,
            status,
            aggressive_resize,
            linked_sessions,
            active_attach_epoch,
            incoming_client: None,
            control_candidates,
        };
        Some((target, selection))
    }
}

fn window_target_for_identity(
    state: &HandlerState,
    session_id: SessionId,
    window_id: WindowId,
) -> Option<WindowTarget> {
    state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
        .find(|(_, session)| session.id() == session_id)
        .and_then(|(session_name, session)| {
            session
                .windows()
                .filter(|(_, window)| window.id() == window_id)
                .min_by_key(|(window_index, _)| *window_index)
                .map(|(window_index, _)| {
                    WindowTarget::with_window(session_name.clone(), window_index)
                })
        })
}
