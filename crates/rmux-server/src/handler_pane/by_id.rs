use rmux_core::LifecycleEvent;
use rmux_proto::{
    ErrorResponse, HookName, OptionName, PaneTarget, PaneTargetRef, ResizePaneAdjustment,
    ResizePaneResponse, Response, RmuxError, ScopeSelector, SelectPaneResponse, SessionId,
    SessionName, SetOptionMode, Target, WindowTarget,
};

use super::super::RequestHandler;
#[cfg(windows)]
use super::pane_io_encoding::{
    prepare_pane_console_input_write, tokens_emulate_windows_cmd_select_all,
    tokens_route_windows_control_as_pty_bytes, windows_console_input_for_target_tokens,
    write_windows_console_input_action_to_target_io,
};
use super::pane_kill_effects::{after_kill_pane_target, KillPaneLifecycleBatch};
#[cfg(windows)]
use super::pane_windows_console_sequence::prepare_single_pane_windows_console_input_sequence;
use super::{
    encode_tokens_for_target, prepare_pane_input_write, write_bytes_to_target, PaneInputLiveness,
};
use crate::hook_runtime::PendingInlineHookFormat;
use crate::pane_state_journal::PaneStateChange;
use crate::pane_terminal_lookup::pane_id_for_target;
use crate::pane_terminals::{session_not_found, HandlerState};

