//! Automatic window-name refresh and synchronization.

use rmux_core::LifecycleEvent;
use rmux_proto::{PaneTarget, SessionId, SessionName, Target, WindowId, WindowTarget};

use super::super::{
    prepare_unsequenced_lifecycle_event, scripting_support::format_context_for_target,
    sequence_prepared_lifecycle_event, RequestHandler, UnsequencedLifecycleEvent,
};
use crate::format_runtime::render_automatic_window_name;
use crate::pane_terminals::HandlerState;

pub(in crate::handler) struct AutomaticWindowNameChange {
    pub(in crate::handler) refresh_sessions: Vec<SessionName>,
    pub(in crate::handler) lifecycle_event: UnsequencedLifecycleEvent,
}

impl RequestHandler {
    pub(in crate::handler) async fn refresh_automatic_window_name_for_pane_target(
        &self,
        target: &PaneTarget,
    ) -> bool {
        self.refresh_automatic_window_name_for_window_target(&WindowTarget::with_window(
            target.session_name().clone(),
            target.window_index(),
        ))
        .await
    }

    pub(in crate::handler) async fn sync_automatic_window_name_for_pane_target(
        &self,
        target: &PaneTarget,
    ) -> bool {
        self.sync_automatic_window_name_for_window_target(&WindowTarget::with_window(
            target.session_name().clone(),
            target.window_index(),
        ))
        .await
        .is_some()
    }

    pub(in crate::handler) async fn sync_automatic_window_name_for_pane_session_identity(
        &self,
        target: &PaneTarget,
        expected_session_id: SessionId,
    ) -> bool {
        let window_target =
            WindowTarget::with_window(target.session_name().clone(), target.window_index());
        self.sync_automatic_window_name_for_window_identity(
            &window_target,
            Some(expected_session_id),
        )
        .await
        .is_some()
    }

    pub(in crate::handler) async fn refresh_automatic_window_name_for_window_target(
        &self,
        target: &WindowTarget,
    ) -> bool {
        let Some(sessions_to_refresh) = self
            .sync_automatic_window_name_for_window_target(target)
            .await
        else {
            return false;
        };
        for session_name in &sessions_to_refresh {
            self.refresh_attached_session(session_name).await;
        }

        true
    }

    async fn sync_automatic_window_name_for_window_target(
        &self,
        target: &WindowTarget,
    ) -> Option<Vec<SessionName>> {
        self.sync_automatic_window_name_for_window_identity(target, None)
            .await
    }

    async fn sync_automatic_window_name_for_window_identity(
        &self,
        target: &WindowTarget,
        expected_session_id: Option<SessionId>,
    ) -> Option<Vec<SessionName>> {
        let (refresh_sessions, lifecycle_event) = {
            let mut state = self.state.lock().await;
            let window_id = state
                .sessions
                .session(target.session_name())
                .filter(|session| {
                    expected_session_id.is_none_or(|expected| session.id() == expected)
                })
                .and_then(|session| session.window_at(target.window_index()))
                .map(rmux_core::Window::id)?;
            let change = Self::sync_automatic_window_name_for_window_target_locked(
                &mut state, target, window_id,
            )?;
            let lifecycle_event =
                sequence_prepared_lifecycle_event(&mut state, change.lifecycle_event);
            (change.refresh_sessions, lifecycle_event)
        };
        self.emit_prepared(lifecycle_event).await;
        Some(refresh_sessions)
    }

    pub(in crate::handler) fn sync_automatic_window_name_for_window_target_locked(
        state: &mut HandlerState,
        target: &WindowTarget,
        expected_window_id: WindowId,
    ) -> Option<AutomaticWindowNameChange> {
        let target_matches_identity = state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .is_some_and(|window| window.id() == expected_window_id);
        if !target_matches_identity {
            return None;
        }

        let rendered_window_name =
            format_context_for_target(state, &Target::Window(target.clone()), 0)
                .ok()
                .and_then(|runtime| render_automatic_window_name(&runtime));
        let fallback_window_name = rendered_window_name
            .is_none()
            .then(|| mode_marker_fallback_window_name(state, target))
            .flatten();
        let window_name = rendered_window_name
            .as_deref()
            .or(fallback_window_name.as_deref())?;

        let tracked = state.tracks_auto_named_window(target.session_name(), target.window_index());
        let should_update = {
            let session = state.sessions.session(target.session_name())?;
            match session.window_at(target.window_index()) {
                Some(window) if window.id() == expected_window_id => {
                    !window_name.is_empty()
                        && window.name() != Some(window_name)
                        && crate::automatic_rename::window_allows_automatic_rename(
                            &state.options,
                            target.session_name(),
                            target.window_index(),
                            window,
                            tracked,
                        )
                }
                _ => return None,
            }
        };
        if !should_update {
            return None;
        }

        let _ = state
            .sessions
            .set_automatic_window_name(target, window_name.to_owned())
            .expect("existing window must accept automatic rename update");
        state.mark_auto_named_window(target.session_name(), target.window_index());
        let refresh_sessions = state
            .synchronize_linked_window_family_from_slot(
                target.session_name(),
                target.window_index(),
            )
            .unwrap_or_else(|_| vec![target.session_name().clone()]);
        let lifecycle_event = prepare_unsequenced_lifecycle_event(
            state,
            &LifecycleEvent::WindowRenamed {
                target: target.clone(),
            },
        );
        Some(AutomaticWindowNameChange {
            refresh_sessions,
            lifecycle_event,
        })
    }
}

fn mode_marker_fallback_window_name(state: &HandlerState, target: &WindowTarget) -> Option<String> {
    let session = state.sessions.session(target.session_name())?;
    let window = session.window_at(target.window_index())?;
    if window.name() != Some("[tmux]")
        || !state.tracks_auto_named_window(target.session_name(), target.window_index())
    {
        return None;
    }

    let pane = window.active_pane()?;
    state
        .pane_runtime_window_name_in_window(
            target.session_name(),
            target.window_index(),
            pane.index(),
        )
        .ok()
        .flatten()
        .or_else(|| {
            state
                .pane_profile_in_window(target.session_name(), target.window_index(), pane.index())
                .ok()
                .and_then(|profile| {
                    profile
                        .shell()
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned)
                })
        })
        .filter(|name| !name.is_empty() && name != "[tmux]")
}
