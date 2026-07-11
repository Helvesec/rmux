use std::collections::HashSet;

use rmux_core::LifecycleEvent;
use rmux_proto::{
    ErrorResponse, HookName, PaneTarget, Response, ScopeSelector, SessionName, Target,
};

#[cfg(windows)]
use super::pane_support::format_references_pane_pid;
use super::{
    client_environment_snapshot, client_spawn_environment,
    scripting_support::render_start_directory_template, RequestHandler,
};
use crate::hook_runtime::PendingInlineHookFormat;
use crate::pane_terminals::{
    HandlerState, NewWindowOptions, RemovedWindowHookContext, RespawnWindowOptions,
    WindowSpawnOptions,
};

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
        let process_command = request
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
        let response = {
            let mut state = self.state.lock().await;
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
                Ok(response) => Response::NewWindow(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::NewWindow(_)) {
            self.sync_session_silence_timers(&session_name).await;
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
                self.emit(LifecycleEvent::WindowLinked {
                    session_name: session_name.clone(),
                    target: Some(success.target.clone()),
                })
                .await;
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
        let (response, removed_windows, removed_pane_ids) = {
            let mut state = self.state.lock().await;
            match state.kill_window(request.target, request.kill_all_others) {
                Ok(result) => {
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    (
                        Response::KillWindow(result.response),
                        result.removed_windows,
                        result.removed_pane_ids,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    Vec::new(),
                ),
            }
        };

        if matches!(response, Response::KillWindow(_)) {
            self.forget_pane_snapshot_coalescers(&removed_pane_ids);
            let mut affected_sessions = removed_windows
                .iter()
                .map(|removed_window| removed_window.target.session_name().clone())
                .collect::<HashSet<_>>();
            let _ = affected_sessions.insert(session_name.clone());
            for affected_session in &affected_sessions {
                self.sync_session_silence_timers(affected_session).await;
            }
            {
                let mut active_attach = self.active_attach.lock().await;
                for removed_window in &removed_windows {
                    active_attach.forget_window(&removed_window.target);
                }
            }
            self.bump_active_attach_epoch();
            for removed_window in removed_windows {
                let removed_session_name = removed_window.target.session_name().clone();
                self.emit(LifecycleEvent::WindowUnlinked {
                    session_name: removed_session_name,
                    target: Some(removed_window.target),
                    window_id: Some(removed_window.window_id),
                    window_name: Some(removed_window.window_name),
                })
                .await;
            }
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
        let (response, window_changed, resize_sessions) = {
            let mut state = self.state.lock().await;
            let previous_window_index = state
                .sessions
                .session(&session_name)
                .map(|session| session.active_window_index());
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
                ),
                Err(error) => (Response::Error(ErrorResponse { error }), false, Vec::new()),
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
            if window_changed {
                self.emit(LifecycleEvent::SessionWindowChanged {
                    session_name: session_name.clone(),
                })
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
        let (response, resize_sessions) = {
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
                    (Response::NextWindow(response), resize_sessions)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new()),
            }
        };

        if matches!(response, Response::NextWindow(_)) {
            self.emit(LifecycleEvent::SessionWindowChanged {
                session_name: session_name.clone(),
            })
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
        let (response, resize_sessions) = {
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
                    (Response::PreviousWindow(response), resize_sessions)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new()),
            }
        };

        if matches!(response, Response::PreviousWindow(_)) {
            self.emit(LifecycleEvent::SessionWindowChanged {
                session_name: session_name.clone(),
            })
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
        let (response, resize_sessions) = {
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
                    (Response::LastWindow(response), resize_sessions)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new()),
            }
        };

        if matches!(response, Response::LastWindow(_)) {
            self.emit(LifecycleEvent::SessionWindowChanged {
                session_name: session_name.clone(),
            })
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
        let refresh_sessions =
            unique_sessions(request.source.session_name(), request.target.session_name());
        self.pause_before_window_lifecycle_mutation().await;
        let (response, removed_destination_pane_ids) = {
            let mut state = self.state.lock().await;
            match state.link_window(request.clone()) {
                Ok(result) => {
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    (
                        Response::LinkWindow(result.response),
                        result.removed_pane_ids,
                    )
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new()),
            }
        };

        if let Response::LinkWindow(success) = &response {
            self.forget_pane_snapshot_coalescers(&removed_destination_pane_ids);
            self.emit(LifecycleEvent::WindowLinked {
                session_name: success.target.session_name().clone(),
                target: Some(success.target.clone()),
            })
            .await;
            for session_name in refresh_sessions {
                self.sync_session_silence_timers(&session_name).await;
                self.refresh_attached_session(&session_name).await;
            }
        }

        response
    }

    pub(super) async fn handle_move_window(
        &self,
        request: rmux_proto::MoveWindowRequest,
    ) -> Response {
        let refresh_sessions = move_window_refresh_sessions(&request);
        self.pause_before_window_lifecycle_mutation().await;
        let (response, unlinked_window, removed_destination_pane_ids) = {
            let mut state = self.state.lock().await;
            match state.move_window(request.clone()) {
                Ok(result) => {
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    (
                        Response::MoveWindow(result.response),
                        result.unlinked_window,
                        result.removed_pane_ids,
                    )
                }
                Err(error) => (Response::Error(ErrorResponse { error }), None, Vec::new()),
            }
        };

        if matches!(response, Response::MoveWindow(_)) {
            self.forget_pane_snapshot_coalescers(&removed_destination_pane_ids);
            let lifecycle_events =
                move_window_lifecycle_events(&response, &request, unlinked_window.as_ref());
            for event in lifecycle_events {
                self.emit(event).await;
            }
            for session_name in refresh_sessions {
                self.sync_session_silence_timers(&session_name).await;
                self.refresh_attached_session(&session_name).await;
            }
        }

        response
    }

    pub(super) async fn handle_unlink_window(
        &self,
        request: rmux_proto::UnlinkWindowRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        self.pause_before_window_lifecycle_mutation().await;
        let (response, removed_window, removed_pane_ids) = {
            let mut state = self.state.lock().await;
            match state.unlink_window(request.target, request.kill_if_last) {
                Ok(result) => {
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    (
                        Response::UnlinkWindow(result.response),
                        Some(result.removed_window),
                        result.removed_pane_ids,
                    )
                }
                Err(error) => (Response::Error(ErrorResponse { error }), None, Vec::new()),
            }
        };

        if matches!(response, Response::UnlinkWindow(_)) {
            self.forget_pane_snapshot_coalescers(&removed_pane_ids);
            if let Some(removed_window) = removed_window {
                self.emit(LifecycleEvent::WindowUnlinked {
                    session_name: session_name.clone(),
                    target: Some(removed_window.target),
                    window_id: Some(removed_window.window_id),
                    window_name: Some(removed_window.window_name),
                })
                .await;
            }
            self.sync_session_silence_timers(&session_name).await;
            self.refresh_attached_session(&session_name).await;
        }

        response
    }

    pub(super) async fn handle_swap_window(
        &self,
        request: rmux_proto::SwapWindowRequest,
    ) -> Response {
        let refresh_sessions =
            unique_sessions(request.source.session_name(), request.target.session_name());
        let response = {
            let mut state = self.state.lock().await;
            match state.swap_window(request.source, request.target, request.detached) {
                Ok(response) => Response::SwapWindow(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::SwapWindow(_)) {
            for session_name in refresh_sessions {
                self.sync_session_silence_timers(&session_name).await;
                self.refresh_attached_session(&session_name).await;
            }
        }

        response
    }

    pub(super) async fn handle_rotate_window(
        &self,
        request: rmux_proto::RotateWindowRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let target = request.target;
        let response = {
            let mut state = self.state.lock().await;
            match state.rotate_window(target.clone(), request.direction, request.restore_zoom) {
                Ok(response) => Response::RotateWindow(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::RotateWindow(_)) {
            self.emit(LifecycleEvent::WindowLayoutChanged { target })
                .await;
            self.refresh_attached_session(&session_name).await;
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
        let (response, removed_pane_ids) = {
            let mut state = self.state.lock().await;
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
                        pane_alert_callback: Some(self.pane_alert_callback()),
                        pane_exit_callback: Some(self.pane_exit_callback()),
                    },
                },
            ) {
                Ok(result) => {
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    self.record_pane_respawn_boundary(result.retained_pane_id);
                    (
                        Response::RespawnWindow(result.response),
                        result.removed_pane_ids,
                    )
                }
                Err(error) => (Response::Error(ErrorResponse { error }), Vec::new()),
            }
        };

        if !removed_pane_ids.is_empty() {
            self.forget_pane_snapshot_coalescers(&removed_pane_ids);
        }
        if matches!(&response, Response::RespawnWindow(_)) {
            self.refresh_attached_session(&session_name).await;
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

fn move_window_refresh_sessions(
    request: &rmux_proto::MoveWindowRequest,
) -> Vec<rmux_proto::SessionName> {
    if request.renumber {
        return match &request.target {
            rmux_proto::MoveWindowTarget::Session(session_name) => vec![session_name.clone()],
            rmux_proto::MoveWindowTarget::Window(target) => vec![target.session_name().clone()],
        };
    }

    let Some(source) = &request.source else {
        return Vec::new();
    };
    let rmux_proto::MoveWindowTarget::Window(target) = &request.target else {
        return vec![source.session_name().clone()];
    };
    unique_sessions(source.session_name(), target.session_name())
}

fn move_window_lifecycle_events(
    response: &Response,
    request: &rmux_proto::MoveWindowRequest,
    unlinked_window: Option<&RemovedWindowHookContext>,
) -> Vec<LifecycleEvent> {
    if request.renumber {
        return Vec::new();
    }

    let Some(source) = &request.source else {
        return Vec::new();
    };

    let Response::MoveWindow(success) = response else {
        return Vec::new();
    };
    let destination_session = success.session_name.clone();
    let destination_window_index = success.target.as_ref().map(|target| target.window_index());
    if source.session_name() == &destination_session
        && Some(source.window_index()) == destination_window_index
    {
        return Vec::new();
    }

    vec![
        LifecycleEvent::WindowUnlinked {
            session_name: source.session_name().clone(),
            target: unlinked_window.as_ref().map(|window| window.target.clone()),
            window_id: unlinked_window.map(|window| window.window_id),
            window_name: unlinked_window.map(|window| window.window_name.clone()),
        },
        LifecycleEvent::WindowLinked {
            session_name: destination_session.clone(),
            target: success.target.clone(),
        },
    ]
}

fn unique_sessions(
    source_session: &rmux_proto::SessionName,
    target_session: &rmux_proto::SessionName,
) -> Vec<rmux_proto::SessionName> {
    if source_session == target_session {
        vec![source_session.clone()]
    } else {
        vec![source_session.clone(), target_session.clone()]
    }
}
