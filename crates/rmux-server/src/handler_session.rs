use std::path::PathBuf;

use rmux_core::{
    formats::{is_truthy, render_list_sessions_line, FormatContext},
    LifecycleEvent, PaneId, WINDOW_ALERTFLAGS,
};
use rmux_proto::request::NewSessionExtRequest;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    ErrorResponse, KillSessionRequest, KillSessionResponse, ListSessionsResponse,
    NewSessionResponse, OptionName, Response, RmuxError, ScopeSelector, SessionId, SessionName,
    WindowTarget,
};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::InitialPaneSpawnOptions;
use crate::terminal::{parse_environment_assignments, validate_process_command};

#[path = "handler_session/client_environment.rs"]
mod client_environment;
#[path = "handler_session/control_mode.rs"]
mod control_mode;
#[path = "handler_session/has.rs"]
mod has;
#[path = "handler_session/list.rs"]
mod list;
#[path = "handler_session/options.rs"]
mod options;
#[path = "handler_session/output.rs"]
mod output;

use client_environment::{
    new_session_client_environment, new_session_raw_client_environment,
    raw_environment_from_assignments,
};
use list::{sort_list_sessions, ListSessionSnapshot};
use options::resolve_session_creation_options;

use super::scripting_support::format_context_for_target;
use super::target_support::{pane_id_target, requester_environment_pane_id};
use super::{
    command_output_from_lines, initial_session_spawn_environment, parse_session_sort_order,
    prepare_lifecycle_event, resolve_existing_session_target, update_environment_from_client,
    PendingShutdownReason, RequestHandler, SessionSortOrder,
};

impl RequestHandler {
    pub(in crate::handler) async fn destroy_unattached_sessions_for_option_scope(
        &self,
        scope: &OptionScopeSelector,
    ) {
        let mut candidates = {
            let state = self.state.lock().await;
            destroy_unattached_candidate_sessions(&state, scope)
        };
        self.destroy_unattached_sessions(std::mem::take(&mut candidates))
            .await;
    }

    pub(in crate::handler) async fn destroy_unattached_sessions(
        &self,
        mut candidates: Vec<SessionName>,
    ) {
        candidates.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        candidates.dedup();

        for session_name in candidates {
            if !self
                .session_should_destroy_when_unattached(&session_name)
                .await
            {
                continue;
            }
            if self.attached_count(&session_name).await != 0 {
                continue;
            }
            let _ = self
                .handle_kill_session(KillSessionRequest {
                    target: session_name,
                    kill_all_except_target: false,
                    clear_alerts: false,
                })
                .await;
        }
    }

    async fn session_should_destroy_when_unattached(&self, session_name: &SessionName) -> bool {
        let state = self.state.lock().await;
        state.sessions.contains_session(session_name)
            && state
                .options
                .resolve(Some(session_name), OptionName::DestroyUnattached)
                == Some("on")
    }

