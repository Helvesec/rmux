//! Automatic window-name refresh and synchronization.

use rmux_proto::{PaneTarget, SessionName, Target, WindowTarget};

use super::super::{scripting_support::format_context_for_target, RequestHandler};
use crate::format_runtime::render_automatic_window_name;

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
        let rendered_window_name = {
            let state = self.state.lock().await;
            format_context_for_target(&state, &Target::Window(target.clone()), 0)
                .ok()
                .and_then(|runtime| render_automatic_window_name(&runtime))
        };
        let Some(window_name) = rendered_window_name.as_deref() else {
            return Vec::new();
        };

        let sessions_to_refresh = {
            let mut state = self.state.lock().await;
            let tracked =
                state.tracks_auto_named_window(target.session_name(), target.window_index());
            let should_update = {
                let Some(session) = state.sessions.session(target.session_name()) else {
                    return Vec::new();
                };
                match session.window_at(target.window_index()) {
                    Some(window) => {
                        !window_name.is_empty()
                            && window.name() != Some(window_name)
                            && (tracked || window.automatic_rename() || window.name().is_none())
                    }
                    None => return Vec::new(),
                }
            };
            if !should_update {
                Vec::new()
            } else {
                state
                    .sessions
                    .session_mut(target.session_name())
                    .expect("existing session must accept automatic rename update")
                    .window_at_mut(target.window_index())
                    .expect("existing window must accept automatic rename update")
                    .set_automatic_name(window_name.to_owned());
                state.mark_auto_named_window(target.session_name(), target.window_index());
                let _ = state.synchronize_linked_window_from_slot(
                    target.session_name(),
                    target.window_index(),
                );
                state
                    .synchronize_session_group_from(target.session_name())
                    .unwrap_or_else(|_| vec![target.session_name().clone()])
            }
        };
        sessions_to_refresh
    }
}
