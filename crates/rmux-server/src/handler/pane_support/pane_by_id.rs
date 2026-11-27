use rmux_proto::{
    ErrorResponse, HookName, OptionName, Response, ScopeSelector, SessionId, SessionName,
    SetOptionMode, Target, WindowTarget,
};

use super::super::{
    attach_support::SessionDetachOnDestroy, subscription_support::capture_pane_stream_sources,
    RequestHandler, SelectionTransitionSnapshot,
};
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
use crate::pane_terminal_lookup::pane_id_for_target;

mod target_ref;

pub(crate) use target_ref::resolve_pane_target_ref;

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
        let captured = {
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
            super::super::StableTargetIdentity::capture(&mut state, Target::Pane(target.clone()))
                .map(|identity| (target, identity))
        };
        let (target, identity) = match captured {
            Ok(captured) => captured,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        super::super::with_expected_stable_target_identities(
            vec![identity],
            None,
            None,
            self.handle_resize_pane(rmux_proto::ResizePaneRequest {
                target,
                adjustment: request.adjustment,
            }),
        )
        .await
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
            destroyed_attached_sessions,
            removed_subscription_keys,
            removed_pane_ids,
            resize_targets,
            after_hook_target,
            post_lifecycle_events,
        ) = {
            let mut state = self.state.lock().await;
            let detach_on_destroy = SessionDetachOnDestroy::capture_all(&state);
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
            let previous_stream_sources =
                capture_pane_stream_sources(&state, &previous_subscription_keys);
            let timer_mutation = self.plan_all_window_mutation_silence_timers_locked(&state);
            let selection_before = SelectionTransitionSnapshot::capture(&state);
            match state.remove_pane_alias_with_options(target.clone(), request.kill_all_except) {
                Ok(result) => {
                    state.retire_removed_lifecycle_targets();
                    self.apply_window_mutation_silence_timers_and_arm_all_locked(
                        &state,
                        timer_mutation,
                        Vec::new(),
                        &[],
                    );
                    let mut affected_sessions = result.affected_sessions.clone();
                    state.expand_with_active_window_linked_session_families(&mut affected_sessions);
                    let destroyed_sessions = result.destroyed_sessions.clone();
                    let destroyed_attached_sessions = destroyed_sessions
                        .iter()
                        .filter_map(|(session_name, session_id)| {
                            let session_id = SessionId::new(*session_id);
                            detach_on_destroy
                                .get(&session_id)
                                .copied()
                                .map(|policy| (session_name.clone(), session_id, policy))
                        })
                        .collect::<Vec<_>>();
                    let after_hook_target =
                        after_kill_pane_target(&state, &result.hook_context, &affected_sessions);
                    let layout_target =
                        WindowTarget::with_window(session_name.clone(), layout_window);
                    let (lifecycle_events, post_lifecycle_events) = hook_batch
                        .prepare_explicit_committed(
                            &mut state,
                            &destroyed_sessions,
                            &selection_before,
                            layout_target,
                            result.response.window_destroyed,
                        );
                    if !result.session_destroyed && !result.response.window_destroyed {
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
                    self.stage_removed_pane_stream_sources(
                        &removed_subscription_keys,
                        previous_stream_sources,
                    );
                    #[cfg(test)]
                    super::super::pane_family_lifecycle_tests::pause_before_pane_kill_subscription_rekey(
                        &session_name,
                    )
                    .await;
                    // Keep the runtime-owner mutation and its registry rekey
                    // in one state -> subscriptions transaction. Otherwise a
                    // later B -> C owner transfer can commit before this
                    // operation's delayed A -> B rekey and strand records on B.
                    self.rekey_pane_output_subscriptions(&subscription_rekeys);
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
                        destroyed_attached_sessions,
                        removed_subscription_keys,
                        result.removed_pane_ids,
                        resize_targets,
                        after_hook_target,
                        post_lifecycle_events,
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
                    None,
                    Vec::new(),
                ),
            }
        };

        if matches!(response, Response::KillPane(_)) {
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
        let mut prepared_attached_switches = std::collections::HashMap::new();
        if matches!(response, Response::KillPane(_)) {
            for (session_name, session_id, detach_on_destroy) in &destroyed_attached_sessions {
                let prepared = self
                    .prepare_destroy_session_rehome(session_name, *session_id, *detach_on_destroy)
                    .await;
                prepared_attached_switches.insert(*session_id, prepared);
            }
        }
        for event in lifecycle_events {
            self.emit_prepared(event).await;
        }
        for (_, session_id, _) in &destroyed_attached_sessions {
            if let Some(prepared) = prepared_attached_switches.get_mut(session_id) {
                for event in prepared.control_lifecycle_events.drain(..) {
                    self.emit_prepared(event).await;
                }
            }
        }
        for event in post_lifecycle_events {
            self.emit_prepared(event).await;
        }
        if matches!(response, Response::KillPane(_)) {
            self.drain_removed_pane_output_subscriptions(&removed_subscription_keys)
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
            for (session_name, session_id, _detach_on_destroy) in destroyed_attached_sessions {
                let prepared = prepared_attached_switches
                    .remove(&session_id)
                    .expect("pane kill-by-id destroy rehome must be prepared before publication");
                self.exit_prepared_attached_session_identity(prepared).await;
                self.cancel_session_silence_timers(&session_name).await;
                self.refresh_control_session(&session_name).await;
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
        }

        response
    }

    pub(in crate::handler) async fn handle_pane_respawn_ref(
        &self,
        request: rmux_proto::PaneRespawnRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let socket_path = self.socket_path();
        let (response, respawned_pane_id, focus_effects) = {
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
                    state.retire_respawned_lifecycle_panes(&[pane_id]);
                    if let Some(outcome) = keep_alive_outcome.as_ref() {
                        let generation =
                            state.pane_output_generation_for_target(&generation_target, pane_id);
                        self.record_pane_option_mutation(pane_id, Some(generation), outcome);
                    }
                    let mut active_attach = self.active_attach.lock().await;
                    let focus_effects = super::super::focus_support::prepare_server_focus_effects(
                        &mut state,
                        &mut active_attach,
                    );
                    (
                        Response::RespawnPane(response),
                        Some(pane_id),
                        Some(focus_effects),
                    )
                }
                Err(error) => {
                    if let Some(previous_options) = previous_options {
                        state.options = previous_options;
                    }
                    (Response::Error(ErrorResponse { error }), None, None)
                }
            }
        };

        if respawned_pane_id.is_some() {
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
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
}
