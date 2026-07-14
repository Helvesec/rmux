use std::collections::{HashMap, HashSet};

use rmux_core::LifecycleEvent;
use rmux_proto::{
    ErrorResponse, HookName, PaneTarget, Response, ScopeSelector, SessionName, Target, WindowTarget,
};

#[cfg(windows)]
use super::pane_support::format_references_pane_pid;
use super::{
    attach_support::surviving_attached_resize_targets, client_environment_snapshot,
    client_spawn_environment, scripting_support::render_start_directory_template,
    PaneOutputSubscriptionKeySnapshot, RequestHandler,
};
use crate::hook_runtime::{hooks_disabled, PendingInlineHookFormat};
use crate::pane_terminals::{
    resolve_new_pane_process_command, HandlerState, NewWindowOptions, RespawnWindowOptions,
    WindowSpawnOptions,
};

#[path = "handler_window/move_window_effects.rs"]
mod move_window_effects;
#[path = "handler_window/timer_mutations.rs"]
mod timer_mutations;

use move_window_effects::MoveWindowEffects;
use timer_mutations::{move_window_timer_target_overrides, swap_window_timer_target_overrides};

fn linked_resize_sessions_for_window_change(
    state: &HandlerState,
    session_name: &SessionName,
    previous_window_index: u32,
    next_window_index: u32,
) -> Vec<SessionName> {
    let mut seen = HashSet::new();
    let mut sessions = Vec::new();
    for window_index in [previous_window_index, next_window_index] {
        for linked_session in state.window_linked_sessions_list(session_name, window_index) {
            if seen.insert(linked_session.clone()) {
                sessions.push(linked_session);
            }
        }
    }
    if sessions.is_empty() && seen.insert(session_name.clone()) {
        sessions.push(session_name.clone());
    }
    sessions
}

fn active_window_ids_by_session(
    state: &HandlerState,
) -> HashMap<SessionName, rmux_proto::WindowId> {
    state
        .sessions
        .iter()
        .map(|(session_name, session)| (session_name.clone(), session.window().id()))
        .collect()
}

fn changed_active_window_ids(
    previous: &HashMap<SessionName, rmux_proto::WindowId>,
    state: &HandlerState,
) -> Vec<rmux_proto::WindowId> {
    let mut changed = Vec::new();
    for (session_name, session) in state.sessions.iter() {
        let active_window_id = session.window().id();
        let previous_window_id = previous.get(session_name).copied();
        if previous_window_id != Some(active_window_id) {
            changed.push(active_window_id);
            if let Some(previous_window_id) = previous_window_id {
                changed.push(previous_window_id);
            }
        }
    }
    changed.sort_by_key(|window_id| window_id.as_u32());
    changed.dedup();
    changed
}

impl RequestHandler {
    async fn reconcile_and_refresh_attached_sessions(&self, sessions: Vec<SessionName>) {
        for session_name in sessions {
            let _ = self
                .reconcile_attached_session_size_and_emit(&session_name)
                .await;
            self.refresh_attached_session(&session_name).await;
        }
    }

