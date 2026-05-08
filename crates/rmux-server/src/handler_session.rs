use std::path::PathBuf;
use std::sync::atomic::Ordering;

use rmux_core::{
    formats::{is_truthy, render_list_sessions_line, FormatContext},
    LifecycleEvent, WINDOW_ALERTFLAGS,
};
use rmux_proto::request::NewSessionExtRequest;
use rmux_proto::{
    CommandOutput, ErrorResponse, HasSessionResponse, HookName, KillSessionResponse,
    ListSessionsResponse, NewSessionResponse, OptionName, Response, RmuxError, ScopeSelector,
};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::hook_runtime::PendingInlineHookFormat;

#[path = "handler_session/list.rs"]
mod list;

use list::{sort_list_sessions, ListSessionSnapshot};

use super::{
    active_session_target, client_environment_snapshot, command_output_from_lines,
    option_value_u32, parse_session_sort_order, prepare_lifecycle_event,
    resolve_existing_session_target, resolve_session_lookup, update_environment_from_client,
    RequestHandler, SessionLookup, SessionSortOrder, DEFAULT_SESSION_SIZE,
};

impl RequestHandler {
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

        if request.attach_if_exists && request.group_target.is_none() {
            if let Some(existing) = request.session_name.as_ref() {
                let session_exists = {
                    let state = self.state.lock().await;
                    state.sessions.contains_session(existing)
                };
                if session_exists {
                    let session_name = existing.clone();
                    if let Some(client_environment) = client_environment_snapshot(requester_pid) {
                        let mut state = self.state.lock().await;
                        update_environment_from_client(
                            &mut state,
                            &session_name,
                            &client_environment,
                        );
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
                        detached: request.detached,
                        output: None,
                    });
                }
            }
        }

        let size = request.size.unwrap_or(DEFAULT_SESSION_SIZE);
        let detached = request.detached;
        let environment_overrides = request.environment;
        let group_target = request.group_target;
        let working_directory = request.working_directory;
        let command = request.command;
        let requested_name = request.session_name;
        let socket_path = self.socket_path();
        let client_environment = client_environment_snapshot(requester_pid);
        let response = {
            let mut state = self.state.lock().await;
            let base_index =
                option_value_u32(&state.options, None, rmux_proto::OptionName::BaseIndex);
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

            if let Some(client_environment) = client_environment.as_ref() {
                update_environment_from_client(&mut state, &session_name, client_environment);
            }

            if let Some(template) = working_directory.as_deref() {
                let rendered = {
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
                .is_none_or(|created| created.template_session.is_none());
            if needs_terminal {
                match state.insert_initial_session_terminal(
                    &session_name,
                    &socket_path,
                    environment_overrides.as_deref(),
                    command.as_deref(),
                    Some(self.pane_alert_callback()),
                    Some(self.pane_exit_callback()),
                ) {
                    Ok(()) => {}
                    Err(error) => {
                        let _removed = state.sessions.remove_session(&session_name);
                        return Response::Error(ErrorResponse { error });
                    }
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
        self.sync_session_silence_timers(&session_name).await;
        let current_target = {
            let state = self.state.lock().await;
            active_session_target(&state.sessions, &session_name)
        };
        self.queue_inline_hook(
            HookName::AfterNewSession,
            ScopeSelector::Session(session_name.clone()),
            current_target,
            PendingInlineHookFormat::AfterCommand,
        );
        self.emit(LifecycleEvent::SessionCreated {
            session_name: session_name.clone(),
        })
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

    pub(in crate::handler) async fn handle_has_session(
        &self,
        request: rmux_proto::HasSessionRequest,
    ) -> Response {
        let state = self.state.lock().await;
        let exists = match resolve_session_lookup(&state.sessions, "has-session", &request.target) {
            Ok(SessionLookup::Found(_)) => true,
            Ok(SessionLookup::Missing) => false,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };

        Response::HasSession(HasSessionResponse { exists })
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
            self.detach_attached_session(session_name).await;
            self.cancel_session_silence_timers(session_name).await;
        }

        let (response, queued_session_closed) = {
            let mut state = self.state.lock().await;
            let mut queued_events = Vec::new();

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
            )
        };

        for event in queued_session_closed {
            self.emit_prepared(event);
        }

        let _ = self.request_shutdown_if_server_empty().await;

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
                let context =
                    FormatContext::from_session(session).with_session_attached(attached_count);
                let mut runtime = RuntimeFormatContext::new(context)
                    .with_state(&state)
                    .with_session(session);
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

    async fn render_new_session_output(
        &self,
        session_name: &rmux_proto::SessionName,
        template: Option<&str>,
    ) -> Result<CommandOutput, RmuxError> {
        const NEW_SESSION_TEMPLATE: &str = "#{session_name}:";

        let attached_count = self.attached_count(session_name).await;
        let state = self.state.lock().await;
        let session = state
            .sessions
            .session(session_name)
            .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?;
        let context = FormatContext::from_session(session).with_session_attached(attached_count);
        let mut runtime = RuntimeFormatContext::new(context)
            .with_state(&state)
            .with_session(session);
        if attached_count == 0 {
            runtime = runtime.with_unclipped_geometry();
        }
        let expanded =
            render_runtime_template(template.unwrap_or(NEW_SESSION_TEMPLATE), &runtime, false);
        Ok(CommandOutput::from_stdout(
            format!("{expanded}\n").into_bytes(),
        ))
    }

    pub(in crate::handler) async fn request_shutdown_if_server_empty(&self) -> bool {
        let should_shutdown = {
            let state = self.state.lock().await;
            state.sessions.is_empty()
                && matches!(
                    state.options.resolve(None, OptionName::ExitEmpty),
                    Some("on")
                )
        };
        if !should_shutdown {
            return false;
        }

        self.shutdown_requested.store(true, Ordering::SeqCst);
        self.request_shutdown_if_pending()
    }
}
