use rmux_core::{LifecycleEvent, WindowId, WINDOW_ALERTFLAGS};
use rmux_proto::{
    ErrorResponse, KillSessionResponse, Response, RmuxError, SessionId, SessionName, WindowTarget,
};

use super::super::attach_support::{surviving_attached_resize_targets, SessionDetachOnDestroy};
use super::super::{
    prepare_lifecycle_event, resolve_existing_session_target, PaneOutputSubscriptionKeySnapshot,
    RequestHandler,
};
use super::session_pane_ids;

enum KillSessionExecution {
    Completed(Response),
    OnlyLinkedWindowChanged,
}

#[derive(Clone, Copy)]
struct OnlyWindowPrecondition {
    window_id: WindowId,
    kill_if_last: bool,
}

impl KillSessionExecution {
    fn into_unconditional_response(self) -> Response {
        match self {
            Self::Completed(response) => response,
            Self::OnlyLinkedWindowChanged => {
                unreachable!("unconditional kill-session cannot lose a window precondition")
            }
        }
    }
}

impl From<Response> for KillSessionExecution {
    fn from(response: Response) -> Self {
        Self::Completed(response)
    }
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_kill_session(
        &self,
        request: rmux_proto::KillSessionRequest,
    ) -> Response {
        let expected_session_id = explicit_session_id_target(&request.target);
        self.handle_kill_session_with_identity(request, expected_session_id, None, None)
            .await
            .into_unconditional_response()
    }

    pub(in crate::handler) async fn handle_kill_session_identity(
        &self,
        request: rmux_proto::KillSessionRequest,
        session_id: SessionId,
    ) -> Response {
        self.handle_kill_session_with_identity(request, Some(session_id), None, None)
            .await
            .into_unconditional_response()
    }

    pub(in crate::handler) async fn handle_kill_session_if_only_linked_window_identity(
        &self,
        request: rmux_proto::KillSessionRequest,
        session_id: SessionId,
        window_id: WindowId,
        kill_if_last: bool,
    ) -> Option<Response> {
        match self
            .handle_kill_session_with_identity(
                request,
                Some(session_id),
                None,
                Some(OnlyWindowPrecondition {
                    window_id,
                    kill_if_last,
                }),
            )
            .await
        {
            KillSessionExecution::Completed(response) => Some(response),
            KillSessionExecution::OnlyLinkedWindowChanged => None,
        }
    }

    pub(in crate::handler) async fn handle_kill_expired_session_lease_identity(
        &self,
        request: rmux_proto::KillSessionRequest,
        session_id: SessionId,
        lease_token: u64,
    ) -> Response {
        self.handle_kill_session_with_identity(request, Some(session_id), Some(lease_token), None)
            .await
            .into_unconditional_response()
    }

