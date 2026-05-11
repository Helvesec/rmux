use rmux_core::LifecycleEvent;
use rmux_proto::{
    CommandOutput, ErrorResponse, HookName, Response, ScopeSelector, Target, WindowTarget,
};

use super::super::{
    prepare_lifecycle_event, scripting_support::format_context_for_target, RequestHandler,
};
use crate::format_runtime::render_runtime_template;
use crate::hook_runtime::PendingInlineHookFormat;
use crate::pane_io::AttachControl;
use crate::pane_terminals::HandlerState;

const DEFAULT_BREAK_PANE_FORMAT: &str = "#{session_name}:#{window_index}.#{pane_index}";

#[derive(Debug, Clone)]
struct UnlinkedWindowSnapshot {
    target: WindowTarget,
    window_id: u32,
    window_name: String,
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_swap_pane(
        &self,
        request: rmux_proto::SwapPaneRequest,
    ) -> Response {
        let source_session_name = request.source.session_name().clone();
        let target_session_name = request.target.session_name().clone();
        let source_window =
            WindowTarget::with_window(source_session_name.clone(), request.source.window_index());
        let target_window =
            WindowTarget::with_window(target_session_name.clone(), request.target.window_index());
        let response = {
            let mut state = self.state.lock().await;
            match state.swap_pane(request) {
                Ok(response) => Response::SwapPane(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::SwapPane(_)) {
            self.emit(LifecycleEvent::WindowLayoutChanged {
                target: source_window.clone(),
            })
            .await;
            if source_window != target_window {
                self.emit(LifecycleEvent::WindowLayoutChanged {
                    target: target_window,
                })
                .await;
            }
            self.refresh_attached_session(&source_session_name).await;
            if source_session_name != target_session_name {
                self.refresh_attached_session(&target_session_name).await;
            }
        }

        response
    }

    pub(in crate::handler) async fn handle_join_pane(
        &self,
        request: rmux_proto::JoinPaneRequest,
    ) -> Response {
        let source_session_name = request.source.session_name().clone();
        let target_session_name = request.target.session_name().clone();
        let source_window =
            WindowTarget::with_window(source_session_name.clone(), request.source.window_index());
        let target_window =
            WindowTarget::with_window(target_session_name.clone(), request.target.window_index());
        let (response, source_window_unlinked) = {
            let mut state = self.state.lock().await;
            let source_window_unlinked = join_pane_unlinked_window_snapshot(&state, &request);
            match state.join_pane(request) {
                Ok(response) => (Response::JoinPane(response), source_window_unlinked),
                Err(error) => (Response::Error(ErrorResponse { error }), None),
            }
        };

        if matches!(response, Response::JoinPane(_)) {
            self.sync_session_silence_timers(&source_session_name).await;
            if source_session_name != target_session_name {
                self.sync_session_silence_timers(&target_session_name).await;
            }
            if let Some(window) = source_window_unlinked {
                self.emit(LifecycleEvent::WindowUnlinked {
                    session_name: source_session_name.clone(),
                    target: Some(window.target),
                    window_id: Some(window.window_id),
                    window_name: Some(window.window_name),
                })
                .await;
            }
            self.emit(LifecycleEvent::WindowLayoutChanged {
                target: source_window.clone(),
            })
            .await;
            if source_window != target_window {
                self.emit(LifecycleEvent::WindowLayoutChanged {
                    target: target_window,
                })
                .await;
            }
            self.refresh_attached_session(&source_session_name).await;
            if source_session_name != target_session_name {
                self.refresh_attached_session(&target_session_name).await;
            }
        }

        response
    }

    pub(in crate::handler) async fn handle_move_pane(
        &self,
        request: rmux_proto::MovePaneRequest,
    ) -> Response {
        let source_session_name = request.source.session_name().clone();
        let target_session_name = request.target.session_name().clone();
        let source_window =
            WindowTarget::with_window(source_session_name.clone(), request.source.window_index());
        let target_window =
            WindowTarget::with_window(target_session_name.clone(), request.target.window_index());
        let (response, source_window_unlinked) = {
            let mut state = self.state.lock().await;
            let source_window_unlinked = join_pane_unlinked_window_snapshot(
                &state,
                &rmux_proto::JoinPaneRequest {
                    source: request.source.clone(),
                    target: request.target.clone(),
                    direction: request.direction,
                    detached: request.detached,
                    before: request.before,
                    full_size: request.full_size,
                    size: request.size,
                },
            );
            match state.move_pane(request) {
                Ok(response) => (Response::MovePane(response), source_window_unlinked),
                Err(error) => (Response::Error(ErrorResponse { error }), None),
            }
        };

        if matches!(response, Response::MovePane(_)) {
            self.sync_session_silence_timers(&source_session_name).await;
            if source_session_name != target_session_name {
                self.sync_session_silence_timers(&target_session_name).await;
            }
            if let Some(window) = source_window_unlinked {
                self.emit(LifecycleEvent::WindowUnlinked {
                    session_name: source_session_name.clone(),
                    target: Some(window.target),
                    window_id: Some(window.window_id),
                    window_name: Some(window.window_name),
                })
                .await;
            }
            self.emit(LifecycleEvent::WindowLayoutChanged {
                target: source_window.clone(),
            })
            .await;
            if source_window != target_window {
                self.emit(LifecycleEvent::WindowLayoutChanged {
                    target: target_window,
                })
                .await;
            }
            self.refresh_attached_session(&source_session_name).await;
            if source_session_name != target_session_name {
                self.refresh_attached_session(&target_session_name).await;
            }
        }

        response
    }

    pub(in crate::handler) async fn handle_break_pane(
        &self,
        request: rmux_proto::BreakPaneRequest,
    ) -> Response {
        let source_session_name = request.source.session_name().clone();
        let source_window =
            WindowTarget::with_window(source_session_name.clone(), request.source.window_index());
        let target_session_name = request.target.as_ref().map_or_else(
            || source_session_name.clone(),
            |target| target.session_name().clone(),
        );
        let print_target = request.print_target;
        let print_format = request.format.clone();
        let response = {
            let mut state = self.state.lock().await;
            match state.break_pane(request) {
                Ok(response) => Response::BreakPane(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::BreakPane(_)) {
            self.sync_session_silence_timers(&source_session_name).await;
            if source_session_name != target_session_name {
                self.sync_session_silence_timers(&target_session_name).await;
            }
            self.emit(LifecycleEvent::WindowLayoutChanged {
                target: source_window.clone(),
            })
            .await;
            if let Response::BreakPane(success) = &response {
                let target_window = WindowTarget::with_window(
                    success.target.session_name().clone(),
                    success.target.window_index(),
                );
                self.emit(LifecycleEvent::WindowLinked {
                    session_name: target_session_name.clone(),
                    target: Some(target_window.clone()),
                })
                .await;
                if source_window != target_window {
                    self.emit(LifecycleEvent::WindowLayoutChanged {
                        target: target_window,
                    })
                    .await;
                }
            }
            self.refresh_attached_session(&source_session_name).await;
            if source_session_name != target_session_name {
                self.refresh_attached_session(&target_session_name).await;
            }
        }

        if print_target {
            let template = print_format.as_deref().unwrap_or(DEFAULT_BREAK_PANE_FORMAT);
            if let Response::BreakPane(success) = &response {
                let attached_count = self.attached_count(success.target.session_name()).await;
                let output = {
                    let state = self.state.lock().await;
                    let runtime = format_context_for_target(
                        &state,
                        &Target::Pane(success.target.clone()),
                        attached_count,
                    )
                    .map_err(|error| ErrorResponse { error });
                    match runtime {
                        Ok(runtime) => Some(CommandOutput::from_stdout(
                            format!("{}\n", render_runtime_template(template, &runtime, false))
                                .into_bytes(),
                        )),
                        Err(error) => return Response::Error(error),
                    }
                };
                return Response::BreakPane(rmux_proto::BreakPaneResponse {
                    target: success.target.clone(),
                    output,
                });
            }
        }

        response
    }

    pub(in crate::handler) async fn handle_split_window(
        &self,
        request: rmux_proto::SplitWindowRequest,
    ) -> Response {
        self.handle_split_window_parts(request.target, request.direction, request.environment, None)
            .await
    }

    pub(in crate::handler) async fn handle_split_window_ext(
        &self,
        request: rmux_proto::SplitWindowExtRequest,
    ) -> Response {
        self.handle_split_window_parts(
            request.target,
            request.direction,
            request.environment,
            request.command,
        )
        .await
    }

    async fn handle_split_window_parts(
        &self,
        target: rmux_proto::SplitWindowTarget,
        direction: rmux_proto::SplitDirection,
        environment_overrides: Option<Vec<String>>,
        command: Option<Vec<String>>,
    ) -> Response {
        let session_name = match &target {
            rmux_proto::SplitWindowTarget::Session(session_name) => session_name.clone(),
            rmux_proto::SplitWindowTarget::Pane(target) => target.session_name().clone(),
        };
        let socket_path = self.socket_path();
        let response = {
            let mut state = self.state.lock().await;
            match state.split_window(
                target,
                direction,
                &socket_path,
                environment_overrides.as_deref(),
                command.as_deref(),
                Some(self.pane_alert_callback()),
                Some(self.pane_exit_callback()),
            ) {
                Ok(response) => Response::SplitWindow(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::SplitWindow(_)) {
            if let Response::SplitWindow(success) = &response {
                self.queue_inline_hook(
                    HookName::AfterSplitWindow,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Pane(success.pane.clone())),
                    PendingInlineHookFormat::AfterCommand,
                );
                self.emit(LifecycleEvent::WindowLayoutChanged {
                    target: WindowTarget::with_window(
                        session_name.clone(),
                        success.pane.window_index(),
                    ),
                })
                .await;
            }
            self.refresh_attached_session(&session_name).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_kill_pane(
        &self,
        request: rmux_proto::KillPaneRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let target = request.target.clone();
        let (
            response,
            queued_pane_exited,
            queued_session_closed,
            session_destroyed,
            removed_subscription_keys,
            removed_pane_ids,
        ) = {
            let mut state = self.state.lock().await;
            let removed_subscription_keys = state
                .pane_output_subscription_keys_for_kill(&request.target, request.kill_all_except)
                .unwrap_or_default();
            match state.kill_pane_with_options(request.target, request.kill_all_except) {
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
                            target.window_index(),
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
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    None,
                    None,
                    false,
                    Vec::new(),
                    Vec::new(),
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
                self.exit_attached_session(&session_name).await;
                self.cancel_session_silence_timers(&session_name).await;
                self.refresh_control_session(&session_name).await;
                let _ = self.queue_shutdown_if_server_empty().await;
            } else {
                self.sync_session_silence_timers(&session_name).await;
                if let Response::KillPane(success) = &response {
                    if !success.window_destroyed {
                        self.emit(LifecycleEvent::WindowLayoutChanged {
                            target: WindowTarget::with_window(
                                session_name.clone(),
                                target.window_index(),
                            ),
                        })
                        .await;
                    }
                }
                self.dismiss_mode_tree_for_session(&session_name).await;
                self.refresh_attached_session(&session_name).await;
            }
        }

        response
    }

    pub(in crate::handler) async fn dismiss_mode_tree_for_session(
        &self,
        session_name: &rmux_proto::SessionName,
    ) {
        let mut active_attach = self.active_attach.lock().await;
        for active in active_attach.by_pid.values_mut() {
            if &active.session_name != session_name || active.suspended {
                continue;
            }
            if active.mode_tree.is_none() {
                continue;
            }
            active.mode_tree = None;
            active.mode_tree_frame = None;
            active.mode_tree_state_id = active.mode_tree_state_id.saturating_add(1);
            active.persistent_overlay_epoch.store(
                active.mode_tree_state_id,
                std::sync::atomic::Ordering::SeqCst,
            );
            active.overlay_generation = active.overlay_generation.saturating_add(1);
            let _ = active
                .control_tx
                .send(AttachControl::AdvancePersistentOverlayState(
                    active.mode_tree_state_id,
                ));
        }
    }

    pub(in crate::handler) async fn handle_pipe_pane(
        &self,
        _requester_pid: u32,
        request: rmux_proto::PipePaneRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let target = request.target.clone();
        let attached_count = self.attached_count(&session_name).await;
        let write_to_pipe = if !request.stdin && !request.stdout {
            true
        } else {
            request.stdout
        };
        let response = {
            let mut state = self.state.lock().await;
            let command = match request.command.as_deref() {
                Some(command) => {
                    let runtime = match format_context_for_target(
                        &state,
                        &Target::Pane(target.clone()),
                        attached_count,
                    ) {
                        Ok(runtime) => runtime,
                        Err(error) => return Response::Error(ErrorResponse { error }),
                    };
                    Some(render_runtime_template(command, &runtime, true))
                }
                None => None,
            };

            match state.pipe_pane(
                target.clone(),
                command,
                request.stdin,
                write_to_pipe,
                request.once,
            ) {
                Ok(response) => Response::PipePane(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::PipePane(_)) {
            self.queue_inline_hook(
                HookName::AfterPipePane,
                ScopeSelector::Pane(target.clone()),
                Some(Target::Pane(target)),
                PendingInlineHookFormat::AfterCommand,
            );
        }

        response
    }

    pub(in crate::handler) async fn handle_respawn_pane(
        &self,
        request: rmux_proto::RespawnPaneRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let socket_path = self.socket_path();
        let response = {
            let mut state = self.state.lock().await;
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
}

fn join_pane_unlinked_window_snapshot(
    state: &HandlerState,
    request: &rmux_proto::JoinPaneRequest,
) -> Option<UnlinkedWindowSnapshot> {
    if request.source.session_name() == request.target.session_name()
        && request.source.window_index() == request.target.window_index()
    {
        return None;
    }

    let window = state
        .sessions
        .session(request.source.session_name())
        .and_then(|session| session.window_at(request.source.window_index()))
        .filter(|window| window.pane_count() == 1)?;

    Some(UnlinkedWindowSnapshot {
        target: WindowTarget::with_window(
            request.source.session_name().clone(),
            request.source.window_index(),
        ),
        window_id: window.id().as_u32(),
        window_name: window.name().unwrap_or_default().to_owned(),
    })
}