    pub(super) async fn handle_new_window(
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
        #[cfg(windows)]
        if wait_for_deferred_pane_pid {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
        let (response, linked_event) = {
            let mut state = self.state.lock().await;
            if let Err(error) = super::require_expected_session_identity(&state, &session_name) {
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
                        timer_targets.extend(session.windows().keys().copied().map(
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
                    let linked_event = super::prepare_lifecycle_event_if_enabled(
                        &mut state,
                        &LifecycleEvent::WindowLinked {
                            session_name: session_name.clone(),
                            target: Some(response.target.clone()),
                        },
                    );
                    (Response::NewWindow(response), linked_event)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), None),
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
                if let Some(linked_event) = linked_event {
                    self.pause_before_window_lifecycle_emit().await;
                    self.emit_prepared(linked_event).await;
                }
            }
            self.refresh_attached_session(&session_name).await;
        }

        response
    }

    pub(super) async fn handle_kill_window(
        &self,
        request: rmux_proto::KillWindowRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let (
            response,
            removed_windows,
            removed_pane_ids,
            lifecycle_events,
            resize_window_ids,
            subscriptions_removed,
        ) = {
            let mut state = self.state.lock().await;
            if let Err(error) = super::require_expected_window_identity(&state, &request.target) {
                return Response::Error(ErrorResponse { error });
            }
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                std::slice::from_ref(&session_name),
            );
            let active_window_ids_before = active_window_ids_by_session(&state);
            let timer_sessions = state
                .sessions
                .iter()
                .map(|(session_name, _)| session_name.clone())
                .collect();
            let timer_mutation =
                self.plan_window_mutation_silence_timers_locked(&state, timer_sessions);
            match state.kill_window(request.target, request.kill_all_others) {
                Ok(result) => {
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    let removed_timer_targets = result
                        .removed_windows
                        .iter()
                        .map(|removed| removed.target.clone())
                        .collect();
                    self.apply_window_mutation_silence_timers_locked(
                        &state,
                        timer_mutation,
                        removed_timer_targets,
                        &result.reindexed_windows,
                        Vec::new(),
                    );
                    let lifecycle_events = if hooks_disabled() {
                        Vec::new()
                    } else {
                        result
                            .removed_windows
                            .iter()
                            .map(|removed_window| {
                                super::prepare_lifecycle_event(
                                    &mut state,
                                    &LifecycleEvent::WindowUnlinked {
                                        session_name: removed_window.target.session_name().clone(),
                                        target: Some(removed_window.target.clone()),
                                        window_id: Some(removed_window.window_id),
                                        window_name: Some(removed_window.window_name.clone()),
                                    },
                                )
                            })
                            .collect()
                    };
                    let mut resize_window_ids = result
                        .removed_windows
                        .iter()
                        .map(|removed_window| rmux_proto::WindowId::new(removed_window.window_id))
                        .collect::<Vec<_>>();
                    resize_window_ids
                        .extend(changed_active_window_ids(&active_window_ids_before, &state));
                    resize_window_ids.sort_by_key(|window_id| window_id.as_u32());
                    resize_window_ids.dedup();
                    let subscriptions_removed = self.apply_pane_output_subscription_reconciliation(
                        subscription_keys.reconcile_after(&state),
                    );
                    (
                        Response::KillWindow(result.response),
                        result.removed_windows,
                        result.removed_pane_ids,
                        lifecycle_events,
                        resize_window_ids,
                        subscriptions_removed,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    false,
                ),
            }
        };

        if subscriptions_removed {
            let _ = self.request_shutdown_if_pending();
        }

        if matches!(response, Response::KillWindow(_)) {
            self.pause_before_window_lifecycle_emit().await;
            self.forget_pane_snapshot_coalescers(&removed_pane_ids);
            let mut affected_sessions = removed_windows
                .iter()
                .map(|removed_window| removed_window.target.session_name().clone())
                .collect::<HashSet<_>>();
            let _ = affected_sessions.insert(session_name.clone());
            {
                let mut active_attach = self.active_attach.lock().await;
                for removed_window in &removed_windows {
                    active_attach.forget_window(&removed_window.target);
                }
            }
            self.bump_active_attach_epoch();
            for lifecycle_event in lifecycle_events {
                self.emit_prepared(lifecycle_event).await;
            }
            let resize_targets = {
                let state = self.state.lock().await;
                let resize_targets = surviving_attached_resize_targets(&state, resize_window_ids);
                for resize_target in &resize_targets {
                    affected_sessions.extend(state.window_linked_session_family_list(
                        resize_target.session_name(),
                        resize_target.window_index(),
                    ));
                }
                resize_targets
            };
            for resize_target in resize_targets {
                let _ = self
                    .reconcile_attached_window_size_and_emit(&resize_target)
                    .await;
            }
            let mut affected_sessions = affected_sessions.into_iter().collect::<Vec<_>>();
            affected_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            for affected_session in affected_sessions {
                self.refresh_attached_session(&affected_session).await;
            }
        }

        response
    }

    pub(super) async fn handle_select_window(
        &self,
        requester_pid: Option<u32>,
        request: rmux_proto::SelectWindowRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let target_window_index = request.target.window_index();
        let (response, window_changed, resize_sessions, session_id) = {
            let mut state = self.state.lock().await;
            if let Err(error) = super::require_expected_window_identity(&state, &request.target) {
                return Response::Error(ErrorResponse { error });
            }
            let previous_window_index = state
                .sessions
                .session(&session_name)
                .map(|session| session.active_window_index());
            let session_id = state
                .sessions
                .session(&session_name)
                .map(rmux_core::Session::id);
            let window_changed =
                previous_window_index.is_some_and(|window| window != target_window_index);
            let resize_sessions = previous_window_index
                .map(|previous| {
                    linked_resize_sessions_for_window_change(
                        &state,
                        &session_name,
                        previous,
                        target_window_index,
                    )
                })
                .unwrap_or_else(|| vec![session_name.clone()]);
            match state.select_window(request.target) {
                Ok(response) => (
                    Response::SelectWindow(response),
                    window_changed,
                    resize_sessions,
                    session_id,
                ),
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    false,
                    Vec::new(),
                    None,
                ),
            }
        };

        if matches!(response, Response::SelectWindow(_)) {
            if let Some(requester_pid) = requester_pid {
                let mut active_attach = self.active_attach.lock().await;
                active_attach.seed_active_client_for_window(
                    requester_pid,
                    &session_name,
                    target_window_index,
                );
                drop(active_attach);
                self.bump_active_attach_epoch();
            }
            if let (true, Some(session_id)) = (window_changed, session_id) {
                let event = LifecycleEvent::SessionWindowChanged {
                    session_name: session_name.clone(),
                };
                self.emit_for_session_identity(event, &session_name, session_id)
                    .await;
            }
            self.queue_inline_hook(
                HookName::AfterSelectWindow,
                ScopeSelector::Session(session_name.clone()),
                Some(Target::Window(rmux_proto::WindowTarget::with_window(
                    session_name.clone(),
                    target_window_index,
                ))),
                PendingInlineHookFormat::AfterCommand,
            );
            self.reconcile_and_refresh_attached_sessions(resize_sessions)
                .await;
        }

        response
    }

