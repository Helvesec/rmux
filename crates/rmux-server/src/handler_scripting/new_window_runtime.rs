use rmux_core::{LifecycleEvent, SessionStore};
use rmux_proto::request::Request;
use rmux_proto::{
    DisplayMessageRequest, ErrorResponse, HookName, KillWindowRequest, MoveWindowRequest,
    MoveWindowTarget, NewWindowRequest, NewWindowResponse, PaneTarget, Response, RmuxError,
    ScopeSelector, SelectWindowRequest, Target, WindowTarget,
};

use super::format_context_for_target_with_server_values;
use super::queue::{queue_action_from_response, QueueCommandAction, QueueExecutionContext};
use super::queue_parse::ParsedNewWindowCommand;
use super::render_start_directory_template;
use super::targets::NewWindowTargetIndex;
use crate::format_runtime::render_runtime_template;
use crate::handler::{
    client_environment_snapshot, client_spawn_environment, prepare_lifecycle_event_if_enabled,
    RequestHandler,
};
use crate::hook_runtime::{capture_inline_hooks, PendingInlineHookFormat};
use crate::pane_terminals::{
    resolve_new_pane_process_command, NewWindowOptions, WindowSpawnOptions,
};

impl RequestHandler {
    pub(super) async fn execute_queued_new_window(
        &self,
        requester_pid: u32,
        command: ParsedNewWindowCommand,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        let ParsedNewWindowCommand {
            target,
            target_window_index,
            insert_at_target,
            name,
            detached,
            print_target,
            format,
            kill_existing,
            select_existing,
            start_directory,
            environment,
            command,
        } = command;
        let start_directory = start_directory.or_else(|| context.caller_cwd.clone());

        let target_window_index = {
            let state = self.state.lock().await;
            resolve_queued_new_window_target_index(&state.sessions, &target, target_window_index)?
        };
        let can_write = self.requester_can_write(requester_pid).await;
        let request_for_hooks = crate::server_access::apply_access_policy(
            Request::NewWindow(Box::new(NewWindowRequest {
                target: target.clone(),
                name: name.clone(),
                detached,
                environment: environment.clone(),
                command: command.clone(),
                process_command: None,
                start_directory: start_directory.clone(),
                target_window_index,
                insert_at_target,
            })),
            can_write,
        )?;

        if select_existing && target_window_index.is_none() {
            if let Some(name) = name.as_deref() {
                if let Some(existing) = self.find_existing_new_window_by_name(&target, name).await?
                {
                    if detached {
                        return Ok(empty_new_window_action());
                    }
                    return queue_action_from_response(
                        self.handle_select_window(
                            Some(requester_pid),
                            SelectWindowRequest { target: existing },
                        )
                        .await,
                    );
                }
            }
        }

        let (target_window_index, replace_after_create) = self
            .prepare_queued_new_window_kill_existing(
                &target,
                target_window_index,
                insert_at_target,
                kill_existing,
            )
            .await?;

        let socket_path = self.socket_path();
        let attached_count = self.attached_count(&target).await;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let rendered_command = self
            .render_queued_new_window_command(
                command.as_deref(),
                &target,
                context,
                attached_count,
                &socket_path,
            )
            .await?;
        let explicit_process_command =
            crate::legacy_command::from_legacy_command(rendered_command.as_deref());
        let client_environment = client_environment_snapshot(requester_pid);
        let spawn_environment = client_spawn_environment(client_environment.as_ref());
        let (response, inline_hooks) = capture_inline_hooks(async {
            let (response, linked_event) = {
                let mut state = self.state.lock().await;
                let process_command = resolve_new_pane_process_command(
                    &state.options,
                    &target,
                    explicit_process_command,
                );
                let timer_sessions = state.sessions.session_group_members(&target);
                let timer_mutation =
                    self.plan_window_mutation_silence_timers_locked(&state, timer_sessions);
                let start_directory = match render_start_directory_template(
                    &state,
                    &Target::Session(target.clone()),
                    attached_count,
                    start_directory.clone(),
                ) {
                    Ok(start_directory) => start_directory,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                };
                match state.create_window_at_requested_index(
                    &target,
                    target_window_index,
                    insert_at_target,
                    NewWindowOptions {
                        name,
                        detached,
                        spawn: WindowSpawnOptions {
                            start_directory: start_directory.as_deref(),
                            command: process_command.as_ref(),
                            socket_path: &socket_path,
                            spawn_environment: spawn_environment.as_ref(),
                            environment_overrides: environment.as_deref(),
                            respawn_shell: None,
                            respawn_environment: None,
                            pane_alert_callback: Some(self.pane_alert_callback()),
                            pane_exit_callback: Some(self.pane_exit_callback()),
                        },
                    },
                ) {
                    Ok(response) => {
                        let mut timer_targets = Vec::new();
                        for timer_session_name in state.sessions.session_group_members(&target) {
                            let Some(session) = state.sessions.session(&timer_session_name) else {
                                continue;
                            };
                            timer_targets.extend(session.windows().keys().copied().map(
                                |window_index| {
                                    WindowTarget::with_window(
                                        timer_session_name.clone(),
                                        window_index,
                                    )
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
                        let linked_event = prepare_lifecycle_event_if_enabled(
                            &mut state,
                            &LifecycleEvent::WindowLinked {
                                session_name: target.clone(),
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
                    self.queue_inline_hook(
                        HookName::AfterNewWindow,
                        ScopeSelector::Session(target.clone()),
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
                self.refresh_attached_session(&target).await;
            }

            response
        })
        .await;
        let response = match replace_after_create {
            Some(window_index) => {
                self.move_queued_new_window_to_replacement_index(response, window_index, detached)
                    .await
            }
            None => response,
        };

        let inline_hook_names = inline_hooks
            .iter()
            .map(|pending| pending.hook)
            .collect::<Vec<_>>();
        self.run_inline_hooks(requester_pid, inline_hooks, None)
            .await;
        self.run_request_hooks(
            requester_pid,
            &request_for_hooks,
            &response,
            None,
            &inline_hook_names,
        )
        .await;

        self.queued_new_window_action(requester_pid, print_target, format, response)
            .await
    }

    async fn find_existing_new_window_by_name(
        &self,
        session_name: &rmux_proto::SessionName,
        name: &str,
    ) -> Result<Option<WindowTarget>, RmuxError> {
        let state = self.state.lock().await;
        let session = state
            .sessions
            .session(session_name)
            .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?;
        let matches = session
            .windows()
            .iter()
            .filter_map(|(index, window)| (window.name() == Some(name)).then_some(*index))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => Ok(None),
            [window_index] => Ok(Some(WindowTarget::with_window(
                session_name.clone(),
                *window_index,
            ))),
            _ => Err(RmuxError::Server(format!("multiple windows named {name}"))),
        }
    }

    async fn prepare_queued_new_window_kill_existing(
        &self,
        session_name: &rmux_proto::SessionName,
        target_window_index: Option<u32>,
        insert_at_target: bool,
        kill_existing: bool,
    ) -> Result<(Option<u32>, Option<u32>), RmuxError> {
        let Some(window_index) = target_window_index else {
            return Ok((None, None));
        };
        if !kill_existing || insert_at_target {
            return Ok((Some(window_index), None));
        }

        let existing_window_count = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session(session_name)
                .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?;
            session
                .window_at(window_index)
                .map(|_| session.windows().len())
        };
        match existing_window_count {
            None => Ok((Some(window_index), None)),
            Some(1) => Ok((None, Some(window_index))),
            Some(_) => match self
                .handle_kill_window(KillWindowRequest {
                    target: WindowTarget::with_window(session_name.clone(), window_index),
                    kill_all_others: false,
                })
                .await
            {
                Response::KillWindow(_) => Ok((Some(window_index), None)),
                Response::Error(ErrorResponse { error }) => Err(error),
                response => Err(RmuxError::Server(format!(
                    "unexpected kill-window response while replacing new-window target: {response:?}"
                ))),
            },
        }
    }

    async fn move_queued_new_window_to_replacement_index(
        &self,
        response: Response,
        window_index: u32,
        detached: bool,
    ) -> Response {
        let Response::NewWindow(created) = response else {
            return response;
        };
        let destination =
            WindowTarget::with_window(created.target.session_name().clone(), window_index);
        match self
            .handle_move_window(MoveWindowRequest {
                source: Some(created.target),
                target: MoveWindowTarget::Window(destination.clone()),
                renumber: false,
                kill_destination: true,
                detached,
                after: false,
                before: false,
            })
            .await
        {
            Response::MoveWindow(moved) => Response::NewWindow(NewWindowResponse {
                target: moved.target.unwrap_or(destination),
            }),
            response => response,
        }
    }

    async fn queued_new_window_action(
        &self,
        requester_pid: u32,
        print_target: bool,
        format: String,
        response: Response,
    ) -> Result<QueueCommandAction, RmuxError> {
        let pane = match &response {
            Response::NewWindow(response) if print_target => PaneTarget::with_window(
                response.target.session_name().clone(),
                response.target.window_index(),
                0,
            ),
            _ => return queue_action_from_response(response),
        };
        queue_action_from_response(
            self.handle_display_message(
                requester_pid,
                DisplayMessageRequest {
                    target: Some(Target::Pane(pane)),
                    print: true,
                    message: Some(format),
                    empty_target_context: false,
                },
            )
            .await,
        )
    }
    async fn render_queued_new_window_command(
        &self,
        command: Option<&[String]>,
        target: &rmux_proto::SessionName,
        context: &QueueExecutionContext,
        attached_count: usize,
        socket_path: &std::path::Path,
    ) -> Result<Option<Vec<String>>, RmuxError> {
        let Some(command) = command else {
            return Ok(None);
        };
        if !command.iter().any(|argument| argument.contains("#{")) {
            return Ok(Some(command.to_vec()));
        }

        let format_target = context
            .current_target()
            .cloned()
            .unwrap_or_else(|| Target::Session(target.clone()));
        let state = self.state.lock().await;
        let mut runtime = format_context_for_target_with_server_values(
            &state,
            &format_target,
            attached_count,
            socket_path,
        )?;
        if let Some(client_name) = context.client_name.as_ref() {
            runtime = runtime.with_named_value("client_name", client_name.clone());
        }

        Ok(Some(
            command
                .iter()
                .map(|argument| render_runtime_template(argument, &runtime, false))
                .collect(),
        ))
    }
}

fn empty_new_window_action() -> QueueCommandAction {
    QueueCommandAction::Normal {
        output: None,
        error: None,
        source_file_error: None,
        exit_status: None,
    }
}

fn resolve_queued_new_window_target_index(
    sessions: &SessionStore,
    target: &rmux_proto::SessionName,
    target_window_index: Option<NewWindowTargetIndex>,
) -> Result<Option<u32>, RmuxError> {
    let Some(target_window_index) = target_window_index else {
        return Ok(None);
    };

    match target_window_index {
        NewWindowTargetIndex::Absolute(index) => Ok(Some(index)),
        NewWindowTargetIndex::Relative(offset) => {
            let active = sessions
                .session(target)
                .ok_or_else(|| RmuxError::SessionNotFound(target.to_string()))?
                .active_window_index();
            if offset >= 0 {
                Ok(Some(active.checked_add(offset as u32).ok_or_else(
                    || RmuxError::Server("window index space exhausted for new-window".to_owned()),
                )?))
            } else {
                Ok(Some(active.checked_sub(offset.unsigned_abs()).ok_or_else(
                    || RmuxError::invalid_target(target.to_string(), "window offset out of range"),
                )?))
            }
        }
    }
}
