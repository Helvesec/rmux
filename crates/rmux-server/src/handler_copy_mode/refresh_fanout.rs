use std::collections::HashSet;

use rmux_core::LifecycleEvent;
use rmux_proto::{PaneTarget, RmuxError, SessionId, SessionName};

use super::RequestHandler;
use crate::pane_terminals::HandlerState;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CopyModeRefreshSessionIdentity {
    session_name: SessionName,
    session_id: SessionId,
}

impl RequestHandler {
    pub(super) async fn prepare_copy_mode_refresh_fanout(
        &self,
        target: &PaneTarget,
        expected_session_id: SessionId,
        mode_changed: bool,
    ) -> Result<Vec<CopyModeRefreshSessionIdentity>, RmuxError> {
        if mode_changed {
            self.sync_automatic_window_name_for_pane_session_identity(target, expected_session_id)
                .await;
        }

        let (identities, event) = {
            let mut state = self.state.lock().await;
            let identities =
                capture_refresh_session_identities(&state, target, expected_session_id)?;
            let event = mode_changed.then(|| {
                super::super::prepare_lifecycle_event(
                    &mut state,
                    &LifecycleEvent::PaneModeChanged {
                        target: target.clone(),
                    },
                )
            });
            (identities, event)
        };
        if let Some(event) = event {
            self.emit_prepared(event).await;
        }
        Ok(identities)
    }

    pub(super) async fn refresh_copy_mode_session_identities(
        &self,
        identities: Vec<CopyModeRefreshSessionIdentity>,
    ) {
        for identity in identities {
            self.refresh_attached_session_for_session_identity(
                &identity.session_name,
                identity.session_id,
            )
            .await;
        }
    }
}

fn capture_refresh_session_identities(
    state: &HandlerState,
    target: &PaneTarget,
    expected_session_id: SessionId,
) -> Result<Vec<CopyModeRefreshSessionIdentity>, RmuxError> {
    let (window_id, pane_id) = state
        .sessions
        .session(target.session_name())
        .filter(|session| session.id() == expected_session_id)
        .and_then(|session| {
            session.window_at(target.window_index()).and_then(|window| {
                window
                    .pane(target.pane_index())
                    .map(|pane| (window.id(), pane.id()))
            })
        })
        .ok_or_else(copy_mode_refresh_identity_error)?;

    let mut seen = HashSet::new();
    let mut identities = state
        .window_linked_window_targets(target.session_name(), target.window_index())
        .into_iter()
        .filter_map(|alias| {
            let session = state.sessions.session(alias.session_name())?;
            let window = session.window_at(alias.window_index())?;
            if window.id() != window_id
                || window
                    .pane(target.pane_index())
                    .is_none_or(|pane| pane.id() != pane_id)
                || !seen.insert(session.id())
            {
                return None;
            }
            Some(CopyModeRefreshSessionIdentity {
                session_name: alias.session_name().clone(),
                session_id: session.id(),
            })
        })
        .collect::<Vec<_>>();
    if !seen.contains(&expected_session_id) {
        return Err(copy_mode_refresh_identity_error());
    }
    identities.sort_by(|left, right| {
        left.session_name
            .as_str()
            .cmp(right.session_name.as_str())
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    Ok(identities)
}

fn copy_mode_refresh_identity_error() -> RmuxError {
    RmuxError::Server("copy-mode pane identity changed before refresh".to_owned())
}
