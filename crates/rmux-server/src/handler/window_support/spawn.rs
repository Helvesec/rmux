//! Window creation and respawn: `new-window` and `respawn-window`.

use super::*;

impl RequestHandler {
    pub(in crate::handler) async fn handle_new_window(
        &self,
        requester_pid: u32,
        request: rmux_proto::NewWindowRequest,
    ) -> Response {
        #[cfg(windows)]
        let wait_for_deferred_pane_pid = !request.detached
            || request.start_directory.as_ref().is_some_and(|path| {
                format_references_pane_pid(Some(path.as_os_str().to_string_lossy().as_ref()))
            });
        let session_name = request.target;
        let environment_overrides = request.environment;
        let start_directory = request.start_directory;
        let command = request.command;
        let explicit_process_command = request
            .process_command
            .or_else(|| crate::legacy_command::from_legacy_command(command.as_deref()));
        let socket_path = self.socket_path();
        let client_environment = client_environment_snapshot(requester_pid);
        let spawn_environment = client_spawn_environment(client_environment.as_ref());
        let attached_count = self.attached_count(&session_name).await;
        let client_loop_snapshots = self.client_loop_snapshots().await;
        let client_name = client_name_for_pid(&client_loop_snapshots, requester_pid);
        #[cfg(windows)]
        if wait_for_deferred_pane_pid {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
        let (response, focus_effects, lifecycle_events) = {
            let mut state = self.state.lock().await;
            if let Err(error) =
                super::super::require_expected_session_identity(&state, &session_name)
            {
                return Response::Error(ErrorResponse { error });
            }
            let process_command = resolve_new_pane_process_command(
                &state.options,
                &session_name,
                explicit_process_command,
            );
            let timer_sessions = state.sessions.session_group_members(&session_name);
            let timer_mutation =
                self.plan_window_mutation_silence_timers_locked(&state, timer_sessions);
            let start_directory = match render_start_directory_template(
                &state,
                &Target::Session(session_name.clone()),
                attached_count,
                start_directory,
                Some(&client_loop_snapshots),
                client_name,
            ) {
                Ok(start_directory) => start_directory,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let options = NewWindowOptions {
                name: request.name,
                detached: request.detached,
                spawn: WindowSpawnOptions {
                    start_directory: start_directory.as_deref(),
                    command: process_command.as_ref(),
                    socket_path: &socket_path,
                    spawn_environment: spawn_environment.as_ref(),
                    environment_overrides: environment_overrides.as_deref(),
                    respawn_shell: None,
                    respawn_environment: None,
                    pane_alert_callback: Some(self.pane_alert_callback()),
                    pane_exit_callback: Some(self.pane_exit_callback()),
                },
            };
            let selection_before = SelectionTransitionSnapshot::capture(&state);
            let result = match request.target_window_index {
                Some(window_index) => state.create_window_at_requested_index(
                    &session_name,
                    Some(window_index),
                    request.insert_at_target,
                    options,
                ),
                None => state.create_window(&session_name, options),
            };
            match result {
                Ok(response) => {
                    let mut timer_targets = Vec::new();
                    for timer_session_name in state.sessions.session_group_members(&session_name) {
                        let Some(session) = state.sessions.session(&timer_session_name) else {
                            continue;
                        };
                        timer_targets.extend(session.windows().map(|(index, _)| index).map(
                            |window_index| {
                                WindowTarget::with_window(timer_session_name.clone(), window_index)
                            },
                        ));
                    }
                    self.apply_window_mutation_silence_timers_locked(
                        &state,
                        timer_mutation,
                        Vec::new(),
                        &[],
                        timer_targets,
                    );
                    let mut active_attach = self.active_attach.lock().await;
                    let focus_effects =
                        prepare_server_focus_effects(&mut state, &mut active_attach);
                    let mut lifecycle_events = selection_before.prepare_session_window_changes(
                        &mut state,
                        std::slice::from_ref(&session_name),
                    );
                    lifecycle_events.extend(super::super::prepare_lifecycle_event_if_enabled(
                        &mut state,
                        &LifecycleEvent::WindowLinked {
                            session_name: session_name.clone(),
                            target: Some(response.target.clone()),
                        },
                    ));
                    (
                        Response::NewWindow(response),
                        Some(focus_effects),
                        lifecycle_events,
                    )
                }
                Err(error) => (Response::Error(ErrorResponse { error }), None, Vec::new()),
            }
        };

        if matches!(response, Response::NewWindow(_)) {
            if let Response::NewWindow(success) = &response {
                {
                    let mut active_attach = self.active_attach.lock().await;
                    active_attach.seed_active_client_for_window(
                        requester_pid,
                        success.target.session_name(),
                        success.target.window_index(),
                    );
                }
                self.bump_active_attach_epoch();
                self.queue_inline_hook(
                    HookName::AfterNewWindow,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Pane(PaneTarget::with_window(
                        success.target.session_name().clone(),
                        success.target.window_index(),
                        0,
                    ))),
                    PendingInlineHookFormat::AfterCommand,
                );
                if let Some(focus_effects) = focus_effects {
                    let _ = focus_effects.execute(self).await;
                }
                if !lifecycle_events.is_empty() {
                    self.pause_before_window_lifecycle_emit().await;
                    for event in lifecycle_events {
                        self.emit_prepared(event).await;
                    }
                }
            }
            self.refresh_attached_session(&session_name).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_respawn_window(
        &self,
        requester_pid: u32,
        mut request: rmux_proto::RespawnWindowRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let target = request.target.clone();
        let socket_path = self.socket_path();
        let process_command =
            crate::legacy_command::from_legacy_command(request.command.as_deref());
        let client_environment = client_environment_snapshot(requester_pid);
        let spawn_environment = client_spawn_environment(client_environment.as_ref());
        let attached_count = self.attached_count(&session_name).await;
        let client_loop_snapshots = self.client_loop_snapshots().await;
        let client_name = client_name_for_pid(&client_loop_snapshots, requester_pid);
        let (response, removed_pane_ids, subscriptions_removed, refresh_sessions, focus_effects) = {
            let mut state = self.state.lock().await;
            if let Err(error) =
                super::super::require_expected_window_identity(&state, &request.target)
            {
                return Response::Error(ErrorResponse { error });
            }
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                std::slice::from_ref(&session_name),
            );
            request.start_directory = match render_start_directory_template(
                &state,
                &Target::Window(target),
                attached_count,
                request.start_directory,
                Some(&client_loop_snapshots),
                client_name,
            ) {
                Ok(start_directory) => start_directory,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            match state.respawn_window(
                request.target,
                RespawnWindowOptions {
                    kill: request.kill,
                    spawn: WindowSpawnOptions {
                        start_directory: request.start_directory.as_deref(),
                        command: process_command.as_ref(),
                        socket_path: &socket_path,
                        spawn_environment: spawn_environment.as_ref(),
                        environment_overrides: request.environment.as_deref(),
                        respawn_shell: None,
                        respawn_environment: None,
                        pane_alert_callback: Some(self.pane_alert_callback()),
                        pane_exit_callback: Some(self.pane_exit_callback()),
                    },
                },
            ) {
                Ok(result) => {
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    self.record_pane_respawn_boundary(result.retained_pane_id);
                    let mut respawned_pane_ids = result.removed_pane_ids.clone();
                    respawned_pane_ids.push(result.retained_pane_id);
                    state.retire_respawned_lifecycle_panes(&respawned_pane_ids);
                    let subscriptions_removed = self.apply_pane_output_subscription_reconciliation(
                        subscription_keys.reconcile_after(&state),
                    );
                    let mut active_attach = self.active_attach.lock().await;
                    let focus_effects =
                        prepare_server_focus_effects(&mut state, &mut active_attach);
                    (
                        Response::RespawnWindow(result.response),
                        result.removed_pane_ids,
                        subscriptions_removed,
                        result.refresh_sessions,
                        Some(focus_effects),
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    false,
                    Vec::new(),
                    None,
                ),
            }
        };

        if subscriptions_removed {
            let _ = self.request_shutdown_if_pending();
        }

        if !removed_pane_ids.is_empty() {
            self.forget_pane_snapshot_coalescers(&removed_pane_ids);
        }
        if matches!(&response, Response::RespawnWindow(_)) {
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
            for refresh_session in refresh_sessions {
                self.refresh_attached_session(&refresh_session).await;
            }
        }

        response
    }
}
