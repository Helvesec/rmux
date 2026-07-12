use rmux_proto::{OptionScopeSelector, RmuxError, SessionId, SessionName, WindowId};

use crate::pane_terminals::{session_not_found, HandlerState};

use super::super::{attach_support::surviving_attached_resize_targets, RequestHandler};

#[derive(Debug, Clone, Copy)]
pub(super) struct StableResizeWindowIdentity {
    session_id: SessionId,
    window_id: WindowId,
}

#[derive(Debug, Clone)]
pub(super) enum ResizePolicyReconcileScope {
    AllSessions(Vec<StableResizeWindowIdentity>),
    Session(StableResizeWindowIdentity),
    ExactWindow(StableResizeWindowIdentity),
}

impl ResizePolicyReconcileScope {
    pub(super) fn capture(
        state: &HandlerState,
        scope: &OptionScopeSelector,
    ) -> Result<Self, RmuxError> {
        match scope {
            OptionScopeSelector::ServerGlobal
            | OptionScopeSelector::SessionGlobal
            | OptionScopeSelector::WindowGlobal => {
                let mut identities = state
                    .sessions
                    .iter()
                    .map(|(_, session)| StableResizeWindowIdentity {
                        session_id: session.id(),
                        window_id: session.window().id(),
                    })
                    .collect::<Vec<_>>();
                identities.sort_by_key(|identity| identity.session_id);
                Ok(Self::AllSessions(identities))
            }
            OptionScopeSelector::Session(session_name) => {
                let session = state
                    .sessions
                    .session(session_name)
                    .ok_or_else(|| session_not_found(session_name))?;
                Ok(Self::Session(StableResizeWindowIdentity {
                    session_id: session.id(),
                    window_id: session.window().id(),
                }))
            }
            OptionScopeSelector::Window(target) => capture_exact_window(
                state,
                target.session_name(),
                target.window_index(),
                target.to_string(),
            ),
            OptionScopeSelector::Pane(target) => capture_exact_window(
                state,
                target.session_name(),
                target.window_index(),
                target.to_string(),
            ),
        }
    }

    pub(super) const fn is_exact_window(&self) -> bool {
        matches!(self, Self::ExactWindow(_))
    }

    pub(super) const fn is_stable_session(&self) -> bool {
        matches!(self, Self::Session(_))
    }
}

impl RequestHandler {
    pub(super) async fn reconcile_attached_sizes_for_option_scope(
        &self,
        scope: &ResizePolicyReconcileScope,
    ) -> Vec<SessionName> {
        match scope {
            ResizePolicyReconcileScope::ExactWindow(identity) => {
                let _ = self
                    .reconcile_attached_window_identity_size_and_emit(
                        identity.session_id,
                        identity.window_id,
                    )
                    .await;
                self.stable_option_resize_refresh_sessions(identity.session_id, identity.window_id)
                    .await
            }
            ResizePolicyReconcileScope::AllSessions(identities) => {
                for identity in identities {
                    let _ = self
                        .reconcile_attached_window_identity_size_and_emit(
                            identity.session_id,
                            identity.window_id,
                        )
                        .await;
                }
                Vec::new()
            }
            ResizePolicyReconcileScope::Session(identity) => {
                let _ = self
                    .reconcile_attached_window_identity_size_and_emit(
                        identity.session_id,
                        identity.window_id,
                    )
                    .await;
                self.current_resize_session_name(identity.session_id)
                    .await
                    .into_iter()
                    .collect()
            }
        }
    }

    async fn current_resize_session_name(&self, session_id: SessionId) -> Option<SessionName> {
        let state = self.state.lock().await;
        let session_name = state.sessions.iter().find_map(|(session_name, session)| {
            (session.id() == session_id).then(|| session_name.clone())
        });
        session_name
    }

    async fn stable_option_resize_refresh_sessions(
        &self,
        session_id: SessionId,
        window_id: WindowId,
    ) -> Vec<SessionName> {
        let state = self.state.lock().await;
        let identity_survives = state
            .sessions
            .session_by_id(session_id)
            .is_some_and(|session| {
                session
                    .windows()
                    .values()
                    .any(|window| window.id() == window_id)
            });
        if !identity_survives {
            return Vec::new();
        }

        let targets = surviving_attached_resize_targets(&state, [window_id]);
        let mut refresh_sessions = targets
            .iter()
            .flat_map(|target| {
                state
                    .window_linked_session_family_list(target.session_name(), target.window_index())
            })
            .collect::<Vec<_>>();
        refresh_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        refresh_sessions.dedup();
        refresh_sessions
    }
}

fn capture_exact_window(
    state: &HandlerState,
    session_name: &SessionName,
    window_index: u32,
    target_display: String,
) -> Result<ResizePolicyReconcileScope, RmuxError> {
    let session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| session_not_found(session_name))?;
    let window = session.window_at(window_index).ok_or_else(|| {
        RmuxError::invalid_target(target_display, "window index does not exist in session")
    })?;
    Ok(ResizePolicyReconcileScope::ExactWindow(
        StableResizeWindowIdentity {
            session_id: session.id(),
            window_id: window.id(),
        },
    ))
}
