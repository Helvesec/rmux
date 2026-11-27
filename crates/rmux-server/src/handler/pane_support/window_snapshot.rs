use super::super::{
    focus_support::prepare_server_focus_effects, prepare_lifecycle_event, RequestHandler,
};
use super::pane_layout::layout_notification_state;
use crate::pane_terminals::{session_not_found, HandlerState, WindowRuntimeResizeScope};
use rmux_core::LifecycleEvent;
use rmux_proto::{
    ErrorResponse, ExportWindowSnapshotRequest, ExportWindowSnapshotResponse,
    ExportWindowSnapshotV2Request, ExportWindowSnapshotV2Response, Response,
    RestoreWindowSnapshotRequest, RestoreWindowSnapshotResponse, RestoreWindowSnapshotV2Request,
    RestoreWindowSnapshotV2Response, RmuxError, RmuxWindowSnapshot, RmuxWindowSnapshotV2,
    WindowTarget,
};

impl RequestHandler {
    pub(in crate::handler) async fn handle_export_window_snapshot(
        &self,
        request: ExportWindowSnapshotRequest,
    ) -> Response {
        let response = {
            let state = self.state.lock().await;
            if let Err(error) =
                super::super::require_expected_window_identity(&state, &request.target)
            {
                return Response::Error(ErrorResponse { error });
            }
            match export_window_snapshot_response(&state, &request.target) {
                Ok(response) => Response::ExportWindowSnapshot(Box::new(response)),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        response
    }

    pub(in crate::handler) async fn handle_restore_window_snapshot(
        &self,
        request: RestoreWindowSnapshotRequest,
    ) -> Response {
        let target = request.target.clone();
        let session_name = target.session_name().clone();
        let window_index = target.window_index();
        let (response, refresh_sessions, focus_effects, layout_event, selection_event) = {
            let mut state = self.state.lock().await;
            match (|| -> Result<_, RmuxError> {
                super::super::require_expected_window_identity(&state, &target)?;
                validate_restore_window_snapshot_target(&state, &target, &request.snapshot)?;
                state
                    .mutate_store_and_resize_window_terminal_with_family_scoped(
                        &session_name,
                        window_index,
                        |store| {
                            let mut window_layout = store.window_layout_context_mut(&target)?;
                            let before = layout_notification_state(&window_layout);
                            let resized_panes =
                                window_layout.restore_rmux_window_snapshot(&request.snapshot)?;
                            let after = layout_notification_state(&window_layout);
                            let layout_changed = before.layout != after.layout
                                || before.floating_stack != after.floating_stack
                                || before.zoomed != after.zoomed;
                            let pane_changed = before.active_pane_id != after.active_pane_id
                                || before.last_pane_id != after.last_pane_id;
                            let active_changed = before.active_pane_id != after.active_pane_id;
                            Ok((
                                RestoreWindowSnapshotResponse {
                                    target: target.clone(),
                                    window_id: request.snapshot.window_id,
                                },
                                resized_panes,
                                layout_changed,
                                pane_changed,
                                active_changed,
                            ))
                        },
                        |_, (_, resized_panes, _, _, _)| {
                            Ok((!resized_panes.is_empty())
                                .then_some(WindowRuntimeResizeScope::Panes(resized_panes.clone())))
                        },
                    )
                    .map_err(RmuxError::from)
            })() {
                Ok(restore) => {
                    let (
                        (response, _, layout_changed, pane_changed, active_changed),
                        refresh_sessions,
                    ) = restore;
                    let focus_effects = if active_changed {
                        let mut active_attach = self.active_attach.lock().await;
                        Some(prepare_server_focus_effects(&mut state, &mut active_attach))
                    } else {
                        None
                    };
                    let layout_event = layout_changed.then(|| {
                        prepare_lifecycle_event(
                            &mut state,
                            &LifecycleEvent::WindowLayoutChanged {
                                target: target.clone(),
                            },
                        )
                    });
                    let selection_event = pane_changed.then(|| {
                        prepare_lifecycle_event(
                            &mut state,
                            &LifecycleEvent::WindowPaneChanged {
                                target: target.clone(),
                            },
                        )
                    });
                    (
                        Response::RestoreWindowSnapshot(response),
                        refresh_sessions,
                        focus_effects,
                        layout_event,
                        selection_event,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    None,
                    None,
                    None,
                ),
            }
        };

        if matches!(response, Response::RestoreWindowSnapshot(_)) {
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
            if let Some(layout_event) = layout_event {
                self.emit_prepared(layout_event).await;
            }
            if let Some(selection_event) = selection_event {
                self.emit_prepared(selection_event).await;
            }
            if refresh_sessions.is_empty() {
                if self.attached_count(&session_name).await > 0 {
                    self.refresh_attached_session(&session_name).await;
                }
            } else {
                self.refresh_linked_window_sessions(refresh_sessions).await;
            }
        }

        response
    }
}

fn export_window_snapshot_response(
    state: &HandlerState,
    target: &WindowTarget,
) -> Result<ExportWindowSnapshotResponse, RmuxError> {
    let session = state
        .sessions
        .session(target.session_name())
        .ok_or_else(|| session_not_found(target.session_name()))?;
    let window = session.window_at(target.window_index()).ok_or_else(|| {
        RmuxError::invalid_target(target.to_string(), "window index does not exist in session")
    })?;
    let mut snapshot = window.export_rmux_window_snapshot(session.id(), target.clone())?;
    snapshot.runtimes = snapshot_runtime_identities(state, window)?;
    ExportWindowSnapshotResponse::from_snapshot(snapshot).map_err(|error| {
        RmuxError::Server(format!("failed to encode RMUX window snapshot: {error}"))
    })
}

fn validate_restore_window_snapshot_target(
    state: &HandlerState,
    target: &WindowTarget,
    snapshot: &RmuxWindowSnapshot,
) -> Result<(), RmuxError> {
    if snapshot.target != *target {
        return Err(RmuxError::invalid_target(
            target.to_string(),
            "snapshot target occurrence does not match restore target",
        ));
    }
    let session = state
        .sessions
        .session(target.session_name())
        .ok_or_else(|| session_not_found(target.session_name()))?;
    if session.id() != snapshot.session_id {
        return Err(RmuxError::SessionNotFound(
            target.session_name().to_string(),
        ));
    }
    let window = session.window_at(target.window_index()).ok_or_else(|| {
        RmuxError::invalid_target(target.to_string(), "window index does not exist in session")
    })?;
    if window.id() != snapshot.window_id {
        return Err(RmuxError::invalid_target(
            target.to_string(),
            "snapshot window id does not match target",
        ));
    }
    if window.size() != snapshot.size {
        return Err(RmuxError::Server(
            "invalid RMUX window snapshot: snapshot size does not match target".to_owned(),
        ));
    }
    if snapshot.runtimes != snapshot_runtime_identities(state, window)? {
        return Err(RmuxError::Server(
            "invalid RMUX window snapshot: pane runtime incarnation changed".to_owned(),
        ));
    }
    Ok(())
}

fn snapshot_runtime_identities(
    state: &HandlerState,
    window: &rmux_core::Window,
) -> Result<Vec<rmux_proto::RmuxPaneRuntimeIdentity>, RmuxError> {
    let mut identities = window
        .panes()
        .iter()
        .map(|pane| {
            let lifecycle = state
                .pane_lifecycle(pane.id())
                .ok_or_else(|| RmuxError::Server("snapshot runtime is missing".to_owned()))?;
            if lifecycle.window_id != window.id() {
                return Err(RmuxError::Server(
                    "snapshot runtime is not live in its canonical window".to_owned(),
                ));
            }
            let incarnation = lifecycle.snapshot_incarnation().ok_or_else(|| {
                RmuxError::Server("snapshot runtime birth identity unavailable".to_owned())
            })?;
            Ok(rmux_proto::RmuxPaneRuntimeIdentity {
                pane_id: pane.id(),
                incarnation,
            })
        })
        .collect::<Result<Vec<_>, RmuxError>>()?;
    identities.sort_by_key(|identity| identity.pane_id);
    Ok(identities)
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_export_window_snapshot_v2(
        &self,
        request: ExportWindowSnapshotV2Request,
    ) -> Response {
        let response = {
            let state = self.state.lock().await;
            if let Err(error) =
                super::super::require_expected_window_identity(&state, &request.target)
            {
                return Response::Error(ErrorResponse { error });
            }
            match export_window_snapshot_v2_response(&state, &request.target) {
                Ok(response) => Response::ExportWindowSnapshotV2(Box::new(response)),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        response
    }

    pub(in crate::handler) async fn handle_restore_window_snapshot_v2(
        &self,
        request: RestoreWindowSnapshotV2Request,
    ) -> Response {
        let target = request.target.clone();
        let session_name = target.session_name().clone();
        let window_index = target.window_index();
        let (response, refresh_sessions, focus_effects, layout_event, selection_event) = {
            let mut state = self.state.lock().await;
            match (|| -> Result<_, RmuxError> {
                super::super::require_expected_window_identity(&state, &target)?;
                validate_restore_window_snapshot_v2_target(&state, &target, &request.snapshot)?;
                state
                    .mutate_store_and_resize_window_terminal_with_family_scoped(
                        &session_name,
                        window_index,
                        |store| {
                            let mut window_layout = store.window_layout_context_mut(&target)?;
                            let before = layout_notification_state(&window_layout);
                            let resized_panes =
                                window_layout.restore_rmux_window_snapshot_v2(&request.snapshot)?;
                            let after = layout_notification_state(&window_layout);
                            let layout_changed = before.layout != after.layout
                                || before.floating_stack != after.floating_stack
                                || before.zoomed != after.zoomed;
                            let pane_changed = before.active_pane_id != after.active_pane_id
                                || before.last_pane_id != after.last_pane_id;
                            let active_changed = before.active_pane_id != after.active_pane_id;
                            Ok((
                                RestoreWindowSnapshotV2Response {
                                    target: target.clone(),
                                    window_id: request.snapshot.window_id,
                                },
                                resized_panes,
                                layout_changed,
                                pane_changed,
                                active_changed,
                            ))
                        },
                        |_, (_, resized_panes, _, _, _)| {
                            Ok((!resized_panes.is_empty())
                                .then_some(WindowRuntimeResizeScope::Panes(resized_panes.clone())))
                        },
                    )
                    .map_err(RmuxError::from)
            })() {
                Ok(restore) => {
                    let (
                        (response, _, layout_changed, pane_changed, active_changed),
                        refresh_sessions,
                    ) = restore;
                    let focus_effects = if active_changed {
                        let mut active_attach = self.active_attach.lock().await;
                        Some(prepare_server_focus_effects(&mut state, &mut active_attach))
                    } else {
                        None
                    };
                    let layout_event = layout_changed.then(|| {
                        prepare_lifecycle_event(
                            &mut state,
                            &LifecycleEvent::WindowLayoutChanged {
                                target: target.clone(),
                            },
                        )
                    });
                    let selection_event = pane_changed.then(|| {
                        prepare_lifecycle_event(
                            &mut state,
                            &LifecycleEvent::WindowPaneChanged {
                                target: target.clone(),
                            },
                        )
                    });
                    (
                        Response::RestoreWindowSnapshotV2(response),
                        refresh_sessions,
                        focus_effects,
                        layout_event,
                        selection_event,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    None,
                    None,
                    None,
                ),
            }
        };

        if matches!(response, Response::RestoreWindowSnapshotV2(_)) {
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
            if let Some(layout_event) = layout_event {
                self.emit_prepared(layout_event).await;
            }
            if let Some(selection_event) = selection_event {
                self.emit_prepared(selection_event).await;
            }
            if refresh_sessions.is_empty() {
                if self.attached_count(&session_name).await > 0 {
                    self.refresh_attached_session(&session_name).await;
                }
            } else {
                self.refresh_linked_window_sessions(refresh_sessions).await;
            }
        }

        response
    }
}

fn export_window_snapshot_v2_response(
    state: &HandlerState,
    target: &WindowTarget,
) -> Result<ExportWindowSnapshotV2Response, RmuxError> {
    let session = state
        .sessions
        .session(target.session_name())
        .ok_or_else(|| session_not_found(target.session_name()))?;
    let window = session.window_at(target.window_index()).ok_or_else(|| {
        RmuxError::invalid_target(target.to_string(), "window index does not exist in session")
    })?;
    let mut snapshot = window.export_rmux_window_snapshot_v2(session.id(), target.clone())?;
    snapshot.runtimes = snapshot_runtime_identities(state, window)?;
    ExportWindowSnapshotV2Response::from_snapshot(snapshot).map_err(|error| {
        RmuxError::Server(format!("failed to encode RMUX window snapshot: {error}"))
    })
}

fn validate_restore_window_snapshot_v2_target(
    state: &HandlerState,
    target: &WindowTarget,
    snapshot: &RmuxWindowSnapshotV2,
) -> Result<(), RmuxError> {
    if snapshot.target != *target {
        return Err(RmuxError::invalid_target(
            target.to_string(),
            "snapshot target occurrence does not match restore target",
        ));
    }
    let session = state
        .sessions
        .session(target.session_name())
        .ok_or_else(|| session_not_found(target.session_name()))?;
    if session.id() != snapshot.session_id {
        return Err(RmuxError::SessionNotFound(
            target.session_name().to_string(),
        ));
    }
    let window = session.window_at(target.window_index()).ok_or_else(|| {
        RmuxError::invalid_target(target.to_string(), "window index does not exist in session")
    })?;
    if window.id() != snapshot.window_id {
        return Err(RmuxError::invalid_target(
            target.to_string(),
            "snapshot window id does not match target",
        ));
    }
    if window.size() != snapshot.size {
        return Err(RmuxError::Server(
            "invalid RMUX window snapshot: snapshot size does not match target".to_owned(),
        ));
    }
    if snapshot.runtimes != snapshot_runtime_identities(state, window)? {
        return Err(RmuxError::Server(
            "invalid RMUX window snapshot: pane runtime incarnation changed".to_owned(),
        ));
    }
    Ok(())
}