    pub(in crate::handler) async fn handle_new_session(
        &self,
        requester_pid: u32,
        request: rmux_proto::NewSessionRequest,
    ) -> Response {
        self.handle_new_session_ext(
            requester_pid,
            NewSessionExtRequest {
                session_name: Some(request.session_name),
                working_directory: None,
                detached: request.detached,
                size: request.size,
                environment: request.environment,
                group_target: None,
                attach_if_exists: false,
                detach_other_clients: false,
                kill_other_clients: false,
                flags: None,
                window_name: None,
                print_session_info: false,
                print_format: None,
                command: None,
                process_command: None,
                client_environment: None,
                skip_environment_update: false,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_new_session_ext(
        &self,
        requester_pid: u32,
        request: NewSessionExtRequest,
    ) -> Response {
        if request.group_target.is_some()
            && (request.window_name.is_some() || request.command.is_some())
        {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server("command or window name given with target".to_owned()),
            });
        }
        let client_environment = match new_session_client_environment(
            requester_pid,
            request.client_environment.as_deref(),
        ) {
            Ok(environment) => environment,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        let spawn_environment = initial_session_spawn_environment(client_environment.as_ref());
        let raw_spawn_environment = if request.client_environment.is_some() {
            client_environment
                .as_ref()
                .map(raw_environment_from_assignments)
        } else {
            new_session_raw_client_environment(requester_pid)
        };

        if request.attach_if_exists && request.group_target.is_none() {
            if let Some(existing) = request.session_name.as_ref() {
                let session_exists = {
                    let state = self.state.lock().await;
                    state.sessions.contains_session(existing)
                };
                if session_exists {
                    let session_name = existing.clone();
                    if !request.skip_environment_update {
                        let mut state = self.state.lock().await;
                        if let Some(client_environment) = client_environment.as_ref() {
                            update_environment_from_client(
                                &mut state,
                                &session_name,
                                client_environment,
                            );
                        }
                    }
                    if !request.detached
                        && (request.detach_other_clients || request.kill_other_clients)
                    {
                        self.detach_other_attach_clients_for_session(
                            &session_name,
                            requester_pid,
                            request.kill_other_clients,
                        )
                        .await;
                    }
                    return Response::NewSession(NewSessionResponse {
                        session_name,
                        detached: false,
                        output: None,
                    });
                }
            }
        }

        let requested_size = request.size;
        let detached = request.detached;
        let environment_overrides = request.environment;
        let environment_assignments = match environment_overrides.as_deref() {
            Some(overrides) => match parse_environment_assignments(overrides) {
                Ok(assignments) => Some(assignments),
                Err(error) => return Response::Error(ErrorResponse { error }),
            },
            None => None,
        };
        let group_target = request.group_target;
        let working_directory = request.working_directory;
        let requester_cwd_pane_id = if working_directory
            .as_ref()
            .is_some_and(|path| path.contains("#{"))
        {
            let socket_path = self.socket_path();
            requester_environment_pane_id(requester_pid, &socket_path)
        } else {
            None
        };
        let requester_cwd_target = match requester_cwd_pane_id {
            Some(pane_id) => {
                let state = self.state.lock().await;
                pane_id_target(&state.sessions, pane_id)
            }
            None => None,
        };
        let requester_cwd_attached_count = match requester_cwd_target.as_ref() {
            Some(target) => self.attached_count(target.session_name()).await,
            None => 0,
        };
        let working_directory_uses_requester_context = requester_cwd_target.is_some();
        let command = request.command;
        let requested_process_command = request
            .process_command
            .or_else(|| crate::legacy_command::from_legacy_command(command.as_deref()));
        let requested_name = request.session_name;
        let socket_path = self.socket_path();
        let response = {
            let mut state = self.state.lock().await;
            let creation_options = resolve_session_creation_options(
                &state.options,
                requested_size,
                requested_process_command,
            );
            if let Err(error) = validate_process_command(creation_options.process_command.as_ref())
            {
                return Response::Error(ErrorResponse { error });
            }
            let size = creation_options.size;
            let base_index = creation_options.base_index;
            let process_command = creation_options.process_command;
            let working_directory = match (requester_cwd_target.as_ref(), working_directory) {
                (Some(target), Some(template)) => {
                    let context = match format_context_for_target(
                        &state,
                        target,
                        requester_cwd_attached_count,
                    ) {
                        Ok(context) => context,
                        Err(error) => return Response::Error(ErrorResponse { error }),
                    };
                    Some(render_runtime_template(&template, &context, false))
                }
                (_, working_directory) => working_directory,
            };
            let (session_name, created_group) = match (requested_name.clone(), group_target.clone())
            {
                (Some(session_name), Some(group_target)) => {
                    let created_group = match state.sessions.create_grouped_session_with_base_index(
                        session_name.clone(),
                        size,
                        base_index,
                        group_target,
                    ) {
                        Ok(created) => created,
                        Err(error) => return Response::Error(ErrorResponse { error }),
                    };
                    (session_name, Some(created_group))
                }
                (Some(session_name), None) => {
                    if let Err(error) = state.sessions.create_session_with_base_index(
                        session_name.clone(),
                        size,
                        base_index,
                    ) {
                        return Response::Error(ErrorResponse { error });
                    }
                    (session_name, None)
                }
                (None, Some(group_target)) => {
                    let created_group = match state
                        .sessions
                        .create_auto_grouped_session_with_base_index(size, base_index, group_target)
                    {
                        Ok(created) => created,
                        Err(error) => return Response::Error(ErrorResponse { error }),
                    };
                    (created_group.session_name.clone(), Some(created_group))
                }
                (None, None) => {
                    let session_name = match state
                        .sessions
                        .create_auto_named_session_with_base_index(size, base_index)
                    {
                        Ok(session_name) => session_name,
                        Err(error) => return Response::Error(ErrorResponse { error }),
                    };
                    (session_name, None)
                }
            };

            if let Some(window_name) = request.window_name.as_ref() {
                let active_window = state
                    .sessions
                    .session(&session_name)
                    .map(|session| session.active_window_index())
                    .expect("newly created session must exist");
                if let Some(session) = state.sessions.session_mut(&session_name) {
                    session
                        .rename_window(active_window, window_name.clone())
                        .expect("newly created session must accept an initial window name");
                }
            }

            if !request.skip_environment_update {
                if let Some(client_environment) = client_environment.as_ref() {
                    update_environment_from_client(&mut state, &session_name, client_environment);
                }
            }
            if let Some(environment_assignments) = environment_assignments.as_ref() {
                for (name, value) in environment_assignments {
                    state.environment.set(
                        ScopeSelector::Session(session_name.clone()),
                        name.clone(),
                        value.clone(),
                    );
                }
            }

            if let Some(template) = working_directory.as_deref() {
                let rendered = if working_directory_uses_requester_context {
                    template.to_owned()
                } else {
                    let session = state
                        .sessions
                        .session(&session_name)
                        .expect("newly created session must exist before cwd assignment");
                    let context = RuntimeFormatContext::new(FormatContext::from_session(session))
                        .with_state(&state)
                        .with_session(session);
                    render_runtime_template(template, &context, false)
                };
                let session = state
                    .sessions
                    .session_mut(&session_name)
                    .expect("newly created session must accept cwd assignment");
                session.set_cwd((!rendered.is_empty()).then(|| PathBuf::from(rendered)));
            }

            let needs_terminal = created_group
                .as_ref()
                .map(|created| created.template_session.is_none())
                .unwrap_or(true);
            if needs_terminal {
                match state.insert_initial_session_terminal(
                    &session_name,
                    InitialPaneSpawnOptions {
                        socket_path: &socket_path,
                        spawn_environment: spawn_environment.as_ref(),
                        raw_spawn_environment: raw_spawn_environment.as_deref(),
                        environment_overrides: environment_overrides.as_deref(),
                        command: process_command.as_ref(),
                        pane_alert_callback: Some(self.pane_alert_callback()),
                        pane_exit_callback: Some(self.pane_exit_callback()),
                    },
                ) {
                    Ok(()) => {}
                    Err(error) => {
                        let _removed = state.sessions.remove_session(&session_name);
                        return Response::Error(ErrorResponse { error });
                    }
                }
            }
            if request.window_name.is_some() {
                let active_window = state
                    .sessions
                    .session(&session_name)
                    .map(|session| session.active_window_index())
                    .expect("newly created session must still exist");
                let target = WindowTarget::with_window(session_name.clone(), active_window);
                if let Err(error) = state.disable_automatic_rename_for_window(&target) {
                    return Response::Error(ErrorResponse { error });
                }
            }

            Response::NewSession(NewSessionResponse {
                session_name,
                detached,
                output: None,
            })
        };

        let Response::NewSession(success) = &response else {
            return response;
        };
        let session_name = success.session_name.clone();
        if !detached && (request.detach_other_clients || request.kill_other_clients) {
            self.detach_other_attach_clients_for_session(
                &session_name,
                requester_pid,
                request.kill_other_clients,
            )
            .await;
        }
        self.finish_new_session_lifecycle(requester_pid, &session_name, detached)
            .await;

        if !request.print_session_info {
            return response;
        }

        match self
            .render_new_session_output(&session_name, request.print_format.as_deref())
            .await
        {
            Ok(output) => Response::NewSession(NewSessionResponse {
                session_name,
                detached,
                output: Some(output),
            }),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(in crate::handler) async fn handle_kill_session(
        &self,
        request: rmux_proto::KillSessionRequest,
    ) -> Response {
        let session_name = {
            let state = self.state.lock().await;
            match resolve_existing_session_target(&state.sessions, "kill-session", &request.target)
            {
                Ok(session_name) => session_name,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };

        if request.clear_alerts {
            let response = {
                let mut state = self.state.lock().await;
                let Some(session) = state.sessions.session_mut(&session_name) else {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::SessionNotFound(session_name.to_string()),
                    });
                };
                let window_indexes = session.windows().keys().copied().collect::<Vec<_>>();
                for window_index in window_indexes {
                    if let Some(window) = session.window_at_mut(window_index) {
                        window.clear_alert_flags(WINDOW_ALERTFLAGS);
                    }
                    let _ = session.clear_all_winlink_alert_flags(window_index);
                }
                Response::KillSession(KillSessionResponse { existed: true })
            };
            self.refresh_attached_session(&session_name).await;
            self.refresh_control_session(&session_name).await;
            return response;
        }

        let sessions_to_remove = {
            let state = self.state.lock().await;
            if !state.sessions.contains_session(&session_name) {
                return Response::Error(ErrorResponse {
                    error: RmuxError::SessionNotFound(session_name.to_string()),
                });
            }

            if request.kill_all_except_target {
                let mut sessions = state
                    .sessions
                    .iter()
                    .map(|(name, _)| name.clone())
                    .filter(|name| name != &session_name)
                    .collect::<Vec<_>>();
                sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
                sessions
            } else {
                vec![session_name.clone()]
            }
        };

        for session_name in &sessions_to_remove {
            self.exit_attached_session(session_name).await;
            self.cancel_session_silence_timers(session_name).await;
        }

        let (response, queued_session_closed, removed_pane_ids, removed_sessions) = {
            let mut state = self.state.lock().await;
            let mut queued_events = Vec::new();
            let mut removed_pane_ids = Vec::new();
            let mut removed_sessions: Vec<(SessionName, SessionId)> = Vec::new();

            for session_name in &sessions_to_remove {
                if !state.sessions.contains_session(session_name) {
                    continue;
                }
                let current_runtime_owner = state.sessions.runtime_owner(session_name);
                if current_runtime_owner.as_ref() == Some(session_name)
                    && !state.contains_session_terminals(session_name)
                {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::Server(format!(
                            "missing pane terminals for session {}",
                            session_name
                        )),
                    });
                }
            }

            for session_name in &sessions_to_remove {
                let current_runtime_owner = state.sessions.runtime_owner(session_name);
                let next_runtime_owner = state.sessions.runtime_owner_transfer_target(session_name);

                match state.sessions.remove_session(session_name) {
                    Ok(removed_session) => {
                        removed_pane_ids.extend(session_pane_ids(&removed_session));
                        removed_sessions.push((session_name.clone(), removed_session.id()));
                        queued_events.push(prepare_lifecycle_event(
                            &mut state,
                            &LifecycleEvent::SessionClosed {
                                session_name: session_name.clone(),
                                session_id: Some(removed_session.id().as_u32()),
                            },
                        ));
                        let _ = state.options.remove_session(session_name);
                        let _ = state.environment.remove_session(session_name);
                        let _ = state.hooks.remove_session(session_name);
                        if let Err(error) = state.remove_session_terminals(
                            session_name,
                            current_runtime_owner.as_ref(),
                            next_runtime_owner.as_ref(),
                        ) {
                            return Response::Error(ErrorResponse { error });
                        }
                    }
                    Err(RmuxError::SessionNotFound(_)) => {}
                    Err(error) => {
                        return Response::Error(ErrorResponse { error });
                    }
                }
            }

            (
                Response::KillSession(KillSessionResponse { existed: true }),
                queued_events,
                removed_pane_ids,
                removed_sessions,
            )
        };

        #[cfg(all(any(unix, windows), feature = "web"))]
        self.web_shares
            .remove_targets_for_sessions(&removed_sessions);
        #[cfg(not(all(any(unix, windows), feature = "web")))]
        let _ = &removed_sessions;
        if !removed_pane_ids.is_empty() {
            self.forget_pane_snapshot_coalescers(&removed_pane_ids);
        }
        for event in queued_session_closed {
            self.emit_prepared(event);
        }
        self.remove_session_leases(&sessions_to_remove);

        let _ = self.queue_shutdown_if_server_empty().await;

        response
    }

    pub(in crate::handler) async fn handle_rename_session(
        &self,
        request: rmux_proto::RenameSessionRequest,
    ) -> Response {
        let session_name = {
            let state = self.state.lock().await;
            match resolve_existing_session_target(
                &state.sessions,
                "rename-session",
                &request.target,
            ) {
                Ok(session_name) => session_name,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };
        let new_name = request.new_name;
        if session_name == new_name {
            return Response::RenameSession(rmux_proto::RenameSessionResponse { session_name });
        }
        let mut renamed = false;
        let response = {
            let mut state = self.state.lock().await;
            if state.sessions.contains_session(&new_name) {
                return Response::Error(ErrorResponse {
                    error: RmuxError::DuplicateSession(new_name.to_string()),
                });
            }

            match state.rename_session(&session_name, &new_name) {
                Ok(()) => {
                    let mut active_attach = self.active_attach.lock().await;
                    active_attach.rename_session(&session_name, &new_name);
                    drop(active_attach);
                    renamed = true;
                    Response::RenameSession(rmux_proto::RenameSessionResponse {
                        session_name: new_name.clone(),
                    })
                }
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if renamed {
            self.rename_control_session(&session_name, &new_name).await;
            self.cancel_session_silence_timers(&session_name).await;
        }
        if matches!(response, Response::RenameSession(_)) {
            self.sync_session_silence_timers(&new_name).await;
            self.emit(LifecycleEvent::SessionRenamed {
                session_name: new_name.clone(),
            })
            .await;
            self.refresh_attached_session(&new_name).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_list_sessions(
        &self,
        request: rmux_proto::ListSessionsRequest,
    ) -> Response {
        let state = self.state.lock().await;
        let sort_order = match parse_session_sort_order(request.sort_order.as_deref()) {
            Some(sort_order) => sort_order,
            None if request.sort_order.is_some() => {
                let value = request.sort_order.unwrap_or_default();
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(format!("invalid sort order: {value}")),
                });
            }
            None => SessionSortOrder::Name,
        };
        let mut sessions = state
            .sessions
            .iter()
            .map(|(session_name, session)| ListSessionSnapshot {
                name: session_name.clone(),
                id: session.id().as_u32(),
                created_at: session.created_at(),
                activity_at: session.activity_at(),
            })
            .collect::<Vec<_>>();
        sort_list_sessions(&mut sessions, sort_order, request.reversed);

        let active_attach = self.active_attach.lock().await;
        let active_control = self.active_control.lock().await;
        let lines = sessions
            .iter()
            .filter_map(|session| state.sessions.session(&session.name))
            .filter_map(|session| {
                let attached_count = active_attach.attached_count(session.name())
                    + active_control.attached_count(session.name());
                let active_window_index = session.active_window_index();
                let active_window = session.window();
                let mut context = FormatContext::from_session(session)
                    .with_session_attached(attached_count)
                    .with_window(active_window_index, active_window, true, false);
                if let Some(pane) = active_window.active_pane() {
                    context = context.with_window_pane(active_window, pane);
                }
                let mut runtime = RuntimeFormatContext::new(context)
                    .with_state(&state)
                    .with_session(session)
                    .with_window(active_window_index, active_window);
                if let Some(pane) = active_window.active_pane() {
                    runtime = runtime.with_pane(pane);
                }
                if attached_count == 0 {
                    runtime = runtime.with_unclipped_geometry();
                }
                if let Some(filter) = request.filter.as_deref() {
                    let expanded = render_runtime_template(filter, &runtime, false);
                    if !is_truthy(&expanded) {
                        return None;
                    }
                }

                Some(render_list_sessions_line(
                    &runtime,
                    request.format.as_deref(),
                ))
            })
            .collect::<Vec<_>>();

        Response::ListSessions(ListSessionsResponse {
            output: command_output_from_lines(&lines),
        })
    }

    pub(in crate::handler) async fn request_shutdown_if_server_empty(&self) -> bool {
        if !self.queue_shutdown_if_server_empty().await {
            return false;
        }

        self.request_shutdown_if_pending()
    }

    pub(in crate::handler) async fn queue_shutdown_if_server_empty(&self) -> bool {
        let should_shutdown = {
            let state = self.state.lock().await;
            state.sessions.is_empty()
                && matches!(
                    state.options.resolve(None, OptionName::ExitEmpty),
                    Some("on")
                )
        };
        if should_shutdown {
            self.queue_shutdown_request(PendingShutdownReason::ExitEmpty);
        }
        should_shutdown
    }
}

fn destroy_unattached_candidate_sessions(
    state: &crate::pane_terminals::HandlerState,
    scope: &OptionScopeSelector,
) -> Vec<SessionName> {
    match scope {
        OptionScopeSelector::ServerGlobal
        | OptionScopeSelector::SessionGlobal
        | OptionScopeSelector::WindowGlobal => state
            .sessions
            .iter()
            .map(|(session_name, _)| session_name.clone())
            .collect(),
        OptionScopeSelector::Session(session_name) => vec![session_name.clone()],
        OptionScopeSelector::Window(target) => vec![target.session_name().clone()],
        OptionScopeSelector::Pane(target) => vec![target.session_name().clone()],
    }
}

fn session_pane_ids(session: &rmux_core::Session) -> Vec<PaneId> {
    session
        .windows()
        .values()
        .flat_map(|window| window.panes().iter().map(|pane| pane.id()))
        .collect()
}