    pub(super) async fn handle_rename_window(
        &self,
        request: rmux_proto::RenameWindowRequest,
    ) -> Response {
        let target = request.target.clone();
        let (response, refresh_sessions) = {
            let mut state = self.state.lock().await;
            if let Err(error) = super::require_expected_window_identity(&state, &request.target) {
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

    pub(super) async fn handle_next_window(
        &self,
        request: rmux_proto::NextWindowRequest,
    ) -> Response {
        let session_name = request.target;
        let (response, resize_sessions, session_id) = {
            let mut state = self.state.lock().await;
            let previous_window_index = state
                .sessions
                .session(&session_name)
                .map(|session| session.active_window_index());
            match state.next_window(&session_name, request.alerts_only) {
                Ok(response) => {
                    let session_id = state
                        .sessions
                        .session(&session_name)
                        .map(rmux_core::Session::id)
                        .expect("next-window target session survives its mutation");
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
                    (
                        Response::NextWindow(response),
                        resize_sessions,
                        Some(session_id),
                    )
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new(), None),
            }
        };

        if let (Response::NextWindow(_), Some(session_id)) = (&response, session_id) {
            let event = LifecycleEvent::SessionWindowChanged {
                session_name: session_name.clone(),
            };
            self.emit_for_session_identity(event, &session_name, session_id)
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

    pub(super) async fn handle_previous_window(
        &self,
        request: rmux_proto::PreviousWindowRequest,
    ) -> Response {
        let session_name = request.target;
        let (response, resize_sessions, session_id) = {
            let mut state = self.state.lock().await;
            let previous_window_index = state
                .sessions
                .session(&session_name)
                .map(|session| session.active_window_index());
            match state.previous_window(&session_name, request.alerts_only) {
                Ok(response) => {
                    let session_id = state
                        .sessions
                        .session(&session_name)
                        .map(rmux_core::Session::id)
                        .expect("previous-window target session survives its mutation");
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
                    (
                        Response::PreviousWindow(response),
                        resize_sessions,
                        Some(session_id),
                    )
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new(), None),
            }
        };

        if let (Response::PreviousWindow(_), Some(session_id)) = (&response, session_id) {
            let event = LifecycleEvent::SessionWindowChanged {
                session_name: session_name.clone(),
            };
            self.emit_for_session_identity(event, &session_name, session_id)
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

    pub(super) async fn handle_last_window(
        &self,
        request: rmux_proto::LastWindowRequest,
    ) -> Response {
        let session_name = request.target;
        let (response, resize_sessions, session_id) = {
            let mut state = self.state.lock().await;
            let previous_window_index = state
                .sessions
                .session(&session_name)
                .map(|session| session.active_window_index());
            match state.last_window(&session_name) {
                Ok(response) => {
                    let session_id = state
                        .sessions
                        .session(&session_name)
                        .map(rmux_core::Session::id)
                        .expect("last-window target session survives its mutation");
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
                    (
                        Response::LastWindow(response),
                        resize_sessions,
                        Some(session_id),
                    )
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new(), None),
            }
        };

        if let (Response::LastWindow(_), Some(session_id)) = (&response, session_id) {
            let event = LifecycleEvent::SessionWindowChanged {
                session_name: session_name.clone(),
            };
            self.emit_for_session_identity(event, &session_name, session_id)
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

    pub(super) async fn handle_list_windows(
        &self,
        request: rmux_proto::ListWindowsRequest,
    ) -> Response {
        let attached_count = {
            let active_attach = self.active_attach.lock().await;
            active_attach.attached_count(&request.target)
        };
        #[cfg(windows)]
        if format_references_pane_pid(request.format.as_deref())
            || format_references_pane_pid(request.filter.as_deref())
        {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
        let state = self.state.lock().await;
        match state.list_windows(
            &request.target,
            request.format.as_deref(),
            attached_count,
            request.filter.as_deref(),
            request.sort_order.as_deref(),
            request.reversed,
        ) {
            Ok(response) => Response::ListWindows(response),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(super) async fn handle_link_window(
        &self,
        request: rmux_proto::LinkWindowRequest,
    ) -> Response {
        self.pause_before_window_lifecycle_mutation().await;
        let (
            response,
            removed_destination_pane_ids,
            mut refresh_sessions,
            resize_window_ids,
            linked_event,
            subscriptions_removed,
        ) = {
            let mut state = self.state.lock().await;
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                &[
                    request.source.session_name().clone(),
                    request.target.session_name().clone(),
                ],
            );
            let active_window_ids_before = active_window_ids_by_session(&state);
            let mut resize_window_ids = [
                state
                    .sessions
                    .session(request.source.session_name())
                    .and_then(|session| session.window_at(request.source.window_index()))
                    .map(rmux_core::Window::id),
                state
                    .sessions
                    .session(request.target.session_name())
                    .and_then(|session| session.window_at(request.target.window_index()))
                    .map(rmux_core::Window::id),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            let deadline_fanout =
                self.plan_silence_timer_deadline_fanout_locked(&state, &request.source);
            match state.link_window(request.clone()) {
                Ok(result) => {
                    resize_window_ids
                        .extend(changed_active_window_ids(&active_window_ids_before, &state));
                    resize_window_ids.sort_by_key(|window_id| window_id.as_u32());
                    resize_window_ids.dedup();
                    let destination_timer_sessions = state
                        .sessions
                        .session_group_members(result.response.target.session_name());
                    let destination_timer_targets = destination_timer_sessions
                        .iter()
                        .cloned()
                        .map(|session_name| {
                            rmux_proto::WindowTarget::with_window(
                                session_name,
                                result.response.target.window_index(),
                            )
                        })
                        .collect::<Vec<_>>();
                    let reindexed_timer_sessions = if result.reindexed_windows.is_empty() {
                        Vec::new()
                    } else {
                        destination_timer_sessions
                    };
                    let reindexed_windows = result.reindexed_windows.clone();
                    let refresh_sessions = state.window_linked_session_family_list(
                        result.response.target.session_name(),
                        result.response.target.window_index(),
                    );
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    if let Some(deadline_fanout) = deadline_fanout {
                        deadline_fanout
                            .apply_expired_state_locked(&mut state, &destination_timer_targets);
                    }
                    self.sync_inserted_window_silence_timers_locked(
                        &state,
                        destination_timer_targets,
                        reindexed_timer_sessions,
                        reindexed_windows,
                        deadline_fanout,
                    );
                    let linked_event = super::prepare_lifecycle_event_if_enabled(
                        &mut state,
                        &LifecycleEvent::WindowLinked {
                            session_name: result.response.target.session_name().clone(),
                            target: Some(result.response.target.clone()),
                        },
                    );
                    let subscriptions_removed = self.apply_pane_output_subscription_reconciliation(
                        subscription_keys.reconcile_after(&state),
                    );
                    (
                        Response::LinkWindow(result.response),
                        result.removed_pane_ids,
                        refresh_sessions,
                        resize_window_ids,
                        linked_event,
                        subscriptions_removed,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    None,
                    false,
                ),
            }
        };

        if subscriptions_removed {
            let _ = self.request_shutdown_if_pending();
        }

        if matches!(response, Response::LinkWindow(_)) {
            self.forget_pane_snapshot_coalescers(&removed_destination_pane_ids);
            if let Some(linked_event) = linked_event {
                self.pause_before_window_lifecycle_emit().await;
                self.emit_prepared(linked_event).await;
            }
            let resize_targets = {
                let state = self.state.lock().await;
                let resize_targets = surviving_attached_resize_targets(&state, resize_window_ids);
                for resize_target in &resize_targets {
                    refresh_sessions.extend(state.window_linked_session_family_list(
                        resize_target.session_name(),
                        resize_target.window_index(),
                    ));
                }
                resize_targets
            };
            for resize_target in resize_targets {
                let _ = self
                    .reconcile_attached_window_size_and_emit(&resize_target)
                    .await;
            }
            refresh_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            refresh_sessions.dedup();
            for session_name in refresh_sessions {
                self.refresh_attached_session(&session_name).await;
            }
        }

        response
    }

    pub(super) async fn handle_move_window(
        &self,
        request: rmux_proto::MoveWindowRequest,
    ) -> Response {
        self.pause_before_window_lifecycle_mutation().await;
        let (response, removed_destination_pane_ids, effects, subscriptions_removed) = {
            let mut state = self.state.lock().await;
            let mut subscription_roots = request
                .source
                .as_ref()
                .map(|source| vec![source.session_name().clone()])
                .unwrap_or_default();
            match &request.target {
                rmux_proto::MoveWindowTarget::Session(session_name) => {
                    subscription_roots.push(session_name.clone());
                }
                rmux_proto::MoveWindowTarget::Window(target) => {
                    subscription_roots.push(target.session_name().clone());
                }
            }
            let subscription_keys =
                PaneOutputSubscriptionKeySnapshot::capture_related(&state, &subscription_roots);
            let effects = MoveWindowEffects::capture(&state, &request);
            let timer_overrides = move_window_timer_target_overrides(&state, &request);
            let mut timer_mutation = self.plan_all_window_mutation_silence_timers_locked(&state);
            for (source, destination) in timer_overrides {
                match destination {
                    Some(destination) => timer_mutation.map_target(source, destination),
                    None => timer_mutation.remove_target(source),
                }
            }
            match state.move_window(request.clone()) {
                Ok(result) => {
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    if let (Some(source), Some(destination)) =
                        (request.source.clone(), result.response.target.as_ref())
                    {
                        timer_mutation.fanout_target_to_destination_group_locked(
                            &state,
                            source,
                            destination,
                        );
                    }
                    self.apply_window_mutation_silence_timers_and_arm_all_locked(
                        &state,
                        timer_mutation,
                        Vec::new(),
                        &[],
                    );
                    let subscriptions_removed = self.apply_pane_output_subscription_reconciliation(
                        subscription_keys.reconcile_after(&state),
                    );
                    let effects = effects.prepare_success(&mut state, &result.response);
                    (
                        Response::MoveWindow(result.response),
                        result.removed_pane_ids,
                        Some(effects),
                        subscriptions_removed,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    None,
                    false,
                ),
            }
        };

        if subscriptions_removed {
            let _ = self.request_shutdown_if_pending();
        }

        if let Some(effects) = effects {
            self.forget_pane_snapshot_coalescers(&removed_destination_pane_ids);
            self.finish_move_window_effects(effects).await;
        }

        response
    }

    pub(super) async fn handle_unlink_window(
        &self,
        request: rmux_proto::UnlinkWindowRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        self.pause_before_window_lifecycle_mutation().await;
        let (
            response,
            removed_pane_ids,
            refresh_sessions,
            resize_targets,
            lifecycle_event,
            subscriptions_removed,
        ) = {
            let mut state = self.state.lock().await;
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                std::slice::from_ref(&session_name),
            );
            let mut refresh_sessions = state
                .window_linked_session_family_list(&session_name, request.target.window_index());
            let linked_targets =
                state.window_linked_window_targets(&session_name, request.target.window_index());
            let removed_window_id = state
                .sessions
                .session(&session_name)
                .and_then(|session| session.window_at(request.target.window_index()))
                .map(rmux_core::Window::id);
            refresh_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            refresh_sessions.dedup();
            let timer_sessions = state
                .sessions
                .iter()
                .map(|(session_name, _)| session_name.clone())
                .collect();
            let timer_mutation =
                self.plan_window_mutation_silence_timers_locked(&state, timer_sessions);
            match state.unlink_window(request.target, request.kill_if_last) {
                Ok(result) => {
                    state.expand_with_active_window_linked_session_families(&mut refresh_sessions);
                    let resize_targets = removed_window_id
                        .and_then(|window_id| {
                            linked_targets.into_iter().find(|target| {
                                state
                                    .sessions
                                    .session(target.session_name())
                                    .and_then(|session| session.window_at(target.window_index()))
                                    .is_some_and(|window| window.id() == window_id)
                            })
                        })
                        .into_iter()
                        .collect();
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    self.apply_window_mutation_silence_timers_locked(
                        &state,
                        timer_mutation,
                        result.removed_timer_targets,
                        &result.reindexed_windows,
                        Vec::new(),
                    );
                    let lifecycle_event = if hooks_disabled() {
                        None
                    } else {
                        Some(super::prepare_lifecycle_event(
                            &mut state,
                            &LifecycleEvent::WindowUnlinked {
                                session_name: session_name.clone(),
                                target: Some(result.removed_window.target.clone()),
                                window_id: Some(result.removed_window.window_id),
                                window_name: Some(result.removed_window.window_name.clone()),
                            },
                        ))
                    };
                    let subscriptions_removed = self.apply_pane_output_subscription_reconciliation(
                        subscription_keys.reconcile_after(&state),
                    );
                    (
                        Response::UnlinkWindow(result.response),
                        result.removed_pane_ids,
                        refresh_sessions,
                        resize_targets,
                        lifecycle_event,
                        subscriptions_removed,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    None,
                    false,
                ),
            }
        };

        if subscriptions_removed {
            let _ = self.request_shutdown_if_pending();
        }

        if matches!(response, Response::UnlinkWindow(_)) {
            self.pause_before_window_lifecycle_emit().await;
            self.forget_pane_snapshot_coalescers(&removed_pane_ids);
            if let Some(lifecycle_event) = lifecycle_event {
                self.emit_prepared(lifecycle_event).await;
            }
            for resize_target in resize_targets {
                let _ = self
                    .reconcile_attached_window_size_and_emit(&resize_target)
                    .await;
            }
            for refresh_session in refresh_sessions {
                let _ = self
                    .reconcile_attached_session_size_and_emit(&refresh_session)
                    .await;
                self.refresh_attached_session(&refresh_session).await;
            }
        }

        response
    }

    pub(super) async fn handle_swap_window(
        &self,
        request: rmux_proto::SwapWindowRequest,
    ) -> Response {
        let (response, mut refresh_sessions, resize_window_ids) = {
            let mut state = self.state.lock().await;
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                &[
                    request.source.session_name().clone(),
                    request.target.session_name().clone(),
                ],
            );
            let active_window_ids_before = active_window_ids_by_session(&state);
            let mut resize_window_ids = [&request.source, &request.target]
                .into_iter()
                .filter_map(|target| {
                    state
                        .sessions
                        .session(target.session_name())
                        .and_then(|session| session.window_at(target.window_index()))
                        .map(rmux_core::Window::id)
                })
                .collect::<Vec<_>>();
            let mut refresh_sessions = state.window_linked_session_family_list(
                request.source.session_name(),
                request.source.window_index(),
            );
            for session_name in state.window_linked_session_family_list(
                request.target.session_name(),
                request.target.window_index(),
            ) {
                if !refresh_sessions.contains(&session_name) {
                    refresh_sessions.push(session_name);
                }
            }
            let timer_overrides = swap_window_timer_target_overrides(&state, &request);
            let source = request.source.clone();
            let target = request.target.clone();
            let mut timer_mutation = self.plan_all_window_mutation_silence_timers_locked(&state);
            for (source, destination) in timer_overrides {
                match destination {
                    Some(destination) => timer_mutation.map_target(source, destination),
                    None => timer_mutation.remove_target(source),
                }
            }
            match state.swap_window(request.source, request.target, request.detached) {
                Ok(response) => {
                    timer_mutation.fanout_target_to_destination_group_locked(
                        &state,
                        source,
                        &response.target,
                    );
                    timer_mutation.fanout_target_to_destination_group_locked(
                        &state,
                        target,
                        &response.source,
                    );
                    self.apply_window_mutation_silence_timers_and_arm_all_locked(
                        &state,
                        timer_mutation,
                        Vec::new(),
                        &[],
                    );
                    resize_window_ids
                        .extend(changed_active_window_ids(&active_window_ids_before, &state));
                    resize_window_ids.sort_by_key(|window_id| window_id.as_u32());
                    resize_window_ids.dedup();
                    self.rekey_pane_output_subscriptions(&subscription_keys.rekeys_after(&state));
                    (
                        Response::SwapWindow(response),
                        refresh_sessions,
                        resize_window_ids,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    Vec::new(),
                ),
            }
        };

        if matches!(response, Response::SwapWindow(_)) {
            let resize_targets = {
                let state = self.state.lock().await;
                let resize_targets = surviving_attached_resize_targets(&state, resize_window_ids);
                for resize_target in &resize_targets {
                    refresh_sessions.extend(state.window_linked_session_family_list(
                        resize_target.session_name(),
                        resize_target.window_index(),
                    ));
                }
                resize_targets
            };
            for resize_target in resize_targets {
                let _ = self
                    .reconcile_attached_window_size_and_emit(&resize_target)
                    .await;
            }
            refresh_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            refresh_sessions.dedup();
            for session_name in refresh_sessions {
                self.refresh_attached_session(&session_name).await;
            }
        }

        response
    }

    pub(super) async fn handle_rotate_window(
        &self,
        request: rmux_proto::RotateWindowRequest,
    ) -> Response {
        let target = request.target;
        let (response, refresh_sessions) = {
            let mut state = self.state.lock().await;
            match state.rotate_window(target.clone(), request.direction, request.restore_zoom) {
                Ok(response) => {
                    let refresh_sessions = state.window_linked_session_family_list(
                        target.session_name(),
                        target.window_index(),
                    );
                    (Response::RotateWindow(response), refresh_sessions)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new()),
            }
        };

        if matches!(response, Response::RotateWindow(_)) {
            self.emit(LifecycleEvent::WindowLayoutChanged { target })
                .await;
            for session_name in refresh_sessions {
                self.refresh_attached_session(&session_name).await;
            }
        }

        response
    }

    pub(super) async fn handle_resize_window(
        &self,
        request: rmux_proto::ResizeWindowRequest,
    ) -> Response {
        let request = match self
            .resolve_resize_window_linked_session_size(request)
            .await
        {
            Ok(request) => request,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        let session_name = request.target.session_name().clone();
        let target = request.target.clone();
        let (response, refresh_sessions) = {
            let mut state = self.state.lock().await;
            match state.resize_window(request) {
                Ok(response) => {
                    let refresh_sessions = state.window_linked_session_family_list(
                        target.session_name(),
                        target.window_index(),
                    );
                    (Response::ResizeWindow(response), refresh_sessions)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new()),
            }
        };

        if matches!(response, Response::ResizeWindow(_)) {
            self.queue_inline_hook(
                HookName::AfterResizeWindow,
                ScopeSelector::Session(session_name.clone()),
                Some(Target::Window(target.clone())),
                PendingInlineHookFormat::AfterCommand,
            );
            self.emit(LifecycleEvent::WindowLayoutChanged {
                target: target.clone(),
            })
            .await;
            self.emit(LifecycleEvent::WindowResized { target }).await;
            for refresh_session in refresh_sessions {
                self.refresh_attached_session(&refresh_session).await;
            }
        }

        response
    }

    pub(super) async fn handle_respawn_window(
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
        let (response, removed_pane_ids, subscriptions_removed, refresh_sessions) = {
            let mut state = self.state.lock().await;
            let subscription_keys = PaneOutputSubscriptionKeySnapshot::capture_related(
                &state,
                std::slice::from_ref(&session_name),
            );
            request.start_directory = match render_start_directory_template(
                &state,
                &Target::Window(target),
                attached_count,
                request.start_directory,
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
                    let subscriptions_removed = self.apply_pane_output_subscription_reconciliation(
                        subscription_keys.reconcile_after(&state),
                    );
                    (
                        Response::RespawnWindow(result.response),
                        result.removed_pane_ids,
                        subscriptions_removed,
                        result.refresh_sessions,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    false,
                    Vec::new(),
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
            for refresh_session in refresh_sessions {
                self.refresh_attached_session(&refresh_session).await;
            }
        }

        response
    }

    async fn resolve_resize_window_linked_session_size(
        &self,
        mut request: rmux_proto::ResizeWindowRequest,
    ) -> Result<rmux_proto::ResizeWindowRequest, rmux_proto::RmuxError> {
        use rmux_proto::ResizeWindowAdjustment::{LargestLinkedSession, SmallestLinkedSession};

        let largest = match request.adjustment {
            Some(LargestLinkedSession) => true,
            Some(SmallestLinkedSession) => false,
            _ => return Ok(request),
        };

        let (linked_sessions, fallback_size) = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session(request.target.session_name())
                .ok_or_else(|| {
                    crate::pane_terminals::session_not_found(request.target.session_name())
                })?;
            let _window = session
                .window_at(request.target.window_index())
                .ok_or_else(|| {
                    rmux_proto::RmuxError::invalid_target(
                        request.target.to_string(),
                        "window index does not exist in session",
                    )
                })?;
            (
                state.window_linked_sessions_list(
                    request.target.session_name(),
                    request.target.window_index(),
                ),
                session.terminal_size(),
            )
        };
        let linked_sessions = linked_sessions.into_iter().collect::<HashSet<_>>();
        let selected = {
            let active_attach = self.active_attach.lock().await;
            let sizes = active_attach
                .by_pid
                .values()
                .filter(|active| {
                    !active.suspended && linked_sessions.contains(&active.session_name)
                })
                .map(|active| active.client_size);
            if largest {
                sizes.max_by_key(resize_window_size_rank)
            } else {
                sizes.min_by_key(resize_window_size_rank)
            }
        }
        .unwrap_or(fallback_size);

        request.width = Some(selected.cols);
        request.height = Some(selected.rows);
        request.adjustment = None;
        Ok(request)
    }
}

fn resize_window_size_rank(size: &rmux_proto::TerminalSize) -> (u32, u16, u16) {
    (
        u32::from(size.cols) * u32::from(size.rows),
        size.cols,
        size.rows,
    )
}
