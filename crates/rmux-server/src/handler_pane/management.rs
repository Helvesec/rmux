use rmux_core::LifecycleEvent;
use rmux_proto::{
    CommandOutput, ErrorResponse, HookName, PaneStateClosedReason, Response, ScopeSelector,
    SessionId, SessionName, Target, WindowTarget,
};

use super::super::{
    client_environment_snapshot, client_spawn_environment, prepare_lifecycle_event,
    scripting_support::{format_context_for_target, render_start_directory_template},
    RequestHandler,
};
use crate::format_runtime::render_runtime_template;
use crate::hook_runtime::PendingInlineHookFormat;
use crate::pane_io::AttachControl;
use crate::pane_state_journal::PaneStateChange;
use crate::pane_terminals::HandlerState;
use crate::terminal::validate_process_command;

use super::pane_split_effects::{apply_split_window_effects, split_window_effects};

const DEFAULT_BREAK_PANE_FORMAT: &str = "#{session_name}:#{window_index}.#{pane_index}";

#[derive(Debug, Clone)]
struct UnlinkedWindowSnapshot {
    target: WindowTarget,
    window_id: u32,
    window_name: String,
}

pub(in crate::handler) struct SplitWindowParts {
    pub(in crate::handler) target: rmux_proto::SplitWindowTarget,
    pub(in crate::handler) direction: rmux_proto::SplitDirection,
    pub(in crate::handler) before: bool,
    pub(in crate::handler) environment_overrides: Option<Vec<String>>,
    pub(in crate::handler) command: Option<Vec<String>>,
    pub(in crate::handler) process_command: Option<rmux_proto::ProcessCommand>,
    pub(in crate::handler) start_directory: Option<std::path::PathBuf>,
    pub(in crate::handler) keep_alive_on_exit: Option<bool>,
    pub(in crate::handler) detached: bool,
    pub(in crate::handler) size: Option<String>,
    pub(in crate::handler) preserve_zoom: bool,
    pub(in crate::handler) full_size: bool,
    pub(in crate::handler) stdin_payload: Option<Vec<u8>>,
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
        let (response, source_window_unlinked, removed_source_sessions) = {
            let mut state = self.state.lock().await;
            let source_group_members = state.sessions.session_group_members(&source_session_name);
            let source_window_unlinked = join_pane_unlinked_window_snapshot(&state, &request);
            let response = match state.join_pane(request) {
                Ok(response) => Response::JoinPane(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            };
            let removed_source_sessions =
                removed_sessions_after_pane_transfer(&state, &response, source_group_members);
            (response, source_window_unlinked, removed_source_sessions)
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
            self.exit_removed_source_sessions(&removed_source_sessions)
                .await;
            if !removed_source_sessions.contains(&source_session_name) {
                self.refresh_attached_session(&source_session_name).await;
            }
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
        let explicit_name = request.name.is_some();
        let (response, removed_source_sessions) = {
            let mut state = self.state.lock().await;
            let source_group_members = state.sessions.session_group_members(&source_session_name);
            let response = match state.break_pane(request) {
                Ok(response) => Response::BreakPane(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            };
            let removed_source_sessions =
                removed_sessions_after_pane_transfer(&state, &response, source_group_members);
            (response, removed_source_sessions)
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
            self.exit_removed_source_sessions(&removed_source_sessions)
                .await;
            if !removed_source_sessions.contains(&source_session_name) {
                self.refresh_attached_session(&source_session_name).await;
            }
            if source_session_name != target_session_name {
                self.refresh_attached_session(&target_session_name).await;
            }
            if !explicit_name {
                if let Response::BreakPane(success) = &response {
                    self.refresh_automatic_window_name_for_pane_target(&success.target)
                        .await;
                }
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

    async fn exit_removed_source_sessions(&self, removed_sessions: &[SessionName]) {
        for session_name in removed_sessions {
            self.exit_attached_session(session_name).await;
        }
    }

    pub(in crate::handler) async fn handle_split_window(
        &self,
        requester_pid: u32,
        request: rmux_proto::SplitWindowRequest,
    ) -> Response {
        self.handle_split_window_parts(
            requester_pid,
            SplitWindowParts {
                target: request.target,
                direction: request.direction,
                before: request.before,
                environment_overrides: request.environment,
                command: None,
                process_command: None,
                start_directory: None,
                keep_alive_on_exit: None,
                detached: false,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_split_window_ext(
        &self,
        requester_pid: u32,
        request: rmux_proto::SplitWindowExtRequest,
    ) -> Response {
        self.handle_split_window_parts(
            requester_pid,
            SplitWindowParts {
                target: request.target,
                direction: request.direction,
                before: request.before,
                environment_overrides: request.environment,
                command: request.command,
                process_command: request.process_command,
                start_directory: request.start_directory,
                keep_alive_on_exit: request.keep_alive_on_exit,
                detached: request.detached,
                size: request.size,
                preserve_zoom: request.preserve_zoom,
                full_size: request.full_size,
                stdin_payload: request.stdin_payload,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_split_window_parts(
        &self,
        requester_pid: u32,
        parts: SplitWindowParts,
    ) -> Response {
        let SplitWindowParts {
            target,
            direction,
            before,
            environment_overrides,
            command,
            process_command,
            start_directory,
            keep_alive_on_exit,
            detached,
            size,
            preserve_zoom,
            full_size,
            stdin_payload,
        } = parts;
        let session_name = match &target {
            rmux_proto::SplitWindowTarget::Session(session_name) => session_name.clone(),
            rmux_proto::SplitWindowTarget::Pane(target) => target.session_name().clone(),
        };
        let format_target = match &target {
            rmux_proto::SplitWindowTarget::Session(session_name) => {
                Target::Session(session_name.clone())
            }
            rmux_proto::SplitWindowTarget::Pane(target) => Target::Pane(target.clone()),
        };
        let socket_path = self.socket_path();
        let process_command = process_command
            .or_else(|| crate::legacy_command::from_legacy_command(command.as_deref()));
        if let Err(error) = validate_process_command(process_command.as_ref()) {
            return Response::Error(ErrorResponse { error });
        }
        let attached_count = self.attached_count(&session_name).await;
        let client_environment = client_environment_snapshot(requester_pid);
        let spawn_environment = client_spawn_environment(client_environment.as_ref());
        let response = {
            let mut state = self.state.lock().await;
            let start_directory = match render_start_directory_template(
                &state,
                &format_target,
                attached_count,
                start_directory,
            ) {
                Ok(start_directory) => start_directory,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let split_effects =
                split_window_effects(&state, &target, direction, detached, size.as_deref());
            let split_effects = match split_effects {
                Ok(effects) => effects,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            match state.split_window(
                target,
                direction,
                before,
                &socket_path,
                spawn_environment.as_ref(),
                environment_overrides.as_deref(),
                process_command.as_ref(),
                start_directory.as_deref(),
                keep_alive_on_exit,
                split_effects.size,
                full_size,
                Some(self.pane_alert_callback()),
                Some(self.pane_exit_callback()),
            ) {
                Ok(response) => match apply_split_window_effects(
                    &mut state,
                    &response.pane,
                    split_effects,
                    preserve_zoom,
                ) {
                    Ok(()) => {
                        if let Some(payload) = stdin_payload
                            .as_deref()
                            .filter(|payload| !payload.is_empty())
                        {
                            if let Err(error) = inject_split_window_stdin_output(
                                &mut state,
                                &response.pane,
                                payload,
                            ) {
                                return Response::Error(ErrorResponse { error });
                            }
                        }
                        if is_split_window_stdin_dead_pane(
                            process_command.as_ref(),
                            keep_alive_on_exit,
                            stdin_payload.as_deref(),
                        ) {
                            if let Err(error) =
                                state.mark_pane_dead_without_exit_details(&response.pane)
                            {
                                return Response::Error(ErrorResponse { error });
                            }
                        }
                        Response::SplitWindow(response)
                    }
                    Err(error) => Response::Error(ErrorResponse { error }),
                },
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
            removed_session,
            removed_subscription_keys,
            removed_pane_ids,
        ) = {
            let mut state = self.state.lock().await;
            let removed_subscription_keys = state
                .pane_output_subscription_keys_for_kill(&request.target, request.kill_all_except)
                .unwrap_or_default();
            match state.kill_pane_with_options(request.target, request.kill_all_except) {
                Ok(result) => {
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
                        None,
                        queued_session,
                        result.session_destroyed,
                        result
                            .removed_session_id
                            .map(|session_id| (session_name.clone(), SessionId::new(session_id))),
                        removed_subscription_keys,
                        result.removed_pane_ids,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    None,
                    None,
                    false,
                    None,
                    Vec::new(),
                    Vec::new(),
                ),
            }
        };

        self.prune_web_session(removed_session);

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
            for pane_id in &removed_pane_ids {
                self.record_pane_state_change(
                    *pane_id,
                    None,
                    PaneStateChange::Closed {
                        reason: PaneStateClosedReason::Killed,
                    },
                );
            }
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
}

fn removed_sessions_after_pane_transfer(
    state: &HandlerState,
    response: &Response,
    source_group_members: Vec<SessionName>,
) -> Vec<SessionName> {
    if !matches!(response, Response::JoinPane(_) | Response::BreakPane(_)) {
        return Vec::new();
    }
    source_group_members
        .into_iter()
        .filter(|session_name| state.sessions.session(session_name).is_none())
        .collect()
}

fn inject_split_window_stdin_output(
    state: &mut HandlerState,
    target: &rmux_proto::PaneTarget,
    payload: &[u8],
) -> Result<(), rmux_proto::RmuxError> {
    let payload = normalize_split_window_stdin_payload(payload);
    let transcript = state.transcript_handle(target)?;
    transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .append_bytes(&payload);
    let pane_output = state.pane_output_for_target(
        target.session_name(),
        target.window_index(),
        target.pane_index(),
    )?;
    let _ = pane_output.send_for_generation(None, payload);
    Ok(())
}

fn normalize_split_window_stdin_payload(payload: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(payload.len().saturating_mul(2));
    let mut previous_was_cr = false;
    for byte in payload {
        match *byte {
            b'\n' if previous_was_cr => {
                normalized.push(b'\n');
                previous_was_cr = false;
            }
            b'\n' => {
                normalized.push(b'\r');
                normalized.push(b'\n');
                previous_was_cr = false;
            }
            b'\r' => {
                normalized.push(b'\r');
                previous_was_cr = true;
            }
            byte => {
                normalized.push(byte);
                previous_was_cr = false;
            }
        }
    }
    normalized
}

fn is_split_window_stdin_dead_pane(
    process_command: Option<&rmux_proto::ProcessCommand>,
    keep_alive_on_exit: Option<bool>,
    stdin_payload: Option<&[u8]>,
) -> bool {
    keep_alive_on_exit == Some(true)
        && stdin_payload.is_some()
        && process_command.is_some_and(rmux_proto::ProcessCommand::is_empty)
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
