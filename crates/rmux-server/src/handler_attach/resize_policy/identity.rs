use std::sync::atomic::Ordering;

use rmux_proto::{OptionName, RmuxError, SessionId, WindowId, WindowTarget};

use crate::pane_terminals::HandlerState;

use super::super::super::{QueuedLifecycleEvent, RequestHandler};
use super::{
    attached_size_candidates, control_size_candidates, linked_session_identities,
    policy_from_option_value, prepare_applied_window_resize_events, selected_client_size,
    AttachedSizeSelection, ATTACHED_SIZE_RECONCILE_ATTEMPTS,
};

impl RequestHandler {
    pub(in crate::handler) async fn reconcile_attached_session_identity_size_and_emit(
        &self,
        session_id: SessionId,
    ) -> Result<(), RmuxError> {
        let active_window_id = {
            let state = self.state.lock().await;
            let active_window_id = state
                .sessions
                .iter()
                .find(|(_, session)| session.id() == session_id)
                .map(|(_, session)| session.window().id());
            active_window_id
        };
        let Some(active_window_id) = active_window_id else {
            return Ok(());
        };
        self.reconcile_attached_window_identity_size_and_emit(session_id, active_window_id)
            .await
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
            let Some(selected_size) = selection.selected_size else {
                return Ok(Vec::new());
            };
            let current_size = state
                .sessions
                .session(target.session_name())
                .expect("stable resize session identity was revalidated")
                .window_at(target.window_index())
                .expect("stable resize window identity was revalidated")
                .size();
            if current_size == selected_size {
                return Ok(Vec::new());
            }
            self.pause_before_attached_size_apply().await;
            let window_index = target.window_index();
            state.mutate_session_and_resize_window_terminal(
                target.session_name(),
                window_index,
                |session| {
                    session.resize_window(window_index, selected_size)?;
                    Ok(())
                },
            )?;
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
        let (target, policy, aggressive_resize, linked_sessions) = {
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
            let linked_sessions = linked_session_identities(
                &state,
                target.session_name(),
                target.window_index(),
                aggressive_resize,
            );
            (target, policy, aggressive_resize, linked_sessions)
        };
        let (candidates, control_candidates, active_attach_epoch) = {
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            let candidates = attached_size_candidates(&active_attach, &linked_sessions, None);
            let control_candidates =
                control_size_candidates(&active_control, &linked_sessions, None);
            (
                candidates,
                control_candidates,
                self.active_attach_epoch.load(Ordering::Acquire),
            )
        };
        let selection = AttachedSizeSelection {
            selected_size: selected_client_size(policy, candidates, &control_candidates),
            session_id,
            active_window_index: target.window_index(),
            active_window_id: window_id,
            policy,
            aggressive_resize,
            linked_sessions,
            active_attach_epoch,
            incoming_client_size: None,
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
        .find(|(_, session)| session.id() == session_id)
        .and_then(|(session_name, session)| {
            session
                .windows()
                .iter()
                .filter(|(_, window)| window.id() == window_id)
                .min_by_key(|(window_index, _)| *window_index)
                .map(|(window_index, _)| {
                    WindowTarget::with_window(session_name.clone(), *window_index)
                })
        })
}
