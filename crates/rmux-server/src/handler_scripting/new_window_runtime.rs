use rmux_core::{LifecycleEvent, SessionStore};
use rmux_proto::request::Request;
use rmux_proto::{
    DisplayMessageRequest, ErrorResponse, HookName, MoveWindowRequest, MoveWindowTarget,
    NewWindowRequest, NewWindowResponse, PaneTarget, Response, RmuxError, ScopeSelector,
    SelectWindowRequest, SessionName, Target, WindowTarget,
};

use super::format_context_for_target_with_server_values;
use super::queue::{queue_action_from_response, QueueCommandAction, QueueExecutionContext};
use super::queue_parse::{NewWindowPlacement, ParsedNewWindowCommand};
use super::render_start_directory_template;
use super::targets::NewWindowTargetIndex;
use crate::format_runtime::render_runtime_template;
use crate::handler::{
    client_environment_snapshot, client_spawn_environment, prepare_lifecycle_event_if_enabled,
    RequestHandler, StableTargetIdentity,
};
use crate::hook_runtime::{capture_inline_hooks, PendingInlineHookFormat};
use crate::pane_terminals::{
    resolve_new_pane_process_command, HandlerState, NewWindowOptions, WindowSpawnOptions,
};

#[derive(Debug, Clone)]
pub(in crate::handler) struct QueuedNewWindowTargetWitness {
    session_name: SessionName,
    session: StableTargetIdentity,
    resolved_window_index: Option<u32>,
    anchor: Option<StableTargetIdentity>,
    destination: Option<WindowSlotWitness>,
}

#[derive(Debug, Clone)]
enum WindowSlotWitness {
    Vacant(u32),
    Occupied(StableTargetIdentity),
}

impl QueuedNewWindowTargetWitness {
    pub(super) fn capture(
        state: &mut HandlerState,
        command: &ParsedNewWindowCommand,
    ) -> Result<Self, RmuxError> {
        let active_window_index = state
            .sessions
            .session(&command.target)
            .ok_or_else(|| RmuxError::SessionNotFound(command.target.to_string()))?
            .active_window_index();
        let session =
            StableTargetIdentity::capture(state, Target::Session(command.target.clone()))?;
        let resolved_window_index = resolve_queued_new_window_target_index(
            &state.sessions,
            &command.target,
            command.target_window_index,
        )?;
        let anchor_index = match (command.placement, command.target_window_index) {
            (Some(NewWindowPlacement::Before), _) => resolved_window_index,
            (Some(NewWindowPlacement::After), _) => Some(
                resolved_window_index
                    .and_then(|index| index.checked_sub(1))
                    .ok_or_else(|| {
                        RmuxError::Server("new-window placement anchor underflowed".to_owned())
                    })?,
            ),
            (None, Some(NewWindowTargetIndex::Relative(_))) => Some(active_window_index),
            (None, _) => None,
        };
        let anchor = anchor_index
            .map(|index| {
                StableTargetIdentity::capture(
                    state,
                    Target::Window(WindowTarget::with_window(command.target.clone(), index)),
                )
            })
            .transpose()?;
        let destination = resolved_window_index
            .map(|index| capture_window_slot(state, &command.target, index))
            .transpose()?;
        Ok(Self {
            session_name: command.target.clone(),
            session,
            resolved_window_index,
            anchor,
            destination,
        })
    }

    fn validate(&self, state: &HandlerState) -> Result<(), RmuxError> {
        if !self.session.is_current(state)
            || self
                .anchor
                .as_ref()
                .is_some_and(|anchor| !anchor.is_current(state))
            || self
                .destination
                .as_ref()
                .is_some_and(|slot| !slot.matches(state, &self.session_name))
        {
            return Err(changed_new_window_target(&self.session_name));
        }
        Ok(())
    }
}

impl WindowSlotWitness {
    fn matches(&self, state: &HandlerState, session_name: &SessionName) -> bool {
        match self {
            Self::Occupied(identity) => identity.is_current(state),
            Self::Vacant(index) => state
                .sessions
                .session(session_name)
                .and_then(|session| session.window_at(*index))
                .is_none(),
        }
    }
}

fn capture_window_slot(
    state: &mut HandlerState,
    session_name: &SessionName,
    index: u32,
) -> Result<WindowSlotWitness, RmuxError> {
    let occupied = state
        .sessions
        .session(session_name)
        .and_then(|session| session.window_at(index))
        .is_some();
    if occupied {
        StableTargetIdentity::capture(
            state,
            Target::Window(WindowTarget::with_window(session_name.clone(), index)),
        )
        .map(WindowSlotWitness::Occupied)
    } else {
        Ok(WindowSlotWitness::Vacant(index))
    }
}

