use rmux_core::LifecycleEvent;
use rmux_proto::{
    ErrorResponse, HookName, PaneId, Response, RmuxError, ScopeSelector, Target, WindowTarget,
};

use super::super::{
    client_environment_snapshot, client_spawn_environment,
    focus_support::prepare_server_focus_effects,
    prepare_lifecycle_event,
    scripting_support::{
        client_name_for_pid, render_start_directory_template, render_target_argument,
    },
    RequestHandler, SelectionTransitionSnapshot, StablePaneOutputIdentity,
};
use crate::hook_runtime::PendingInlineHookFormat;
use crate::pane_io::AttachControl;
use crate::pane_terminals::{resolve_new_pane_process_command, HandlerState, SplitPaneProcess};
use crate::terminal::validate_process_command;

use super::pane_split_effects::{
    apply_split_window_effects, ensure_detached_zoom_pane_available, split_window_effects,
};

pub(in crate::handler) struct SplitWindowParts<'a> {
    pub(in crate::handler) target: rmux_proto::SplitWindowTarget,
    pub(in crate::handler) expected_pane_id: Option<PaneId>,
    pub(in crate::handler) floating: Option<rmux_core::FloatingPaneRequest>,
    pub(in crate::handler) direction: rmux_proto::SplitDirection,
    pub(in crate::handler) before: bool,
    pub(in crate::handler) environment_overrides: Option<Vec<String>>,
    pub(in crate::handler) command: Option<Vec<String>>,
    pub(in crate::handler) process_command: Option<rmux_proto::ProcessCommand>,
    pub(in crate::handler) start_directory: Option<std::path::PathBuf>,
    pub(in crate::handler) keep_alive_on_exit:
        Option<crate::pane_terminals::SplitExitPolicyOverride>,
    pub(in crate::handler) detached: bool,
    pub(in crate::handler) size: Option<String>,
    pub(in crate::handler) preserve_zoom: bool,
    pub(in crate::handler) full_size: bool,
    pub(in crate::handler) stdin_payload: Option<Vec<u8>>,
    pub(in crate::handler) stdin_policy: PaneStdinPolicy<'a>,
    pub(in crate::handler) response_mode: SplitWindowResponseMode,
}

