//! Window naming, cycling and listing: `rename-window`, `next-window`,
//! `previous-window`, `last-window` and `list-windows`.

use super::*;

impl RequestHandler {
    pub(in crate::handler) async fn handle_rename_window(
        &self,
        request: rmux_proto::RenameWindowRequest,
    ) -> Response {
        let target = request.target.clone();
        let (response, refresh_sessions) = {
            let mut state = self.state.lock().await;
            if let Err(error) =
                super::super::require_expected_window_identity(&state, &request.target)
            {
                return Response::Error(ErrorResponse { error });
            }
            match state.rename_window(request.target, request.name) {
                Ok(response) => {
                    let refresh_sessions = state.window_linked_session_family_list(
                        target.session_name(),
                        target.window_index(),
                    );
                    (Response::RenameWindow(response), refresh_sessions)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new()),
            }
        };

        if matches!(response, Response::RenameWindow(_)) {
            self.emit(LifecycleEvent::WindowRenamed { target }).await;
            for refresh_session in refresh_sessions {
                self.refresh_attached_session(&refresh_session).await;
            }
        }

        response
    }

    pub(in crate::handler) async fn handle_next_window(
        &self,
        request: rmux_proto::NextWindowRequest,
    ) -> Response {
        let session_name = request.target;
        let (response, resize_sessions, focus_effects, selection_event) = {
            let mut state = self.state.lock().await;
            let previous_window_index = state
                .sessions
                .session(&session_name)
                .map(|session| session.active_window_index());
            match state.next_window(&session_name, request.alerts_only) {
                Ok(response) => {
                    let resize_sessions = previous_window_index
                        .map(|previous| {
                            linked_resize_sessions_for_window_change(
                                &state,
                                &session_name,
                                previous,
                                response.target.window_index(),
                            )
                        })
                        .unwrap_or_else(|| vec![session_name.clone()]);
                    let mut active_attach = self.active_attach.lock().await;
                    let focus_effects =
                        prepare_server_focus_effects(&mut state, &mut active_attach);
                    let event = prepare_lifecycle_event(
                        &mut state,
                        &LifecycleEvent::SessionWindowChanged {
                            session_name: session_name.clone(),
                        },
                    );
                    (
                        Response::NextWindow(response),
                        resize_sessions,
                        Some(focus_effects),
                        Some(event),
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    None,
                    None,
                ),
            }
        };

        if let Response::NextWindow(_) = &response {
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
            self.emit_prepared(selection_event.expect("next-window prepares event"))
                .await;
            if let Response::NextWindow(success) = &response {
                self.queue_inline_hook(
                    HookName::AfterSelectWindow,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Window(success.target.clone())),
                    PendingInlineHookFormat::AfterCommand,
                );
            }
            self.reconcile_and_refresh_attached_sessions(resize_sessions)
                .await;
        }

        response
    }

    pub(in crate::handler) async fn handle_previous_window(
        &self,
        request: rmux_proto::PreviousWindowRequest,
    ) -> Response {
        let session_name = request.target;
        let (response, resize_sessions, focus_effects, selection_event) = {
            let mut state = self.state.lock().await;
            let previous_window_index = state
                .sessions
                .session(&session_name)
                .map(|session| session.active_window_index());
            match state.previous_window(&session_name, request.alerts_only) {
                Ok(response) => {
                    let resize_sessions = previous_window_index
                        .map(|previous| {
                            linked_resize_sessions_for_window_change(
                                &state,
                                &session_name,
                                previous,
                                response.target.window_index(),
                            )
                        })
                        .unwrap_or_else(|| vec![session_name.clone()]);
                    let mut active_attach = self.active_attach.lock().await;
                    let focus_effects =
                        prepare_server_focus_effects(&mut state, &mut active_attach);
                    let event = prepare_lifecycle_event(
                        &mut state,
                        &LifecycleEvent::SessionWindowChanged {
                            session_name: session_name.clone(),
                        },
                    );
                    (
                        Response::PreviousWindow(response),
                        resize_sessions,
                        Some(focus_effects),
                        Some(event),
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    None,
                    None,
                ),
            }
        };

        if let Response::PreviousWindow(_) = &response {
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
            self.emit_prepared(selection_event.expect("previous-window prepares event"))
                .await;
            if let Response::PreviousWindow(success) = &response {
                self.queue_inline_hook(
                    HookName::AfterSelectWindow,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Window(success.target.clone())),
                    PendingInlineHookFormat::AfterCommand,
                );
            }
            self.reconcile_and_refresh_attached_sessions(resize_sessions)
                .await;
        }

        response
    }

