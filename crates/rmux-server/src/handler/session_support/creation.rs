use std::path::PathBuf;

use rmux_core::formats::FormatContext;
use rmux_proto::request::NewSessionExtRequest;
use rmux_proto::{
    ErrorResponse, HookName, NewSessionResponse, Response, RmuxError, ScopeSelector, WindowTarget,
};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::hook_runtime::PendingInlineHookFormat;
use crate::pane_terminals::InitialPaneSpawnOptions;
use crate::terminal::{parse_environment_assignments, validate_process_command};

#[cfg(windows)]
use super::super::pane_support::format_references_pane_pid;
use super::super::scripting_support::format_context_for_target;
use super::super::target_support::{inherited_pane_target, requester_environment_context};
use super::super::{
    initial_session_spawn_environment, update_environment_from_client, RequestHandler,
};
use super::client_environment::{
    new_session_client_environment, new_session_raw_client_environment,
    raw_environment_from_assignments,
};
use super::options::resolve_session_creation_options;

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
        // `-A` on an existing session attaches even with `-d`, as in tmux.
        let attached_requester = if !request.detached || request.attach_if_exists {
            match self.new_session_attached_requester(requester_pid).await {
                Ok(client) => client,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        } else {
            None
        };
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
                let existing_session_id = {
                    let state = self.state.lock().await;
                    state.sessions.session(existing).map(|session| session.id())
                };
                if let Some(session_id) = existing_session_id {
                    let mut session_name = existing.clone();
                    self.pause_before_created_session_control_attach(&session_name)
                        .await;
                    if !request.skip_environment_update {
                        let mut state = self.state.lock().await;
                        let Some(current_name) = state
                            .sessions
                            .iter()
                            .map(|session| (session.name(), session))
                            .find_map(|(name, session)| {
                                (session.id() == session_id).then(|| name.clone())
                            })
                        else {
                            return Response::Error(ErrorResponse {
                                error: crate::pane_terminals::session_not_found(&session_name),
                            });
                        };
                        if let Some(client_environment) = client_environment.as_ref() {
                            update_environment_from_client(
                                &mut state,
                                &current_name,
                                client_environment,
                            );
                        }
                        session_name = current_name;
                    }
                    if !request.detached
                        && (request.detach_other_clients || request.kill_other_clients)
                    {
                        if let Err(error) = self
                            .detach_other_attach_clients_for_session_identity(
                                &session_name,
                                session_id,
                                requester_pid,
                                request.kill_other_clients,
                            )
                            .await
                        {
                            return Response::Error(ErrorResponse { error });
                        }
                    }
                    self.prepare_existing_session_control_attach(
                        requester_pid,
                        &session_name,
                        session_id,
                    )
                    .await;
                    if let Some(client) = attached_requester {
                        if let Err(error) = self
                            .switch_attached_requester_to_new_session(
                                requester_pid,
                                client,
                                &session_name,
                                session_id,
                            )
                            .await
                        {
                            return Response::Error(ErrorResponse { error });
                        }
                    }
                    self.queue_suppressed_inline_hook(
                        HookName::AfterNewSession,
                        PendingInlineHookFormat::AfterCommand,
                    );
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
        #[cfg(windows)]
        if working_directory
            .as_ref()
            .is_some_and(|path| format_references_pane_pid(Some(path.as_str())))
        {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
        let requester_cwd_target = if working_directory
            .as_ref()
            .is_some_and(|path| path.contains("#{"))
        {
            let socket_path = self.socket_path();
            let requester_environment = requester_environment_context(requester_pid, &socket_path);
            let state = self.state.lock().await;
            match inherited_pane_target(&state.sessions, requester_environment, None) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        } else {
            None
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
        #[cfg(windows)]
        let mut deferred_initial_spawn = None;
        let (response, silence_template_session, created_session_id) = {
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

            if created_group
                .as_ref()
                .is_some_and(|created| created.template_session.is_some())
            {
                state
                    .sessions
                    .select_initial_grouped_window(&session_name)
                    .expect("new group peer must accept its initial selection");
            }

            if let Some(window_name) = request.window_name.as_ref() {
                let active_window = state
                    .sessions
                    .session(&session_name)
                    .map(|session| session.active_window_index())
                    .expect("newly created session must exist");
                let _ = state
                    .sessions
                    .rename_window(
                        &WindowTarget::with_window(session_name.clone(), active_window),
                        window_name.clone(),
                    )
                    .expect("newly created session must accept an initial window name");
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
                    let context = RuntimeFormatContext::new(
                        FormatContext::new().with_managed_session(session),
                    )
                    .with_state(&state)
                    .with_session(session);
                    render_runtime_template(template, &context, false)
                };
                state
                    .sessions
                    .set_cwd(
                        &session_name,
                        (!rendered.is_empty()).then(|| PathBuf::from(rendered)),
                    )
                    .expect("newly created session must accept cwd assignment");
            }

            if let Some(template_session) = created_group
                .as_ref()
                .and_then(|created| created.template_session.as_ref())
                .cloned()
            {
                let _ = state.synchronize_window_alias_options_from_name(&template_session);
                if let Err(error) =
                    state.synchronize_pane_alias_options_from_name(&template_session)
                {
                    return Response::Error(ErrorResponse { error });
                }
            }

            let needs_terminal = created_group
                .as_ref()
                .map(|created| created.template_session.is_none())
                .unwrap_or(true);
            if needs_terminal {
                let defer_initial_terminal = should_defer_windows_initial_pane(
                    detached,
                    request.print_session_info,
                    created_group.is_some(),
                    process_command.is_some(),
                );
                let spawn_options = InitialPaneSpawnOptions {
                    socket_path: &socket_path,
                    spawn_environment: spawn_environment.as_ref(),
                    raw_spawn_environment: raw_spawn_environment.as_deref(),
                    environment_overrides: environment_overrides.as_deref(),
                    command: process_command.as_ref(),
                    pane_alert_callback: Some(self.pane_alert_callback()),
                    pane_exit_callback: Some(self.pane_exit_callback()),
                };
                if defer_initial_terminal {
                    #[cfg(windows)]
                    {
                        match state
                            .prepare_deferred_initial_session_terminal(&session_name, spawn_options)
                        {
                            Ok(spawn) => {
                                deferred_initial_spawn = Some(spawn);
                            }
                            Err(error) => {
                                let _removed = state.sessions.remove_session(&session_name);
                                let _ = state.environment.remove_session(&session_name);
                                return Response::Error(ErrorResponse { error });
                            }
                        }
                    }
                } else {
                    match state.insert_initial_session_terminal(&session_name, spawn_options) {
                        Ok(()) => {}
                        Err(error) => {
                            let _removed = state.sessions.remove_session(&session_name);
                            let _ = state.environment.remove_session(&session_name);
                            return Response::Error(ErrorResponse { error });
                        }
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

            let created_session_id = state
                .sessions
                .session(&session_name)
                .expect("newly created session must still exist")
                .id();
            let silence_template_session =
                created_group.and_then(|created| created.template_session);
            (
                Response::NewSession(NewSessionResponse {
                    session_name,
                    detached,
                    output: None,
                }),
                silence_template_session,
                created_session_id,
            )
        };

        let Response::NewSession(success) = &response else {
            return response;
        };
        let session_name = success.session_name.clone();
        #[cfg(windows)]
        if let Some(spawn) = deferred_initial_spawn {
            self.spawn_deferred_initial_pane(spawn);
        }
        if !detached {
            self.pause_before_created_session_control_attach(&session_name)
                .await;
        }
        if !detached && (request.detach_other_clients || request.kill_other_clients) {
            if let Err(error) = self
                .detach_other_attach_clients_for_session_identity(
                    &session_name,
                    created_session_id,
                    requester_pid,
                    request.kill_other_clients,
                )
                .await
            {
                return Response::Error(ErrorResponse { error });
            }
        }
        super::super::scripting_support::record_queued_new_session_transition(
            created_session_id,
            session_name.clone(),
        );
        self.finish_new_session_lifecycle(
            requester_pid,
            &session_name,
            created_session_id,
            silence_template_session.as_ref(),
            detached,
        )
        .await;
        if let Some(client) = attached_requester.filter(|_| !detached) {
            if let Err(error) = self
                .switch_attached_requester_to_new_session(
                    requester_pid,
                    client,
                    &session_name,
                    created_session_id,
                )
                .await
            {
                return Response::Error(ErrorResponse { error });
            }
        }

        if !request.print_session_info {
            return response;
        }

        match self
            .render_new_session_output(created_session_id, request.print_format.as_deref())
            .await
        {
            Ok((current_session_name, output)) => Response::NewSession(NewSessionResponse {
                session_name: current_session_name,
                detached,
                output: Some(output),
            }),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }
}

fn should_defer_windows_initial_pane(
    detached: bool,
    print_session_info: bool,
    grouped: bool,
    has_command: bool,
) -> bool {
    #[cfg(windows)]
    {
        let _ = has_command;
        detached && !print_session_info && !grouped && windows_deferred_initial_pane_enabled()
    }
    #[cfg(not(windows))]
    {
        let _ = (detached, print_session_info, grouped, has_command);
        false
    }
}

#[cfg(windows)]
fn windows_deferred_initial_pane_enabled() -> bool {
    std::env::var("RMUX_WINDOWS_DEFER_CONPTY")
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true)
}