    async fn handle_kill_session_with_identity(
        &self,
        request: rmux_proto::KillSessionRequest,
        expected_session_id: Option<SessionId>,
        expected_expired_lease_token: Option<u64>,
        expected_only_window: Option<OnlyWindowPrecondition>,
    ) -> KillSessionExecution {
        let (session_name, selected_session_id) = {
            let state = self.state.lock().await;
            if let Some(session_id) = expected_session_id {
                let Some(session) = state.sessions.session_by_id(session_id) else {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::SessionNotFound(request.target.to_string()),
                    })
                    .into();
                };
                let session_name = session.name().clone();
                if let Err(error) =
                    super::super::require_expected_session_identity(&state, &session_name)
                {
                    return Response::Error(ErrorResponse { error }).into();
                }
                (session_name, session_id)
            } else {
                match resolve_existing_session_target(
                    &state.sessions,
                    "kill-session",
                    &request.target,
                ) {
                    Ok(session_name) => {
                        if let Err(error) =
                            super::super::require_expected_session_identity(&state, &session_name)
                        {
                            return Response::Error(ErrorResponse { error }).into();
                        }
                        let session_id = state
                            .sessions
                            .session(&session_name)
                            .expect("resolved session must exist")
                            .id();
                        (session_name, session_id)
                    }
                    Err(error) => return Response::Error(ErrorResponse { error }).into(),
                }
            }
        };

        if request.clear_alerts {
            let (response, refresh_session_name) = {
                let mut state = self.state.lock().await;
                let current_session_name = if expected_session_id.is_some() {
                    let Some(session) = state.sessions.session_by_id(selected_session_id) else {
                        return Response::Error(ErrorResponse {
                            error: RmuxError::SessionNotFound(session_name.to_string()),
                        })
                        .into();
                    };
                    session.name().clone()
                } else {
                    session_name.clone()
                };
                let Some(session) = state.sessions.session(&current_session_name) else {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::SessionNotFound(current_session_name.to_string()),
                    })
                    .into();
                };
                if session.id() != selected_session_id {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::SessionNotFound(current_session_name.to_string()),
                    })
                    .into();
                }
                let window_indexes = session
                    .windows()
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>();
                for window_index in window_indexes {
                    let target =
                        WindowTarget::with_window(current_session_name.clone(), window_index);
                    let _ = state
                        .sessions
                        .clear_window_alerts(&target, WINDOW_ALERTFLAGS);
                    let _ = state
                        .sessions
                        .clear_winlink_alert_flags(&target, rmux_core::WINLINK_ALERTFLAGS);
                }
                (
                    Response::KillSession(KillSessionResponse { existed: true }),
                    current_session_name,
                )
            };
            self.refresh_attached_session(&refresh_session_name).await;
            self.refresh_control_session(&refresh_session_name).await;
            return response.into();
        }

        #[cfg(test)]
        self.pause_after_kill_session_selection_identity_capture(&session_name)
            .await;

        let (
            response,
            queued_lifecycle_events,
            removed_pane_ids,
            removed_sessions,
            removed_attached_sessions,
            resize_window_ids,
            subscriptions_removed,
        ) = {
            let mut state = self.state.lock().await;
            let session_name = if expected_session_id.is_some() {
                let Some(session) = state.sessions.session_by_id(selected_session_id) else {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::SessionNotFound(session_name.to_string()),
                    })
                    .into();
                };
                session.name().clone()
            } else {
                session_name.clone()
            };
            if state
                .sessions
                .session(&session_name)
                .is_none_or(|session| session.id() != selected_session_id)
            {
                return Response::Error(ErrorResponse {
                    error: RmuxError::SessionNotFound(session_name.to_string()),
                })
                .into();
            }
            if let Some(expected_window) = expected_only_window {
                let session = state
                    .sessions
                    .session(&session_name)
                    .expect("validated session must exist");
                let mut windows = session.windows();
                let only_window = windows.next();
                let only_window_is_still_linked =
                    only_window.is_some_and(|(window_index, window)| {
                        windows.next().is_none()
                            && window.id() == expected_window.window_id
                            && (expected_window.kill_if_last
                                || state.window_link_count(&session_name, window_index) > 1)
                    });
                if !only_window_is_still_linked {
                    return KillSessionExecution::OnlyLinkedWindowChanged;
                }
            }
            if let Some(token) = expected_expired_lease_token {
                if !self.claim_expired_session_lease(selected_session_id, token) {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::owned_session_lease_lost(session_name),
                    })
                    .into();
                }
            }
            let sessions_to_remove = if request.kill_all_except_target {
                let mut sessions = state
                    .sessions
                    .iter()
                    .map(|session| (session.name(), session))
                    .map(|(name, session)| (name.clone(), session.id()))
                    .filter(|(name, _)| name != &session_name)
                    .collect::<Vec<_>>();
                sessions.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
                sessions
            } else if request.kill_group {
                let mut sessions = state.sessions.session_group_members(&session_name);
                if sessions.is_empty() {
                    sessions.push(session_name.clone());
                }
                sessions
                    .into_iter()
                    .filter_map(|name| {
                        let id = state.sessions.session(&name)?.id();
                        Some((name, id))
                    })
                    .collect()
            } else {
                vec![(session_name.clone(), selected_session_id)]
            };
            // kill-session -a may remove unrelated session families, while a
            // grouped owner removal may transfer surviving runtimes. Capture
            // all stable pane identities so both cases reconcile atomically.
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_all(&state);
            let mut queued_events = Vec::new();
            let mut removed_pane_ids = Vec::new();
            let mut removed_sessions: Vec<(SessionName, SessionId)> = Vec::new();
            let mut removed_attached_sessions = Vec::new();
            let mut removed_window_ids = Vec::new();
            let mut response_error = None;

            for (session_name, session_id) in &sessions_to_remove {
                if state
                    .sessions
                    .session(session_name)
                    .is_none_or(|session| session.id() != *session_id)
                {
                    continue;
                }
                let current_runtime_owner = state.sessions.runtime_owner(session_name);
                if current_runtime_owner.as_ref() == Some(session_name)
                    && !state.contains_session_terminals(session_name)
                {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::Server(format!(
                            "missing pane terminals for session {}",
                            session_name
                        )),
                    })
                    .into();
                }
            }

            for (session_name, session_id) in &sessions_to_remove {
                if state
                    .sessions
                    .session(session_name)
                    .is_none_or(|session| session.id() != *session_id)
                {
                    continue;
                }
                let current_runtime_owner = state.sessions.runtime_owner(session_name);
                let next_runtime_owner = state.sessions.runtime_owner_transfer_target(session_name);
                let detach_on_destroy = SessionDetachOnDestroy::capture(&state, session_name);

                let (unlinked_windows, unlinked_panes) = {
                    let session = state
                        .sessions
                        .session(session_name)
                        .expect("validated removal");
                    (
                        session
                            .windows()
                            .map(|(index, window)| {
                                (
                                    index,
                                    window.id(),
                                    window.name().unwrap_or_default().to_owned(),
                                )
                            })
                            .collect::<Vec<_>>(),
                        session_pane_ids(session),
                    )
                };

                match state.sessions.remove_session(session_name) {
                    Ok(removed_session) => {
                        state.retire_removed_lifecycle_targets();
                        removed_window_ids.extend(unlinked_windows.iter().map(|(_, id, _)| *id));
                        removed_pane_ids.extend(unlinked_panes);
                        removed_sessions.push((session_name.clone(), removed_session.session_id));
                        removed_attached_sessions.push((
                            session_name.clone(),
                            removed_session.session_id,
                            detach_on_destroy,
                        ));
                        let session_closed = LifecycleEvent::SessionClosed {
                            session_name: session_name.clone(),
                            session_id: Some(removed_session.session_id.as_u32()),
                        };
                        let implicit_unlink_teardown = expected_only_window.is_some();
                        let mut removal_events = Vec::with_capacity(unlinked_windows.len() + 1);
                        // Explicit kill-session closes the session first. An
                        // implicit unlink teardown lets the window hook consume
                        // its state before the session hook cleans that state up.
                        if !implicit_unlink_teardown {
                            removal_events.push(session_closed.clone());
                        }
                        for (window_index, window_id, window_name) in unlinked_windows {
                            removal_events.push(LifecycleEvent::WindowUnlinked {
                                session_name: session_name.clone(),
                                target: Some(WindowTarget::with_window(
                                    session_name.clone(),
                                    window_index,
                                )),
                                window_id: Some(window_id.as_u32()),
                                window_name: Some(window_name),
                            });
                        }
                        if implicit_unlink_teardown {
                            removal_events.push(session_closed);
                        }
                        for event in removal_events {
                            queued_events.push(prepare_lifecycle_event(&mut state, &event));
                        }
                        let _ = state.options.remove_session(session_name);
                        let _ = state.environment.remove_session(session_name);
                        let _ = state.hooks.remove_session(session_name);
                        if let Err(error) = state.remove_session_terminals(
                            session_name,
                            current_runtime_owner.as_ref(),
                            next_runtime_owner.as_ref(),
                        ) {
                            response_error = Some(error);
                            break;
                        }
                    }
                    Err(RmuxError::SessionNotFound(_)) => {}
                    Err(error) => {
                        response_error = Some(error);
                        break;
                    }
                }
            }

            let response = response_error.map_or_else(
                || Response::KillSession(KillSessionResponse { existed: true }),
                |error| Response::Error(ErrorResponse { error }),
            );
            let removed_pane_ids = state.pane_ids_no_longer_referenced(removed_pane_ids);
            self.record_panes_closed_as_killed(&removed_pane_ids);
            #[cfg(test)]
            self.pause_before_kill_session_subscription_rekey(&session_name)
                .await;
            // Keep removals and owner rekeys in the same state -> registry
            // transaction. Apply the committed delta even when a later
            // multi-session removal made the aggregate response an error.
            let subscriptions_removed = self.apply_pane_output_subscription_reconciliation(
                subscription_keys.reconcile_after(&state),
            );
            (
                response,
                queued_events,
                removed_pane_ids,
                removed_sessions,
                removed_attached_sessions,
                removed_window_ids,
                subscriptions_removed,
            )
        };

        if subscriptions_removed {
            let _ = self.request_shutdown_if_pending();
        }

        // Rehome control clients before publishing SessionClosed effects, but
        // defer their later ClientSessionChanged tickets until the teardown
        // batch reserved by the state mutation has been published.
        let mut prepared_rehomes = Vec::with_capacity(removed_attached_sessions.len());
        for (removed_session_name, removed_session_id, detach_on_destroy) in
            removed_attached_sessions
        {
            prepared_rehomes.push(
                self.prepare_destroy_session_rehome(
                    &removed_session_name,
                    removed_session_id,
                    detach_on_destroy,
                )
                .await,
            );
        }

        for event in queued_lifecycle_events {
            self.emit_prepared(event).await;
        }
        for prepared in &mut prepared_rehomes {
            for event in prepared.control_lifecycle_events.drain(..) {
                self.emit_prepared(event).await;
            }
        }
        for prepared in prepared_rehomes {
            self.exit_prepared_attached_session_identity(prepared).await;
        }

        #[cfg(all(test, feature = "web"))]
        self.pause_before_kill_session_web_prune(&removed_sessions)
            .await;
        #[cfg(all(any(unix, windows), feature = "web"))]
        {
            self.web_shares.remove_targets_for_panes(&removed_pane_ids);
            self.web_shares
                .remove_targets_for_sessions(&removed_sessions);
        }
        #[cfg(not(all(any(unix, windows), feature = "web")))]
        let _ = &removed_sessions;
        if !removed_pane_ids.is_empty() {
            self.forget_pane_snapshot_coalescers(&removed_pane_ids);
        }
        self.remove_session_leases(&removed_sessions);
        let removed_session_names = removed_sessions
            .iter()
            .map(|(session_name, _)| session_name.clone())
            .collect::<Vec<_>>();
        for removed_session_name in &removed_session_names {
            self.cancel_session_silence_timers(removed_session_name)
                .await;
        }
        let (resize_targets, refresh_sessions) = {
            let state = self.state.lock().await;
            let resize_targets = surviving_attached_resize_targets(&state, resize_window_ids);
            let mut refresh_sessions = resize_targets
                .iter()
                .flat_map(|target| {
                    state.window_linked_session_family_list(
                        target.session_name(),
                        target.window_index(),
                    )
                })
                .collect::<Vec<_>>();
            refresh_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            refresh_sessions.dedup();
            (resize_targets, refresh_sessions)
        };
        for resize_target in resize_targets {
            let _ = self
                .reconcile_attached_window_size_and_emit(&resize_target)
                .await;
        }
        for refresh_session in refresh_sessions {
            self.refresh_attached_session(&refresh_session).await;
        }

        let _ = self.queue_shutdown_if_server_empty().await;

        response.into()
    }
}

fn explicit_session_id_target(target: &SessionName) -> Option<SessionId> {
    let raw_id = target.as_str().strip_prefix('$')?;
    if raw_id.is_empty() || !raw_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    raw_id.parse::<u32>().ok().map(SessionId::new)
}