fn changed_new_window_target(session_name: &SessionName) -> RmuxError {
    RmuxError::invalid_target(
        session_name.to_string(),
        "queued new-window target changed before mutation",
    )
}

impl RequestHandler {
    pub(super) async fn execute_queued_new_window(
        &self,
        requester_pid: u32,
        command: ParsedNewWindowCommand,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        let ParsedNewWindowCommand {
            target,
            target_window_index: _,
            insert_at_target,
            placement: _,
            target_witness,
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
        let target_witness = *target_witness.ok_or_else(|| {
            RmuxError::Server("queued new-window target witness was not captured".to_owned())
        })?;
        let target_window_index = target_witness.resolved_window_index;
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

        if let Some(environment) = environment.as_deref() {
            crate::terminal::parse_environment_assignments(environment)?;
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
            let (response, linked_event, committed_move) = {
                let mut state = self.state.lock().await;
                if let Err(error) = target_witness.validate(&state) {
                    return Response::Error(ErrorResponse { error });
                }
                if let Err(error) =
                    crate::handler::require_expected_session_identity(&state, &target)
                {
                    return Response::Error(ErrorResponse { error });
                }
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
                    Ok(created) => {
                        let destination = replace_after_create.map(|window_index| {
                            WindowTarget::with_window(target.clone(), window_index)
                        });
                        if let Some(destination) =
                            destination.filter(|destination| destination != &created.target)
                        {
                            if let Err(error) = target_witness.validate(&state) {
                                let error = self.rollback_unpublished_new_window_locked(
                                    &mut state,
                                    &created.target,
                                    error,
                                );
                                return Response::Error(ErrorResponse { error });
                            }
                            let request = MoveWindowRequest {
                                source: Some(created.target.clone()),
                                target: MoveWindowTarget::Window(destination.clone()),
                                renumber: false,
                                kill_destination: true,
                                detached,
                                after: false,
                                before: false,
                            };
                            let committed = match self
                                .commit_prevalidated_move_window_locked(&mut state, &request)
                            {
                                Ok(committed) => committed,
                                Err(error) => {
                                    let error = self.rollback_unpublished_new_window_locked(
                                        &mut state,
                                        &created.target,
                                        error,
                                    );
                                    return Response::Error(ErrorResponse { error });
                                }
                            };
                            (
                                Response::NewWindow(created),
                                None,
                                Some((committed, destination)),
                            )
                        } else {
                            let mut timer_targets = Vec::new();
                            for timer_session_name in state.sessions.session_group_members(&target)
                            {
                                let Some(session) = state.sessions.session(&timer_session_name)
                                else {
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
                                    target: Some(created.target.clone()),
                                },
                            );
                            (Response::NewWindow(created), linked_event, None)
                        }
                    }
                    Err(error) => (Response::Error(ErrorResponse { error }), None, None),
                }
            };

            let response = match committed_move {
                Some((committed, destination)) => {
                    match self.finish_committed_move_window(committed).await {
                        Response::MoveWindow(moved) => Response::NewWindow(NewWindowResponse {
                            target: moved.target.unwrap_or(destination),
                        }),
                        response => response,
                    }
                }
                None => response,
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
                let refresh_target = match &response {
                    Response::NewWindow(success) => success.target.session_name(),
                    _ => &target,
                };
                self.refresh_attached_session(refresh_target).await;
            }

            response
        })
        .await;

        super::super::without_expected_stable_target_identities(async {
            // tmux renders -P/-F before after-new-window hooks may move the target.
            let action = self
                .queued_new_window_action(requester_pid, print_target, format, response.clone())
                .await;
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

            action
        })
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
        _session_name: &rmux_proto::SessionName,
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
        Ok((None, Some(window_index)))
    }

    fn rollback_unpublished_new_window_locked(
        &self,
        state: &mut HandlerState,
        target: &WindowTarget,
        source_error: RmuxError,
    ) -> RmuxError {
        match state.kill_window(target.clone(), false) {
            Ok(removed) => {
                state.retire_removed_lifecycle_targets();
                self.record_panes_closed_as_killed(&removed.removed_pane_ids);
                source_error
            }
            Err(rollback_error) => RmuxError::Server(format!(
                "failed to roll back unpublished new-window {target} after {source_error}: {rollback_error}"
            )),
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