    pub(in crate::handler) async fn handle_last_window(
        &self,
        request: rmux_proto::LastWindowRequest,
    ) -> Response {
        let session_name = request.target;
        let (response, resize_sessions, focus_effects, selection_event) = {
            let mut state = self.state.lock().await;
            let previous_window_index = state
                .sessions
                .session(&session_name)
                .map(|session| session.active_window_index());
            match state.last_window(&session_name) {
                Ok(response) => {
                    let resize_sessions = previous_window_index
                        .map(|previous| {
                            linked_resize_sessions_for_window_change(
                                &state,
                                &session_name,
                                previous,
                                response.target.window_index(),
                            )
                        })
                        .unwrap_or_else(|| vec![session_name.clone()]);
                    let mut active_attach = self.active_attach.lock().await;
                    let focus_effects =
                        prepare_server_focus_effects(&mut state, &mut active_attach);
                    let event = prepare_lifecycle_event(
                        &mut state,
                        &LifecycleEvent::SessionWindowChanged {
                            session_name: session_name.clone(),
                        },
                    );
                    (
                        Response::LastWindow(response),
                        resize_sessions,
                        Some(focus_effects),
                        Some(event),
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    None,
                    None,
                ),
            }
        };

        if let Response::LastWindow(_) = &response {
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
            self.emit_prepared(selection_event.expect("last-window prepares event"))
                .await;
            if let Response::LastWindow(success) = &response {
                self.queue_inline_hook(
                    HookName::AfterSelectWindow,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Window(success.target.clone())),
                    PendingInlineHookFormat::AfterCommand,
                );
            }
            self.reconcile_and_refresh_attached_sessions(resize_sessions)
                .await;
        }

        response
    }

    pub(in crate::handler) async fn handle_list_windows(
        &self,
        request: rmux_proto::ListWindowsRequest,
    ) -> Response {
        if let Some(suffix) = request
            .format
            .as_deref()
            .and_then(|s| s.strip_prefix(rmux_proto::scene::SCENE_QUERY_PREFIX))
        {
            let result = async {
                if request.filter.is_some() || request.sort_order.is_some() || request.reversed {
                    return Err(rmux_proto::RmuxError::Server(
                        "scene query does not accept listing modifiers".to_owned(),
                    ));
                }
                let query = rmux_proto::scene::SceneQuery::decode(suffix)?;
                let state = self.state.lock().await;
                let session = state.sessions.session(&request.target).ok_or_else(|| {
                    rmux_proto::RmuxError::Server("scene session disappeared".to_owned())
                })?;
                crate::renderer::window_scene(session, &state.options, &query)?.encode()
            }
            .await;
            return match result {
                Ok(stdout) => Response::ListWindows(rmux_proto::ListWindowsResponse {
                    windows: Vec::new(),
                    output: rmux_proto::CommandOutput::from_stdout(stdout.into_bytes()),
                }),
                Err(error) => Response::Error(ErrorResponse { error }),
            };
        }
        let socket_path = self.socket_path();
        let attached_count = {
            let active_attach = self.active_attach.lock().await;
            active_attach.attached_count(&request.target)
        };
        let client_loop_snapshots = self.client_loop_snapshots().await;
        #[cfg(windows)]
        if format_references_pane_pid(request.format.as_deref())
            || format_references_pane_pid(request.filter.as_deref())
        {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
        let state = self.state.lock().await;
        match state.list_windows(ListWindowsSelection {
            session_name: &request.target,
            socket_path: &socket_path,
            format: request.format.as_deref(),
            attached_count,
            filter: request.filter.as_deref(),
            sort_order: request.sort_order.as_deref(),
            reversed: request.reversed,
            client_loop_snapshots: &client_loop_snapshots,
        }) {
            Ok(response) => Response::ListWindows(response),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }
}
