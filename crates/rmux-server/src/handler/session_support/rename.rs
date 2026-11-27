use rmux_core::LifecycleEvent;
use rmux_proto::{ErrorResponse, Response, RmuxError};

use super::super::{resolve_existing_session_target, RequestHandler};
use super::session_pane_ids;

impl RequestHandler {
    pub(in crate::handler) async fn handle_rename_session(
        &self,
        request: rmux_proto::RenameSessionRequest,
    ) -> Response {
        let (session_name, session_id) = {
            let state = self.state.lock().await;
            let session_name = match resolve_existing_session_target(
                &state.sessions,
                "rename-session",
                &request.target,
            ) {
                Ok(session_name) => session_name,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let session_id = state
                .sessions
                .session(&session_name)
                .expect("resolved session must exist")
                .id();
            (session_name, session_id)
        };
        #[cfg(test)]
        self.pause_after_rename_session_identity_capture(&session_name)
            .await;
        let new_name = request.new_name;
        if session_name == new_name {
            return Response::RenameSession(rmux_proto::RenameSessionResponse { session_name });
        }
        let mut sessions_remembering_renamed = Vec::new();
        let response = {
            let mut state = self.state.lock().await;
            if state
                .sessions
                .session(&session_name)
                .is_none_or(|session| session.id() != session_id)
            {
                return Response::Error(ErrorResponse {
                    error: RmuxError::SessionNotFound(session_name.to_string()),
                });
            }
            if state.sessions.contains_session(&new_name) {
                return Response::Error(ErrorResponse {
                    error: RmuxError::DuplicateSession(new_name.to_string()),
                });
            }
            let previous_subscription_keys = {
                let mut seen = std::collections::HashSet::new();
                session_pane_ids(
                    state
                        .sessions
                        .session(&session_name)
                        .expect("validated rename source must exist"),
                )
                .into_iter()
                .filter(|pane_id| seen.insert(*pane_id))
                .filter_map(|pane_id| state.pane_output_subscription_key_for_pane_id(pane_id))
                .collect::<Vec<_>>()
            };

            match state.rename_session(&session_name, &new_name) {
                Ok(()) => {
                    let subscription_rekeys = previous_subscription_keys
                        .into_iter()
                        .filter_map(|previous| {
                            state
                                .pane_output_subscription_key_for_pane_id(previous.pane_id())
                                .filter(|current| current != &previous)
                                .map(|current| (previous, current))
                        })
                        .collect::<Vec<_>>();
                    // The live subscribe path takes the same state ->
                    // subscriptions lock order, so no late old-name record
                    // can land after this rename commits its rekeys.
                    self.rekey_pane_output_subscriptions(&subscription_rekeys);
                    self.rekey_retained_exited_pane_outputs(&session_name, &new_name, session_id);
                    self.rekey_session_silence_timers_locked(
                        &state,
                        &session_name,
                        &new_name,
                        session_id,
                    );
                    self.rename_session_lease(&session_name, &new_name, session_id);
                    self.rekey_web_session(&session_name, &new_name, session_id);
                    let mut active_attach = self.active_attach.lock().await;
                    sessions_remembering_renamed =
                        active_attach.rename_session(&session_name, session_id, &new_name);
                    drop(active_attach);
                    #[cfg(test)]
                    self.pause_before_rename_session_control_commit(&session_name)
                        .await;
                    self.rename_control_session(&session_name, session_id, &new_name)
                        .await;
                    Response::RenameSession(rmux_proto::RenameSessionResponse {
                        session_name: new_name.clone(),
                    })
                }
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };
        if matches!(response, Response::RenameSession(_)) {
            let event = LifecycleEvent::SessionRenamed {
                session_name: new_name.clone(),
            };
            self.emit_for_session_identity(event, &new_name, session_id)
                .await;
            self.refresh_attached_session(&new_name).await;
            for remembering_session in sessions_remembering_renamed {
                if remembering_session != new_name {
                    self.refresh_attached_session(&remembering_session).await;
                }
            }
        }

        response
    }
}
