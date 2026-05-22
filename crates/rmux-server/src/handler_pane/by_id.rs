use rmux_core::LifecycleEvent;
use rmux_proto::{
    ErrorResponse, HookName, OptionName, PaneTarget, PaneTargetRef, ResizePaneResponse, Response,
    RmuxError, ScopeSelector, SelectPaneResponse, SetOptionMode, Target, WindowTarget,
};

use super::super::{prepare_lifecycle_event, RequestHandler};
use super::{encode_tokens_for_target, prepare_pane_input_write, write_bytes_to_target};
use crate::hook_runtime::PendingInlineHookFormat;
use crate::pane_terminals::{session_not_found, HandlerState};

impl RequestHandler {
    pub(in crate::handler) async fn handle_pane_input_ref(
        &self,
        request: rmux_proto::PaneInputRequest,
    ) -> Response {
        let key_count = request.keys.len();
        let prepared = {
            let state = self.state.lock().await;
            let target = match resolve_pane_target_ref(&state, &request.target) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let bytes = if request.literal {
                request
                    .keys
                    .iter()
                    .flat_map(|key| key.as_bytes().iter().copied())
                    .collect::<Vec<_>>()
            } else {
                match encode_tokens_for_target(&state, &target, &request.keys) {
                    Ok(bytes) => bytes,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                }
            };
            let write = match prepare_pane_input_write(&state, &target, &bytes) {
                Ok(write) => write,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            (write, bytes)
        };

        write_bytes_to_target(prepared.0, prepared.1, key_count).await
    }

    pub(in crate::handler) async fn handle_pane_resize_ref(
        &self,
        request: rmux_proto::PaneResizeRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let adjustment = request.adjustment;
        let (response, window_index) = {
            let mut state = self.state.lock().await;
            let target = match resolve_pane_target_ref(&state, &request.target) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let window_index = target.window_index();
            let pane_index = target.pane_index();
            let response_target = target.clone();
            let response =
                match state.mutate_session_and_resize_terminals(&session_name, |session| {
                    session.resize_pane_in_window(window_index, pane_index, adjustment)?;
                    Ok(ResizePaneResponse {
                        target: response_target,
                        adjustment,
                    })
                }) {
                    Ok(response) => Response::ResizePane(response),
                    Err(error) => Response::Error(ErrorResponse { error }),
                };
            (response, window_index)
        };

        if matches!(response, Response::ResizePane(_))
            && !matches!(adjustment, rmux_proto::ResizePaneAdjustment::NoOp)
        {
            self.emit(LifecycleEvent::WindowLayoutChanged {
                target: WindowTarget::with_window(session_name.clone(), window_index),
            })
            .await;
            self.refresh_attached_session(&session_name).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_pane_kill_ref(
        &self,
        request: rmux_proto::PaneKillRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let (
            response,
            queued_pane_exited,
            queued_session_closed,
            session_destroyed,
            removed_subscription_keys,
            removed_pane_ids,
            layout_window,
        ) = {
            let mut state = self.state.lock().await;
            let target = match resolve_pane_target_ref(&state, &request.target) {
                Ok(target) => target,
                Err(error) => {
                    return Response::Error(ErrorResponse { error });
                }
            };
            let layout_window = target.window_index();
            let removed_subscription_keys = state
                .pane_output_subscription_keys_for_kill(&target, request.kill_all_except)
                .unwrap_or_default();
            match state.kill_pane_with_options(target.clone(), request.kill_all_except) {
                Ok(result) => {
                    let queued_pane = prepare_lifecycle_event(
                        &mut state,
                        &LifecycleEvent::PaneExited {
                            target: result.hook_context.target.clone(),
                            pane_id: Some(result.hook_context.pane_id),
                            window_id: Some(result.hook_context.window_id),
                            window_name: Some(result.hook_context.window_name.clone()),
                        },
                    );
                    let queued_session = if result.session_destroyed {
                        let _ = state.hooks.remove_session(&session_name);
                        result.removed_session_id.map(|session_id| {
                            prepare_lifecycle_event(
                                &mut state,
                                &LifecycleEvent::SessionClosed {
                                    session_name: session_name.clone(),
                                    session_id: Some(session_id),
                                },
                            )
                        })
                    } else if result.response.window_destroyed {
                        let _ = state.hooks.remove_window(&WindowTarget::with_window(
                            session_name.clone(),
                            layout_window,
                        ));
                        None
                    } else {
                        let _ = state.hooks.remove_pane(&target);
                        None
                    };
                    (
                        Response::KillPane(result.response),
                        Some(queued_pane),
                        queued_session,
                        result.session_destroyed,
                        removed_subscription_keys,
                        result.removed_pane_ids,
                        layout_window,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    None,
                    None,
                    false,
                    Vec::new(),
                    Vec::new(),
                    layout_window,
                ),
            }
        };

        if !removed_pane_ids.is_empty() {
            self.forget_pane_snapshot_coalescers(&removed_pane_ids);
        }
        if let Some(event) = queued_pane_exited {
            self.emit_prepared(event);
        }
        if let Some(event) = queued_session_closed {
            self.emit_prepared(event);
        }
        if matches!(response, Response::KillPane(_)) {
            self.cleanup_pane_output_subscriptions(&removed_subscription_keys)
                .await;
            if session_destroyed {
                self.remove_session_leases(std::slice::from_ref(&session_name));
                self.exit_attached_session(&session_name).await;
                self.cancel_session_silence_timers(&session_name).await;
                self.refresh_control_session(&session_name).await;
                let _ = self.queue_shutdown_if_server_empty().await;
            } else {
                self.sync_session_silence_timers(&session_name).await;
                if let Response::KillPane(success) = &response
                    && !success.window_destroyed {
                        self.emit(LifecycleEvent::WindowLayoutChanged {
                            target: WindowTarget::with_window(session_name.clone(), layout_window),
                        })
                        .await;
                    }
                self.dismiss_mode_tree_for_session(&session_name).await;
                self.refresh_attached_session(&session_name).await;
            }
        }

        response
    }

    pub(in crate::handler) async fn handle_pane_respawn_ref(
        &self,
        request: rmux_proto::PaneRespawnRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let socket_path = self.socket_path();
        let response = {
            let mut state = self.state.lock().await;
            let target = match resolve_pane_target_ref(&state, &request.target) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            if let Some(keep_alive) = request.keep_alive_on_exit
                && let Err(error) = state.options.set(
                    ScopeSelector::Pane(target.clone()),
                    OptionName::RemainOnExit,
                    if keep_alive { "on" } else { "off" }.to_owned(),
                    SetOptionMode::Replace,
                ) {
                    return Response::Error(ErrorResponse { error });
                }
            let request = rmux_proto::RespawnPaneRequest {
                target,
                kill: request.kill,
                start_directory: request.start_directory,
                environment: request.environment,
                command: request.command,
                process_command: request.process_command,
            };
            match state.respawn_pane(
                request,
                &socket_path,
                Some(self.pane_alert_callback()),
                Some(self.pane_exit_callback()),
                |state, replaced| {
                    let queued = prepare_lifecycle_event(
                        state,
                        &LifecycleEvent::PaneExited {
                            target: replaced.target.clone(),
                            pane_id: Some(replaced.pane_id),
                            window_id: Some(replaced.window_id),
                            window_name: Some(replaced.window_name.clone()),
                        },
                    );
                    self.emit_prepared(queued);
                },
            ) {
                Ok(response) => Response::RespawnPane(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::RespawnPane(_)) {
            self.refresh_attached_session(&session_name).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_pane_snapshot_ref(
        &self,
        request: rmux_proto::PaneSnapshotRefRequest,
    ) -> Response {
        let state = self.state.lock().await;
        let target = match resolve_pane_target_ref(&state, &request.target) {
            Ok(target) => target,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        self.handle_resolved_pane_snapshot(&state, &target)
    }

    pub(in crate::handler) async fn handle_pane_select_ref(
        &self,
        request: rmux_proto::PaneSelectRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let title = request.title.clone();
        let (response, pane_changed, window_index) = {
            let mut state = self.state.lock().await;
            let target = match resolve_pane_target_ref(&state, &request.target) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let window_index = target.window_index();
            let pane_index = target.pane_index();
            let pane_changed = title.is_none()
                && state
                    .sessions
                    .session(&session_name)
                    .and_then(|session| session.window_at(window_index))
                    .is_some_and(|window| window.active_pane_index() != pane_index);
            match (|| -> Result<SelectPaneResponse, RmuxError> {
                let response_target = if let Some(title) = title.as_deref() {
                    state.set_pane_title(&target, title)?;
                    target.clone()
                } else {
                    let session = state
                        .sessions
                        .session_mut(&session_name)
                        .ok_or_else(|| session_not_found(&session_name))?;
                    session.select_pane_in_window(window_index, pane_index)?;
                    let active_pane_index = session
                        .window_at(window_index)
                        .expect("selected pane window must exist")
                        .active_pane_index();
                    PaneTarget::with_window(session_name.clone(), window_index, active_pane_index)
                };
                Ok(SelectPaneResponse {
                    target: response_target,
                })
            })() {
                Ok(response) => (Response::SelectPane(response), pane_changed, window_index),
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    false,
                    window_index,
                ),
            }
        };

        if matches!(response, Response::SelectPane(_)) {
            if pane_changed {
                self.emit(LifecycleEvent::WindowPaneChanged {
                    target: WindowTarget::with_window(session_name.clone(), window_index),
                })
                .await;
            }
            if let Response::SelectPane(success) = &response {
                self.queue_inline_hook(
                    HookName::AfterSelectPane,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Pane(success.target.clone())),
                    PendingInlineHookFormat::AfterCommand,
                );
            }
            self.refresh_attached_session(&session_name).await;
        }

        response
    }
}

pub(crate) fn resolve_pane_target_ref(
    state: &HandlerState,
    target: &PaneTargetRef,
) -> Result<PaneTarget, RmuxError> {
    match target {
        PaneTargetRef::Slot(target) => Ok(target.clone()),
        PaneTargetRef::Id {
            session_name,
            pane_id,
        } => resolve_pane_id(state, session_name, *pane_id),
    }
}

fn resolve_pane_id(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    pane_id: rmux_proto::PaneId,
) -> Result<PaneTarget, RmuxError> {
    let session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| session_not_found(session_name))?;
    let window_index = session
        .window_index_for_pane_id(pane_id)
        .ok_or_else(|| RmuxError::pane_not_found(session_name.clone(), pane_id))?;
    let pane_index = session
        .window_at(window_index)
        .and_then(|window| {
            window
                .panes()
                .iter()
                .find(|pane| pane.id() == pane_id)
                .map(|pane| pane.index())
        })
        .ok_or_else(|| RmuxError::pane_not_found(session_name.clone(), pane_id))?;
    Ok(PaneTarget::with_window(
        session_name.clone(),
        window_index,
        pane_index,
    ))
}
