use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{KillSessionRequest, OptionName, SessionId, SessionName};

use crate::handler::RequestHandler;

impl RequestHandler {
    pub(in crate::handler) async fn destroy_unattached_sessions_for_option_scope(
        &self,
        scope: &OptionScopeSelector,
    ) {
        let mut candidates = {
            let state = self.state.lock().await;
            destroy_unattached_candidate_sessions(&state, scope)
        };
        self.destroy_unattached_sessions(std::mem::take(&mut candidates))
            .await;
    }

    /// Resolve the current name by identity after switch hooks and geometry
    /// notifications: a renamed source is still ours, a recreated one is not.
    pub(in crate::handler) async fn destroy_unattached_session_identity(
        &self,
        session_id: SessionId,
    ) {
        let candidate = {
            let state = self.state.lock().await;
            state
                .sessions
                .session_by_id(session_id)
                .map(|session| (session.name().clone(), session_id))
        };
        if let Some(candidate) = candidate {
            // Destroying a session can itself relocate clients. Keep that
            // asynchronous cleanup cycle behind a boxed future.
            Box::pin(self.destroy_unattached_sessions(vec![candidate])).await;
        }
    }

    pub(in crate::handler) async fn destroy_unattached_sessions(
        &self,
        mut candidates: Vec<(SessionName, SessionId)>,
    ) {
        candidates.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
        candidates.dedup();

        for (session_name, session_id) in candidates {
            if !self
                .session_should_destroy_when_unattached(&session_name, session_id)
                .await
            {
                continue;
            }
            if self
                .attached_count_for_session_identity(&session_name, session_id)
                .await
                != 0
            {
                continue;
            }
            let _ = self
                .handle_kill_session_identity(
                    KillSessionRequest {
                        target: session_name,
                        kill_all_except_target: false,
                        clear_alerts: false,
                        kill_group: false,
                    },
                    session_id,
                )
                .await;
        }
        self.refresh_hook_identity_aliases().await;
    }

    async fn session_should_destroy_when_unattached(
        &self,
        session_name: &SessionName,
        session_id: SessionId,
    ) -> bool {
        let state = self.state.lock().await;
        state
            .sessions
            .session(session_name)
            .is_some_and(|session| session.id() == session_id)
            && state
                .options
                .resolve(Some(session_name), OptionName::DestroyUnattached)
                == Some("on")
    }
}

fn destroy_unattached_candidate_sessions(
    state: &crate::pane_terminals::HandlerState,
    scope: &OptionScopeSelector,
) -> Vec<(SessionName, SessionId)> {
    match scope {
        OptionScopeSelector::ServerGlobal
        | OptionScopeSelector::SessionGlobal
        | OptionScopeSelector::WindowGlobal => state
            .sessions
            .iter()
            .map(|session| (session.name(), session))
            .map(|(session_name, session)| (session_name.clone(), session.id()))
            .collect(),
        OptionScopeSelector::Session(session_name) => state
            .sessions
            .session(session_name)
            .map(|session| vec![(session_name.clone(), session.id())])
            .unwrap_or_default(),
        OptionScopeSelector::Window(target) => state
            .sessions
            .session(target.session_name())
            .map(|session| vec![(target.session_name().clone(), session.id())])
            .unwrap_or_default(),
        OptionScopeSelector::Pane(target) => state
            .sessions
            .session(target.session_name())
            .map(|session| vec![(target.session_name().clone(), session.id())])
            .unwrap_or_default(),
    }
}
