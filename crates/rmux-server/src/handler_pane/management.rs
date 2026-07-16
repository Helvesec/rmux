use rmux_core::LifecycleEvent;
use rmux_proto::{
    ErrorResponse, HookName, PaneId, Response, RmuxError, ScopeSelector, SessionId, Target,
    WindowTarget,
};

use super::super::{
    attach_support::SessionDetachOnDestroy, client_environment_snapshot, client_spawn_environment,
    scripting_support::render_start_directory_template, RequestHandler,
};
use crate::hook_runtime::PendingInlineHookFormat;
use crate::pane_io::AttachControl;
use crate::pane_terminals::{resolve_new_pane_process_command, HandlerState};
use crate::terminal::validate_process_command;

use super::pane_kill_effects::{after_kill_pane_target, KillPaneLifecycleBatch};
use super::pane_split_effects::{apply_split_window_effects, split_window_effects};

pub(in crate::handler) struct SplitWindowParts {
    pub(in crate::handler) target: rmux_proto::SplitWindowTarget,
    pub(in crate::handler) expected_pane_id: Option<PaneId>,
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
    pub(in crate::handler) response_mode: SplitWindowResponseMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum SplitWindowResponseMode {
    Legacy,
    StableIdentity,
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_split_window(
        &self,
        requester_pid: u32,
        request: rmux_proto::SplitWindowRequest,
    ) -> Response {
        self.handle_split_window_parts(
            requester_pid,
            SplitWindowParts {
                target: request.target,
                expected_pane_id: None,
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
                response_mode: SplitWindowResponseMode::Legacy,
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
                expected_pane_id: None,
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
                response_mode: SplitWindowResponseMode::Legacy,
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
            expected_pane_id,
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
            response_mode,
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
        let explicit_process_command = process_command
            .or_else(|| crate::legacy_command::from_legacy_command(command.as_deref()));
        if let Err(error) = validate_process_command(explicit_process_command.as_ref()) {
            return Response::Error(ErrorResponse { error });
        }
        let attached_count = self.attached_count(&session_name).await;
        let client_environment = client_environment_snapshot(requester_pid);
        let spawn_environment = client_spawn_environment(client_environment.as_ref());
        let (response, successful_pane) = {
            let mut state = self.state.lock().await;
            if let Err(error) =
                super::super::require_expected_session_identity(&state, &session_name)
            {
                return Response::Error(ErrorResponse { error });
            }
            if let Err(error) =
                require_expected_split_pane_identity(&state, &target, expected_pane_id)
            {
                return Response::Error(ErrorResponse { error });
            }
            let process_command = resolve_new_pane_process_command(
                &state.options,
                &session_name,
                explicit_process_command,
            );
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
                detached,
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
                        let successful_pane = response.pane.clone();
                        match split_window_response(&state, response, response_mode) {
                            Ok(response) => (response, Some(successful_pane)),
                            Err(error) => (Response::Error(ErrorResponse { error }), None),
                        }
                    }
                    Err(error) => (Response::Error(ErrorResponse { error }), None),
                },
                Err(error) => (Response::Error(ErrorResponse { error }), None),
            }
        };

        if let Some(successful_pane) = successful_pane {
            self.queue_inline_hook(
                HookName::AfterSplitWindow,
                ScopeSelector::Session(session_name.clone()),
                Some(Target::Pane(successful_pane.clone())),
                PendingInlineHookFormat::AfterCommand,
            );
            self.emit(LifecycleEvent::WindowLayoutChanged {
                target: WindowTarget::with_window(
                    session_name.clone(),
                    successful_pane.window_index(),
                ),
            })
            .await;
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
            lifecycle_events,
            affected_sessions,
            destroyed_sessions,
            destroyed_attached_sessions,
            removed_subscription_keys,
            removed_pane_ids,
            after_hook_target,
        ) = {
            let mut state = self.state.lock().await;
            let target_window = WindowTarget::with_window(
                request.target.session_name().clone(),
                request.target.window_index(),
            );
            if let Err(error) =
                super::super::require_expected_window_identity(&state, &target_window)
            {
                return Response::Error(ErrorResponse { error });
            }
            let detach_on_destroy = SessionDetachOnDestroy::capture_all(&state);
            let hook_batch =
                KillPaneLifecycleBatch::capture(&state, &request.target, request.kill_all_except);
            let removed_subscription_keys = state
                .pane_output_subscription_keys_for_kill(&request.target, request.kill_all_except)
                .unwrap_or_default();
            let timer_mutation = self.plan_all_window_mutation_silence_timers_locked(&state);
            match state.kill_pane_with_options(request.target, request.kill_all_except) {
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
                    let lifecycle_events =
                        hook_batch.prepare_committed(&mut state, &destroyed_sessions);
                    let after_hook_target =
                        after_kill_pane_target(&state, &result.hook_context, &affected_sessions);
                    if !result.session_destroyed && result.response.window_destroyed {
                        let _ = state.hooks.remove_window(&WindowTarget::with_window(
                            session_name.clone(),
                            target.window_index(),
                        ));
                    } else if !result.session_destroyed {
                        let _ = state.hooks.remove_pane(&target);
                    }
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    (
                        Response::KillPane(result.response),
                        lifecycle_events,
                        affected_sessions,
                        destroyed_sessions,
                        destroyed_attached_sessions,
                        removed_subscription_keys,
                        result.removed_pane_ids,
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
                    None,
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
                    .rehome_control_session_identity(session_name, *session_id, *detach_on_destroy)
                    .await;
                prepared_attached_switches.insert(*session_id, prepared);
            }
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
            for (session_name, session_id, detach_on_destroy) in destroyed_attached_sessions {
                if let Some(prepared) = prepared_attached_switches.remove(&session_id) {
                    self.exit_prepared_attached_session_identity(prepared).await;
                } else {
                    self.exit_attached_session_identity(
                        &session_name,
                        session_id,
                        detach_on_destroy,
                    )
                    .await;
                }
                self.cancel_session_silence_timers(&session_name).await;
                self.refresh_control_session(&session_name).await;
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
                        target: WindowTarget::with_window(
                            session_name.clone(),
                            target.window_index(),
                        ),
                    })
                    .await;
                }
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

fn require_expected_split_pane_identity(
    state: &HandlerState,
    target: &rmux_proto::SplitWindowTarget,
    expected_pane_id: Option<PaneId>,
) -> Result<(), RmuxError> {
    let Some(expected_pane_id) = expected_pane_id else {
        return Ok(());
    };
    let rmux_proto::SplitWindowTarget::Pane(target) = target else {
        return Err(RmuxError::Server(
            "stable pane split resolved to a non-pane target".to_owned(),
        ));
    };
    let resolved = state
        .sessions
        .resolve_pane(&Target::Pane(target.clone()))
        .is_ok_and(|pane| pane.id() == expected_pane_id);
    if resolved {
        Ok(())
    } else {
        Err(RmuxError::pane_not_found(
            target.session_name().clone(),
            expected_pane_id,
        ))
    }
}

fn split_window_response(
    state: &HandlerState,
    response: rmux_proto::SplitWindowResponse,
    mode: SplitWindowResponseMode,
) -> Result<Response, rmux_proto::RmuxError> {
    if mode == SplitWindowResponseMode::Legacy {
        return Ok(Response::SplitWindow(response));
    }

    let raw_target = &response.pane;
    let session = state
        .sessions
        .session(raw_target.session_name())
        .ok_or_else(|| {
            rmux_proto::RmuxError::SessionNotFound(raw_target.session_name().to_string())
        })?;
    let pane_id = crate::pane_terminal_lookup::pane_id_for_target(
        &state.sessions,
        raw_target.session_name(),
        raw_target.window_index(),
        raw_target.pane_index(),
    )?;
    let visible_index = crate::pane_indices::visible_pane_index(
        session,
        &state.options,
        raw_target.window_index(),
        raw_target.pane_index(),
    );

    Ok(Response::SplitWindowIdentity(
        rmux_proto::SplitWindowIdentityResponse {
            pane: rmux_proto::PaneTarget::with_window(
                raw_target.session_name().clone(),
                raw_target.window_index(),
                visible_index,
            ),
            pane_id,
        },
    ))
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