impl RequestHandler {
    pub(in crate::handler) async fn handle_pane_input_ref(
        &self,
        request: rmux_proto::PaneInputRequest,
    ) -> Response {
        let key_count = request.keys.len();
        let prepared = {
            let mut state = self.state.lock().await;
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
            #[cfg(windows)]
            if !request.literal
                && !tokens_emulate_windows_cmd_select_all(&state, &target, &request.keys)
            {
                match prepare_single_pane_windows_console_input_sequence(
                    &mut state,
                    &target,
                    &request.keys,
                    None,
                ) {
                    Ok(Some(steps)) => {
                        drop(state);
                        return self
                            .write_windows_console_input_sequence_and_mark_interactive(
                                steps, key_count,
                            )
                            .await;
                    }
                    Ok(None) => {}
                    Err(error) => return Response::Error(ErrorResponse { error }),
                }
                if let Some((action, console_bytes)) =
                    windows_console_input_for_target_tokens(&state, &target, &request.keys, 1)
                {
                    if tokens_route_windows_control_as_pty_bytes(&state, &target, &request.keys) {
                        let write = match prepare_pane_input_write(
                            &mut state,
                            &target,
                            &bytes,
                            PaneInputLiveness::TolerateDead,
                        ) {
                            Ok(write) => write,
                            Err(error) => return Response::Error(ErrorResponse { error }),
                        };
                        let session_name = write.session_name().clone();
                        let wrote_bytes = !bytes.is_empty();
                        drop(state);
                        let response = write_bytes_to_target(write, bytes, key_count).await;
                        return self
                            .mark_single_pane_input_ref_as_interactive(
                                &session_name,
                                wrote_bytes,
                                response,
                            )
                            .await;
                    }
                    let write = match prepare_pane_console_input_write(
                        &mut state,
                        &target,
                        &console_bytes,
                        action,
                    ) {
                        Ok(write) => write,
                        Err(error) => return Response::Error(ErrorResponse { error }),
                    };
                    let session_name = write.session_name().clone();
                    let wrote_bytes = !console_bytes.is_empty();
                    drop(state);
                    let response = match write_windows_console_input_action_to_target_io(
                        write, action,
                    )
                    .await
                    {
                        Ok(()) => Response::SendKeys(rmux_proto::SendKeysResponse { key_count }),
                        Err(error) => Response::Error(ErrorResponse { error }),
                    };
                    return self
                        .mark_single_pane_input_ref_as_interactive(
                            &session_name,
                            wrote_bytes,
                            response,
                        )
                        .await;
                }
            }
            let write = match prepare_pane_input_write(
                &mut state,
                &target,
                &bytes,
                PaneInputLiveness::TolerateDead,
            ) {
                Ok(write) => write,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            (write, bytes)
        };

        let session_name = prepared.0.session_name().clone();
        let wrote_bytes = !prepared.1.is_empty();
        let response = write_bytes_to_target(prepared.0, prepared.1, key_count).await;
        self.mark_single_pane_input_ref_as_interactive(&session_name, wrote_bytes, response)
            .await
    }

    async fn mark_single_pane_input_ref_as_interactive(
        &self,
        session_name: &SessionName,
        wrote_bytes: bool,
        response: Response,
    ) -> Response {
        if wrote_bytes && matches!(response, Response::SendKeys(_)) {
            self.mark_attached_session_interactive_input(session_name)
                .await;
        }
        response
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
            if let Err(error) =
                super::super::require_expected_session_identity(&state, target.session_name())
            {
                return Response::Error(ErrorResponse { error });
            }
            let window_index = target.window_index();
            let pane_index = target.pane_index();
            let response_target = target.clone();
            let response = match adjustment {
                ResizePaneAdjustment::TrimBelow => {
                    match state.trim_pane_below_cursor(&response_target) {
                        Ok(()) => Response::ResizePane(ResizePaneResponse {
                            target: response_target,
                            adjustment,
                        }),
                        Err(error) => Response::Error(ErrorResponse { error }),
                    }
                }
                _ => match state.mutate_session_and_resize_terminals(&session_name, |session| {
                    session.resize_pane_in_window(window_index, pane_index, adjustment)?;
                    Ok(ResizePaneResponse {
                        target: response_target,
                        adjustment,
                    })
                }) {
                    Ok(response) => Response::ResizePane(response),
                    Err(error) => Response::Error(ErrorResponse { error }),
                },
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
            // See handle_resize_pane in layout.rs: skip the refresh (and its
            // Windows deferred-pane wait) when nothing is attached so a
            // still-starting sibling cannot stall a detached resize.
            if self.attached_count(&session_name).await > 0 {
                self.refresh_attached_session(&session_name).await;
            }
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
            lifecycle_events,
            affected_sessions,
            destroyed_sessions,
            removed_subscription_keys,
            subscription_rekeys,
            removed_pane_ids,
            resize_targets,
            layout_window,
            after_hook_target,
        ) = {
            let mut state = self.state.lock().await;
            let target = match resolve_pane_target_ref(&state, &request.target) {
                Ok(target) => target,
                Err(error) => {
                    return Response::Error(ErrorResponse { error });
                }
            };
            let target_window =
                WindowTarget::with_window(target.session_name().clone(), target.window_index());
            if let Err(error) =
                super::super::require_expected_window_identity(&state, &target_window)
            {
                return Response::Error(ErrorResponse { error });
            }
            let hook_batch =
                KillPaneLifecycleBatch::capture(&state, &target, request.kill_all_except);
            let layout_window = target.window_index();
            let linked_targets =
                state.window_linked_window_targets(target.session_name(), target.window_index());
            let target_window_id = state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .map(rmux_core::Window::id);
            let previous_subscription_keys = state
                .pane_output_subscription_keys_for_kill(&target, request.kill_all_except)
                .unwrap_or_default();
            let timer_mutation = self.plan_all_window_mutation_silence_timers_locked(&state);
            match state.remove_pane_alias_with_options(target.clone(), request.kill_all_except) {
                Ok(result) => {
                    self.apply_window_mutation_silence_timers_and_arm_all_locked(
                        &state,
                        timer_mutation,
                        Vec::new(),
                        &[],
                    );
                    let mut affected_sessions = result.affected_sessions.clone();
                    state.expand_with_active_window_linked_session_families(&mut affected_sessions);
                    let destroyed_sessions = result.destroyed_sessions.clone();
                    let lifecycle_events =
                        hook_batch.prepare_committed(&mut state, &destroyed_sessions);
                    let after_hook_target =
                        after_kill_pane_target(&state, &result.hook_context, &affected_sessions);
                    if !result.session_destroyed && result.response.window_destroyed {
                        let _ = state.hooks.remove_window(&WindowTarget::with_window(
                            session_name.clone(),
                            layout_window,
                        ));
                    } else if !result.session_destroyed {
                        let _ = state.hooks.remove_pane(&target);
                    }
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    let mut removed_subscription_keys = Vec::new();
                    let mut subscription_rekeys = Vec::new();
                    for previous_key in previous_subscription_keys {
                        if result.removed_pane_ids.contains(&previous_key.pane_id()) {
                            removed_subscription_keys.push(previous_key);
                            continue;
                        }
                        if let Some(current_key) = state
                            .pane_output_subscription_key_for_pane_id(previous_key.pane_id())
                            .filter(|current_key| current_key != &previous_key)
                        {
                            subscription_rekeys.push((previous_key, current_key));
                        }
                    }
                    let resize_targets = result
                        .response
                        .window_destroyed
                        .then(|| {
                            target_window_id.and_then(|window_id| {
                                linked_targets.into_iter().find(|target| {
                                    state
                                        .sessions
                                        .session(target.session_name())
                                        .and_then(|session| {
                                            session.window_at(target.window_index())
                                        })
                                        .is_some_and(|window| window.id() == window_id)
                                })
                            })
                        })
                        .flatten()
                        .into_iter()
                        .collect();
                    (
                        Response::KillPane(result.response),
                        lifecycle_events,
                        affected_sessions,
                        destroyed_sessions,
                        removed_subscription_keys,
                        subscription_rekeys,
                        result.removed_pane_ids,
                        resize_targets,
                        layout_window,
                        after_hook_target,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    layout_window,
                    None,
                ),
            }
        };

        if matches!(response, Response::KillPane(_)) {
            self.rekey_pane_output_subscriptions(&subscription_rekeys);
            match after_hook_target {
                Some(target) => self.queue_exact_pane_inline_hook(
                    HookName::AfterKillPane,
                    target.target,
                    target.identity,
                    PendingInlineHookFormat::AfterCommand,
                ),
                None => self.queue_missing_target_inline_hook(
                    HookName::AfterKillPane,
                    PendingInlineHookFormat::AfterCommand,
                ),
            }
        }

        for (destroyed_session, session_id) in &destroyed_sessions {
            self.prune_web_session(Some((
                destroyed_session.clone(),
                SessionId::new(*session_id),
            )));
        }

        if !removed_pane_ids.is_empty() {
            self.forget_pane_snapshot_coalescers(&removed_pane_ids);
        }
        for event in lifecycle_events {
            self.emit_prepared(event).await;
        }
        if matches!(response, Response::KillPane(_)) {
            self.cleanup_pane_output_subscriptions(&removed_subscription_keys)
                .await;
            let destroyed_names = destroyed_sessions
                .iter()
                .map(|(destroyed_session, _)| destroyed_session.clone())
                .collect::<Vec<_>>();
            let destroyed_identities = destroyed_sessions
                .iter()
                .map(|(session_name, session_id)| {
                    (session_name.clone(), SessionId::new(*session_id))
                })
                .collect::<Vec<_>>();
            self.remove_session_leases(&destroyed_identities);
            for affected_session in &affected_sessions {
                if let Some((_, session_id)) = destroyed_identities
                    .iter()
                    .find(|(session_name, _)| session_name == affected_session)
                {
                    self.exit_attached_session_identity(affected_session, *session_id)
                        .await;
                    self.cancel_session_silence_timers(affected_session).await;
                    self.refresh_control_session(affected_session).await;
                }
            }
            for resize_target in resize_targets {
                let _ = self
                    .reconcile_attached_window_size_and_emit(&resize_target)
                    .await;
            }
            for affected_session in affected_sessions {
                if destroyed_names.contains(&affected_session) {
                    continue;
                }
                let _ = self
                    .reconcile_attached_session_size_and_emit(&affected_session)
                    .await;
                self.dismiss_mode_tree_for_session(&affected_session).await;
                self.refresh_attached_session(&affected_session).await;
            }
            if !destroyed_names.is_empty() {
                let _ = self.queue_shutdown_if_server_empty().await;
            }
            if let Response::KillPane(success) = &response {
                if !success.window_destroyed {
                    self.emit(LifecycleEvent::WindowLayoutChanged {
                        target: WindowTarget::with_window(session_name.clone(), layout_window),
                    })
                    .await;
                }
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
        let (response, respawned_pane_id) = {
            let mut state = self.state.lock().await;
            let target = match resolve_pane_target_ref(&state, &request.target) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let previous_options = request.keep_alive_on_exit.map(|_| state.options.clone());
            let keep_alive_outcome = if let Some(keep_alive) = request.keep_alive_on_exit {
                match state.options.set(
                    ScopeSelector::Pane(target.clone()),
                    OptionName::RemainOnExit,
                    if keep_alive { "on" } else { "off" }.to_owned(),
                    SetOptionMode::Replace,
                ) {
                    Ok(outcome) => Some(outcome),
                    Err(error) => return Response::Error(ErrorResponse { error }),
                }
            } else {
                None
            };
            if keep_alive_outcome.is_some() {
                if let Err(error) = state.synchronize_pane_alias_options_from_target(&target) {
                    if let Some(previous_options) = previous_options {
                        state.options = previous_options;
                    }
                    return Response::Error(ErrorResponse { error });
                }
            }
            let request = rmux_proto::RespawnPaneRequest {
                target,
                kill: request.kill,
                start_directory: request.start_directory,
                environment: request.environment,
                command: request.command,
                process_command: request.process_command,
            };
            let pane_id = match pane_id_for_target(
                &state.sessions,
                request.target.session_name(),
                request.target.window_index(),
                request.target.pane_index(),
            ) {
                Ok(pane_id) => pane_id,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let generation_target = request.target.clone();
            match state.respawn_pane(
                request,
                &socket_path,
                None,
                Some(self.pane_alert_callback()),
                Some(self.pane_exit_callback()),
                |_, _| {},
            ) {
                Ok(response) => {
                    self.record_pane_respawn_boundary(pane_id);
                    if let Some(outcome) = keep_alive_outcome.as_ref() {
                        let generation =
                            state.pane_output_generation_for_target(&generation_target, pane_id);
                        self.record_pane_option_mutation(pane_id, Some(generation), outcome);
                    }
                    (Response::RespawnPane(response), Some(pane_id))
                }
                Err(error) => {
                    if let Some(previous_options) = previous_options {
                        state.options = previous_options;
                    }
                    (Response::Error(ErrorResponse { error }), None)
                }
            }
        };

        if respawned_pane_id.is_some() {
            self.refresh_attached_session(&session_name).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_pane_snapshot_ref(
        &self,
        request: rmux_proto::PaneSnapshotRefRequest,
    ) -> Response {
        let inputs = {
            let state = self.state.lock().await;
            let target = match resolve_pane_target_ref(&state, &request.target) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            match self.resolve_pane_snapshot_inputs(&state, &target) {
                Ok(inputs) => inputs,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };
        self.handle_pane_snapshot_inputs(inputs)
    }

    pub(in crate::handler) async fn handle_pane_select_ref(
        &self,
        request: rmux_proto::PaneSelectRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let title = request.title.clone();
        let (response, pane_changed, window_index, title_changed_target) = {
            let mut state = self.state.lock().await;
            let target = match resolve_pane_target_ref(&state, &request.target) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            if let Err(error) =
                super::super::require_expected_session_identity(&state, target.session_name())
            {
                return Response::Error(ErrorResponse { error });
            }
            let window_index = target.window_index();
            let pane_index = target.pane_index();
            let pane_changed = title.is_none()
                && state
                    .sessions
                    .session(&session_name)
                    .and_then(|session| session.window_at(window_index))
                    .is_some_and(|window| window.active_pane_index() != pane_index);
            let mut title_changed_target = None;
            let mut title_state_event = None;
            match (|| -> Result<SelectPaneResponse, RmuxError> {
                let response_target = if let Some(title) = title.as_deref() {
                    if let Some((old, new)) = state.set_pane_title(&target, title)? {
                        title_changed_target = Some(target.clone());
                        if let Some(pane_id) = pane_id_for_select_target(&state, &target) {
                            let generation =
                                state.pane_output_generation_for_target(&target, pane_id);
                            title_state_event = Some((pane_id, generation, old, new));
                        }
                    }
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
                Ok(response) => {
                    if let Some((pane_id, generation, old, new)) = &title_state_event {
                        self.record_pane_state_change(
                            *pane_id,
                            Some(*generation),
                            PaneStateChange::TitleChanged {
                                old: old.clone(),
                                new: new.clone(),
                            },
                        );
                    }
                    (
                        Response::SelectPane(response),
                        pane_changed,
                        window_index,
                        title_changed_target,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    false,
                    window_index,
                    None,
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
            if let Some(target) = title_changed_target {
                self.emit(LifecycleEvent::PaneTitleChanged { target }).await;
            }
            if let Response::SelectPane(success) = &response {
                self.queue_inline_hook(
                    HookName::AfterSelectPane,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Pane(success.target.clone())),
                    PendingInlineHookFormat::AfterCommand,
                );
            }
            // See handle_select_pane in handler_pane/selection.rs: skip the
            // refresh (and its Windows deferred-pane wait) when nothing is
            // attached so a still-starting sibling cannot stall a detached
            // select via the pane-id-typed SDK API either.
            if self.attached_count(&session_name).await > 0 {
                self.refresh_attached_session(&session_name).await;
            }
        }

        response
    }
}

fn pane_id_for_select_target(
    state: &HandlerState,
    target: &PaneTarget,
) -> Option<rmux_core::PaneId> {
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
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
        } => {
            match super::super::resolve_expected_window_pane_target(state, session_name, *pane_id)?
            {
                Some(target) => Ok(target),
                None => resolve_pane_id(state, session_name, *pane_id),
            }
        }
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
