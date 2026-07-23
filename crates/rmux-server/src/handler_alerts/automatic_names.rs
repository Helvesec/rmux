//! Automatic window-name refresh and synchronization.

use rmux_proto::{PaneTarget, SessionId, SessionName, Target, WindowId, WindowTarget};

use super::super::{scripting_support::format_context_for_target, RequestHandler};
use crate::format_runtime::render_automatic_window_name;
use crate::pane_terminals::HandlerState;

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
        !self
            .sync_automatic_window_name_for_window_target(&WindowTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
            ))
            .await
            .is_empty()
    }

    pub(in crate::handler) async fn sync_automatic_window_name_for_pane_session_identity(
        &self,
        target: &PaneTarget,
        expected_session_id: SessionId,
    ) -> bool {
        let window_target =
            WindowTarget::with_window(target.session_name().clone(), target.window_index());
        let mut state = self.state.lock().await;
        let Some(window_id) = state
            .sessions
            .session(target.session_name())
            .filter(|session| session.id() == expected_session_id)
            .and_then(|session| session.window_at(target.window_index()))
            .map(rmux_core::Window::id)
        else {
            return false;
        };
        !Self::sync_automatic_window_name_for_window_target_locked(
            &mut state,
            &window_target,
            window_id,
        )
        .is_empty()
    }

    pub(in crate::handler) async fn refresh_automatic_window_name_for_window_target(
        &self,
        target: &WindowTarget,
    ) -> bool {
        let sessions_to_refresh = self
            .sync_automatic_window_name_for_window_target(target)
            .await;
        for session_name in &sessions_to_refresh {
            self.refresh_attached_session(session_name).await;
        }

        !sessions_to_refresh.is_empty()
    }

    async fn sync_automatic_window_name_for_window_target(
        &self,
        target: &WindowTarget,
    ) -> Vec<SessionName> {
        let mut state = self.state.lock().await;
        let Some(window_id) = state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .map(rmux_core::Window::id)
        else {
            return Vec::new();
        };
        Self::sync_automatic_window_name_for_window_target_locked(&mut state, target, window_id)
    }

    pub(in crate::handler) fn sync_automatic_window_name_for_window_target_locked(
        state: &mut HandlerState,
        target: &WindowTarget,
        expected_window_id: WindowId,
    ) -> Vec<SessionName> {
        let target_matches_identity = state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .is_some_and(|window| window.id() == expected_window_id);
        if !target_matches_identity {
            return Vec::new();
        }

        let rendered_window_name =
            format_context_for_target(state, &Target::Window(target.clone()), 0)
                .ok()
                .and_then(|runtime| render_automatic_window_name(&runtime));
        let fallback_window_name = rendered_window_name
            .is_none()
            .then(|| mode_marker_fallback_window_name(state, target))
            .flatten();
        let Some(window_name) = rendered_window_name
            .as_deref()
            .or(fallback_window_name.as_deref())
        else {
            return Vec::new();
        };

        let tracked = state.tracks_auto_named_window(target.session_name(), target.window_index());
        let should_update = {
            let Some(session) = state.sessions.session(target.session_name()) else {
                return Vec::new();
            };
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
                _ => return Vec::new(),
            }
        };
        if !should_update {
            return Vec::new();
        }

        state
            .sessions
            .session_mut(target.session_name())
            .expect("existing session must accept automatic rename update")
            .window_at_mut(target.window_index())
            .expect("existing window must accept automatic rename update")
            .set_automatic_name(window_name.to_owned());
        state.mark_auto_named_window(target.session_name(), target.window_index());
        state
            .synchronize_linked_window_family_from_slot(
                target.session_name(),
                target.window_index(),
            )
            .unwrap_or_else(|_| vec![target.session_name().clone()])
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