/// How a pane's own standard-input payload reaches its screen.
#[derive(Debug, Default)]
pub(in crate::handler) enum PaneStdinPolicy<'a> {
    /// `split-window -I`, as this server acquired it: a bare line feed is
    /// rewritten as CRLF before the parser sees the bytes.
    #[default]
    SplitNormalized,
    /// `new-pane -I`: the finished output of a tool reaches the parser exactly
    /// as it was read, and the float's screen is first put in the state the
    /// source gives an empty pane - the cursor is not shown and a line feed
    /// also returns the carriage (`spawn.c:373-376`). Rewriting the bytes
    /// instead would change a payload that is data, not command text.
    FloatingRaw,
    /// Pin the created screen under the commit lock before waiting for stdin.
    FloatingOnDemand(&'a mut Option<StablePaneOutputIdentity>),
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
                floating: None,
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
                stdin_policy: PaneStdinPolicy::default(),
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
        let exit_policy = request.keep_alive_on_exit.map(Into::into);
        self.handle_split_window_command_parts(requester_pid, request, exit_policy)
            .await
    }

    pub(in crate::handler) async fn handle_split_window_command_parts(
        &self,
        requester_pid: u32,
        request: rmux_proto::SplitWindowExtRequest,
        exit_policy: Option<crate::pane_terminals::SplitExitPolicyOverride>,
    ) -> Response {
        self.handle_split_window_parts(
            requester_pid,
            SplitWindowParts {
                target: request.target,
                expected_pane_id: None,
                floating: None,
                direction: request.direction,
                before: request.before,
                environment_overrides: request.environment,
                command: request.command,
                process_command: request.process_command,
                start_directory: request.start_directory,
                keep_alive_on_exit: exit_policy,
                detached: request.detached,
                size: request.size,
                preserve_zoom: request.preserve_zoom,
                full_size: request.full_size,
                stdin_payload: request.stdin_payload,
                stdin_policy: PaneStdinPolicy::default(),
                response_mode: SplitWindowResponseMode::Legacy,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_split_window_parts(
        &self,
        requester_pid: u32,
        parts: SplitWindowParts<'_>,
    ) -> Response {
        let SplitWindowParts {
            target,
            expected_pane_id,
            floating,
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
            stdin_policy,
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
        let empty_pane = stdin_payload.is_some()
            && explicit_process_command
                .as_ref()
                .is_none_or(rmux_proto::ProcessCommand::is_empty);
        if let Err(error) = validate_process_command(explicit_process_command.as_ref()) {
            return Response::Error(ErrorResponse { error });
        }
        let attached_count = self.attached_count(&session_name).await;
        let client_loop_snapshots = self.client_loop_snapshots().await;
        let client_name = client_name_for_pid(&client_loop_snapshots, requester_pid);
        let client_environment = client_environment_snapshot(requester_pid);
        let spawn_environment = client_spawn_environment(client_environment.as_ref());
        let (response, successful_pane, refresh_sessions, focus_effects, lifecycle_events) = {
            let mut state = self.state.lock().await;
            let mut floating_access = if floating.is_some() {
                match self.lock_new_pane_commit(&state).await {
                    Ok(access) => Some(access),
                    Err(error) => return Response::Error(ErrorResponse { error }),
                }
            } else {
                None
            };
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
            if floating.is_some()
                || keep_alive_on_exit == Some(crate::pane_terminals::SplitExitPolicyOverride::Key)
            {
                if let rmux_proto::SplitWindowTarget::Pane(target) = &target {
                    if let Err(error) = super::super::require_expected_pane_identity(&state, target)
                    {
                        return Response::Error(ErrorResponse { error });
                    }
                }
            }
            let process_command = if empty_pane {
                None
            } else {
                resolve_new_pane_process_command(
                    &state.options,
                    &session_name,
                    explicit_process_command,
                )
            };
            let start_directory = match render_start_directory_template(
                &state,
                &format_target,
                attached_count,
                start_directory,
                Some(&client_loop_snapshots),
                client_name,
            ) {
                Ok(start_directory) => start_directory,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let expand_size = |argument: &str| {
                render_target_argument(
                    &state,
                    &format_target,
                    attached_count,
                    argument,
                    Some(&client_loop_snapshots),
                    client_name,
                )
            };
            let split_effects = split_window_effects(
                &state,
                &target,
                direction,
                detached,
                size.as_deref(),
                expand_size,
            );
            let split_effects = match split_effects {
                Ok(effects) => effects,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            if let Err(error) =
                ensure_detached_zoom_pane_available(&state, &split_effects, preserve_zoom)
            {
                return Response::Error(ErrorResponse { error });
            }
            let selection_before = SelectionTransitionSnapshot::capture(&state);
            match state.split_window(
                target,
                direction,
                before,
                &socket_path,
                spawn_environment.as_ref(),
                environment_overrides.as_deref(),
                if empty_pane {
                    SplitPaneProcess::Empty
                } else {
                    SplitPaneProcess::Process(process_command.as_ref())
                },
                start_directory.as_deref(),
                keep_alive_on_exit,
                split_effects.size,
                full_size,
                detached,
                floating,
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
                        if let Some(payload) = stdin_payload {
                            let injected = match stdin_policy {
                                PaneStdinPolicy::FloatingRaw => {
                                    mark_empty_pane_screen(&state, &response.pane).and_then(|()| {
                                        append_floating_stdin_output(
                                            &state,
                                            &response.pane,
                                            payload,
                                        )
                                    })
                                }
                                PaneStdinPolicy::FloatingOnDemand(identity) => {
                                    *identity = StablePaneOutputIdentity::capture_for_target(
                                        &state,
                                        &Target::Pane(response.pane.clone()),
                                    );
                                    mark_empty_pane_screen(&state, &response.pane)
                                }
                                PaneStdinPolicy::SplitNormalized if !payload.is_empty() => {
                                    inject_split_window_stdin_output(
                                        &mut state,
                                        &response.pane,
                                        &payload,
                                    )
                                }
                                PaneStdinPolicy::SplitNormalized => Ok(()),
                            };
                            if let Err(error) = injected {
                                return Response::Error(ErrorResponse { error });
                            }
                        }
                        let successful_pane = response.pane.clone();
                        let refresh_sessions = state.window_linked_session_family_list(
                            successful_pane.session_name(),
                            successful_pane.window_index(),
                        );
                        match split_window_response(&state, response, response_mode) {
                            Ok(response) => {
                                let layout_target = WindowTarget::with_window(
                                    successful_pane.session_name().clone(),
                                    successful_pane.window_index(),
                                );
                                let mut active_attach = match floating_access.take() {
                                    Some(access) => access.attach,
                                    None => self.active_attach.lock().await,
                                };
                                let focus_effects =
                                    prepare_server_focus_effects(&mut state, &mut active_attach);
                                let mut lifecycle_events = selection_before
                                    .prepare_window_pane_changes(
                                        &mut state,
                                        std::slice::from_ref(&layout_target),
                                    );
                                lifecycle_events.push(prepare_lifecycle_event(
                                    &mut state,
                                    &LifecycleEvent::WindowLayoutChanged {
                                        target: layout_target,
                                    },
                                ));
                                (
                                    response,
                                    Some(successful_pane),
                                    refresh_sessions,
                                    Some(focus_effects),
                                    lifecycle_events,
                                )
                            }
                            Err(error) => (
                                Response::Error(ErrorResponse { error }),
                                None,
                                Vec::new(),
                                None,
                                Vec::new(),
                            ),
                        }
                    }
                    Err(error) => (
                        Response::Error(ErrorResponse { error }),
                        None,
                        Vec::new(),
                        None,
                        Vec::new(),
                    ),
                },
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    None,
                    Vec::new(),
                    None,
                    Vec::new(),
                ),
            }
        };

        if let Some(successful_pane) = successful_pane {
            if floating.is_none() {
                self.queue_inline_hook(
                    HookName::AfterSplitWindow,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Pane(successful_pane.clone())),
                    PendingInlineHookFormat::AfterCommand,
                );
            }
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
            for event in lifecycle_events {
                self.emit_prepared(event).await;
            }
            self.refresh_linked_window_sessions(refresh_sessions).await;
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

/// Empty panes hide the cursor and treat LF as CRLF before any bytes are fed.
fn mark_empty_pane_screen(
    state: &HandlerState,
    target: &rmux_proto::PaneTarget,
) -> Result<(), RmuxError> {
    state
        .transcript_handle(target)?
        .lock()
        .mark_as_empty_pane_screen();
    Ok(())
}

pub(in crate::handler) fn append_floating_stdin_output(
    state: &HandlerState,
    target: &rmux_proto::PaneTarget,
    payload: Vec<u8>,
) -> Result<(), RmuxError> {
    if payload.is_empty() {
        return Ok(());
    }
    let transcript = state.transcript_handle(target)?;
    let pane_output = state.pane_output_for_target(
        target.session_name(),
        target.window_index(),
        target.pane_index(),
    )?;
    let _ = pane_output.publish_for_generation_with_invalidation(None, payload, |bytes| {
        let append = transcript.lock().append_bytes_with_effects(bytes);
        let invalidation = append
            .recovery_rebase_required
            .then_some(crate::pane_io::PaneInvalidationReason::TranscriptMutation);
        ((), Vec::new(), invalidation)
    });
    Ok(())
}

fn inject_split_window_stdin_output(
    state: &mut HandlerState,
    target: &rmux_proto::PaneTarget,
    payload: &[u8],
) -> Result<(), rmux_proto::RmuxError> {
    let payload = normalize_split_window_stdin_payload(payload);
    let transcript = state.transcript_handle(target)?;
    let pane_output = state.pane_output_for_target(
        target.session_name(),
        target.window_index(),
        target.pane_index(),
    )?;
    let _ = pane_output.publish_for_generation_with_invalidation(None, payload, |bytes| {
        let append = transcript.lock().append_bytes_with_effects(bytes);
        let invalidation = append
            .recovery_rebase_required
            .then_some(crate::pane_io::PaneInvalidationReason::TranscriptMutation);
        ((), Vec::new(), invalidation)
    });
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
