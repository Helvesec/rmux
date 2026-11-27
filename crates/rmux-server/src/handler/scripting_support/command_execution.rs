mod dispatched_request;
#[cfg(test)]
#[path = "source_input_tests.rs"]
mod source_input_tests;

use self::dispatched_request::{
    quiet_only_if_unset_response, DispatchedQueueRequest, QueueDispatchOrigin,
    QueuedRunShellResponse,
};
use super::command_admission::QueuedCommandAdmission;
use super::diagnostics::source_file_runtime_error;
use super::queue::QueueTerminalClient;
#[cfg(test)]
use super::queue_exact_target;
use super::shell_runtime::RunShellCommandError;
use super::{
    apply_queue_context_to_request, capture_switch_client_target_identity,
    captures_attached_client_transition, control_command_response_stream_is_active,
    maybe_capture_control_command_response, queue_invocation_allowed_for_read_only,
    rebase_expected_attach_session_after_switch, send_control_command_response_event,
    source_file_context_error, with_queued_display_target_client, ControlClientIdentity,
    ControlCommandResponseEvent, HookName, ParsedCommand, QueueCommandAction,
    QueueExecutionContext, QueueInvocation, QueueMode, QueuedCommandExecution,
    QueuedCurrentSessionTransition, Request, RequestHandler, Response, RmuxError, Target,
    QUEUED_COMMAND_CONTEXT,
};
use crate::future::boxed;

fn prepare_clientless_terminal_invocation(invocation: &mut QueueInvocation) {
    let request = match invocation {
        QueueInvocation::Request(request) | QueueInvocation::QuietOnlyIfUnset(request) => request,
        _ => return,
    };
    match request {
        Request::AttachSession(_)
        | Request::AttachSessionExt(_)
        | Request::AttachSessionExt2(_)
        | Request::AttachSessionExt3(_) => *invocation = QueueInvocation::NoOp,
        Request::NewSession(request) => request.detached = true,
        Request::NewSessionExt(request) => request.detached = true,
        _ => {}
    }
}

impl RequestHandler {
    pub(super) async fn execute_queued_command(
        &self,
        requester_pid: u32,
        command: ParsedCommand,
        context: &QueueExecutionContext,
        mode: QueueMode,
        expected_control_id: Option<u64>,
    ) -> Result<QueuedCommandExecution, RmuxError> {
        let floating = command.name() == "new-pane";
        let execution = QUEUED_COMMAND_CONTEXT.scope(
            context.clone(),
            self.execute_queued_command_scoped(
                requester_pid,
                command,
                context,
                mode,
                expected_control_id,
                None,
            ),
        );
        let execution = self
            .with_new_pane_authority(requester_pid, floating, execution)
            .await;
        // Same applied-window-resize backstop as `run_dispatched_hooks`, for
        // the invocations that never dispatch a `Request` (new-window,
        // split-window, choose-tree, display-popup, source-file, ...). Every
        // queued command therefore leaves the geometry queue empty, whether it
        // came from a control client, a multi-command CLI invocation, a hook or
        // a key binding.
        self.publish_applied_window_resizes().await;
        execution
    }

    #[async_recursion::async_recursion]
    pub(super) async fn execute_queued_command_scoped(
        &self,
        requester_pid: u32,
        command: ParsedCommand,
        context: &QueueExecutionContext,
        mode: QueueMode,
        expected_control_id: Option<u64>,
        stdin_payload: Option<Vec<u8>>,
    ) -> Result<QueuedCommandExecution, RmuxError> {
        let (input_context, stdin_payload) = if command.name() == "source-file" {
            (
                stdin_payload.map(|bytes| context.clone().with_invocation_input(bytes)),
                None,
            )
        } else {
            (None, stdin_payload)
        };
        let context = input_context.as_ref().unwrap_or(context);
        let command_for_hooks = command.clone();
        if mode == QueueMode::Control {
            self.validate_control_queue_session_identity(
                requester_pid,
                expected_control_id.expect("control queues capture a client identity"),
            )
            .await?;
        }
        let attached_session = self.current_session_candidate(requester_pid).await;
        let admission = self.admit_queued_command(
            requester_pid,
            (command, &command_for_hooks),
            attached_session.as_ref(),
            context,
            mode,
            stdin_payload,
        );
        #[cfg(test)]
        let admission =
            super::send_keys_admission_probe::observe(self, command_for_hooks.name(), admission);
        let invocation = admission.await;
        let QueuedCommandAdmission {
            mut invocation,
            send_keys_client,
            target_identities,
            retained_lifecycle_target,
            retained_lifecycle_identity,
            special_target_binding,
            session_rename,
        } = match invocation {
            Ok(invocation) => invocation,
            Err(error) => {
                self.run_command_error_hook_for_parsed_command(
                    requester_pid,
                    &command_for_hooks,
                    context.current_target.clone(),
                    attached_session.as_ref(),
                )
                .await;
                return Err(source_file_runtime_error(
                    error,
                    &command_for_hooks,
                    context,
                    mode,
                ));
            }
        };
        let queued_display_target_client = self
            .capture_queued_display_target_client(requester_pid, &mut invocation)
            .await;
        #[cfg(test)]
        queue_exact_target::pause_after_queue_exact_target_capture(self, command_for_hooks.name())
            .await;
        let floating_move_authority = if matches!(
            &invocation,
            QueueInvocation::MoveFloatingPane(_) | QueueInvocation::SetPanePlacement(_)
        ) {
            Some(
                self.capture_floating_move_write_authority(requester_pid)
                    .await?,
            )
        } else {
            None
        };
        let can_write = if floating_move_authority.is_some() {
            true
        } else {
            self.requester_can_write(requester_pid).await
        };
        if !can_write && !queue_invocation_allowed_for_read_only(&invocation) {
            return Err(RmuxError::Server("client is read-only".to_owned()));
        }
        if context.terminal_client == QueueTerminalClient::Clientless {
            prepare_clientless_terminal_invocation(&mut invocation);
        }
        // A shell job prints after its control guard. Command callbacks (-C)
        // retain their own child guards; background jobs use their delivery path.
        let stdout_after_frame = matches!(
            &invocation,
            QueueInvocation::RunShell(command)
                if !command.request.as_commands && !command.request.background
        );
        let request_invocation = invocation.uses_request_hooks();
        let quiet_only_if_unset = matches!(&invocation, QueueInvocation::QuietOnlyIfUnset(_));
        let queued_success_hook = match &invocation {
            QueueInvocation::ListWindowsAll(command) => Some((
                HookName::AfterListWindows,
                command.hook_target.clone().map(Target::Session),
            )),
            QueueInvocation::MoveFloatingPane(command) => Some((
                HookName::AfterResizePane,
                Some(Target::Pane(command.target.clone())),
            )),
            _ => None,
        };
        let hook_current_target = queued_success_hook
            .as_ref()
            .and_then(|(_, target)| target.clone())
            .or_else(|| context.current_target.clone());
        let origin = QueueDispatchOrigin {
            requester_pid,
            mode,
            expected_control_id,
        };

        let mut attached_switch_target = None;
        let mut committed_current_session_transition = None;
        let mut committed_session_rename = None;
        let mut response_frame_completed = false;
        let mut error_after_frame = false;
        let result = match invocation {
            QueueInvocation::NoOp => Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            }),
            invocation @ (QueueInvocation::Request(_)
            | QueueInvocation::QuietOnlyIfUnset(_)
            | QueueInvocation::RestoreWindowSnapshot(_)) => {
                let request = match invocation {
                    QueueInvocation::Request(request)
                    | QueueInvocation::QuietOnlyIfUnset(request) => request,
                    QueueInvocation::RestoreWindowSnapshot(command) => match command.load().await {
                        Ok(request) => request,
                        Err(error) => {
                            self.run_command_error_hook_for_parsed_command(
                                requester_pid,
                                &command_for_hooks,
                                hook_current_target,
                                attached_session.as_ref(),
                            )
                            .await;
                            return Err(source_file_runtime_error(
                                error,
                                &command_for_hooks,
                                context,
                                mode,
                            ));
                        }
                    },
                    _ => unreachable!("request invocation matched above"),
                };
                let explicit_target_run_shell = match &request {
                    Request::RunShell(request) if !request.as_commands => request.target.clone(),
                    _ => None,
                };
                let explicit_run_shell_target = self
                    .resolve_explicit_run_shell_target(explicit_target_run_shell)
                    .await;
                let request = apply_queue_context_to_request(request, context, false);
                let request = crate::server_access::apply_access_policy(request, can_write)?;
                // Only an executed request needs a terminal. Stored bindings and
                // hooks retain their command text, without this queue's constraint.
                let needs_terminal = match &request {
                    Request::AttachSession(_)
                    | Request::AttachSessionExt(_)
                    | Request::AttachSessionExt2(_)
                    | Request::AttachSessionExt3(_) => true,
                    Request::NewSession(request) => !request.detached,
                    Request::NewSessionExt(request) => !request.detached,
                    _ => false,
                };
                if context.terminal_attach_is_refused() && needs_terminal {
                    return Err(RmuxError::Server(
                        "open terminal failed: not a terminal".to_owned(),
                    ));
                }
                if let (QueueMode::Control, Some(control_id), Request::WaitFor(wait)) =
                    (mode, expected_control_id, &request)
                {
                    let identity = ControlClientIdentity::new(requester_pid, control_id);
                    if matches!(
                        wait.mode,
                        rmux_proto::WaitForMode::Wait | rmux_proto::WaitForMode::Lock
                    ) && control_command_response_stream_is_active(identity)
                    {
                        response_frame_completed = send_control_command_response_event(
                            identity,
                            ControlCommandResponseEvent::FrameCompleted {
                                stdout: Vec::new(),
                                error: None,
                                trailing_stdout: Vec::new(),
                            },
                        );
                    }
                }
                let capture_control_response = mode == QueueMode::Control
                    && (context.run_shell_command_depth() == 0
                        || context.control_queue_origin().is_some())
                    && matches!(
                        &request,
                        Request::DisplayMessage(_) | Request::DisplayMessageExt(_)
                    );
                let request_for_hooks = request.clone();
                let captures_client_transition = captures_attached_client_transition(&request);
                let dispatch = boxed(|| {
                    maybe_capture_control_command_response(
                        capture_control_response,
                        with_queued_display_target_client(
                            queued_display_target_client,
                            crate::handler::with_expected_stable_target_identities(
                                target_identities,
                                retained_lifecycle_target,
                                retained_lifecycle_identity,
                                super::queued_send_keys_client::with_client(
                                    send_keys_client,
                                    self.dispatch_captured_with_client_name(
                                        requester_pid,
                                        u64::from(requester_pid),
                                        request,
                                        context.client_name.clone(),
                                    ),
                                ),
                            ),
                        ),
                    )
                });
                let dispatch =
                    QueuedCurrentSessionTransition::capture(context, &request_for_hooks, dispatch);
                let (
                    ((mut outcome, inline_hooks), current_session_transition),
                    switch_client_capture,
                ) = if captures_client_transition {
                    capture_switch_client_target_identity(dispatch).await
                } else {
                    (dispatch.await, Default::default())
                };
                if quiet_only_if_unset {
                    outcome.response =
                        quiet_only_if_unset_response(&request_for_hooks, outcome.response);
                }
                committed_current_session_transition = current_session_transition
                    .and_then(|transition| transition.commit(&outcome.response));
                committed_session_rename =
                    session_rename.and_then(|rename| rename.commit(&outcome.response));
                if captures_client_transition {
                    // A `new-session` that moved no attached client (a
                    // control or clientless queue) leaves the queue where it was.
                    let switched = match &outcome.response {
                        Response::SwitchClient(response) => Some((
                            &response.session_name,
                            switch_client_capture.targeted_client.ok_or_else(|| {
                                RmuxError::Server(
                                    "switch-client response omitted its targeted client identity"
                                        .to_owned(),
                                )
                            })?,
                        )),
                        Response::NewSession(response) if !response.detached => {
                            switch_client_capture
                                .targeted_client
                                .map(|client| (&response.session_name, client))
                        }
                        _ => None,
                    };
                    if let Some((session_name, targeted_client)) = switched {
                        attached_switch_target = rebase_expected_attach_session_after_switch(
                            self,
                            requester_pid,
                            targeted_client,
                            session_name,
                            switch_client_capture.committed_target,
                        )
                        .await?;
                    }
                }
                boxed(|| {
                    self.complete_dispatched_queue_request(
                        origin,
                        DispatchedQueueRequest {
                            request: request_for_hooks,
                            outcome,
                            inline_hooks,
                            explicit_run_shell_target,
                        },
                        &command_for_hooks,
                    )
                })
                .await
            }
            QueueInvocation::RunShell(command) => {
                let target_missing_canfail = command.target_missing_canfail;
                let inserts_commands = command.request.as_commands && !command.request.background;
                let explicit_target_run_shell = command.request.target.clone();
                let request = Request::RunShell(Box::new(command.request));
                let request =
                    apply_queue_context_to_request(request, context, target_missing_canfail);
                let request = crate::server_access::apply_access_policy(request, can_write)?;
                let Request::RunShell(request) = request else {
                    return Err(RmuxError::Server(
                        "queued run-shell lost its request payload".to_owned(),
                    ));
                };
                let client_name = context.client_name.clone();
                if inserts_commands {
                    let prepared = boxed(|| {
                        self.execute_queued_run_shell_commands(
                            requester_pid,
                            *request,
                            client_name,
                            target_missing_canfail,
                            context,
                            special_target_binding.as_deref(),
                        )
                    })
                    .await;
                    match prepared {
                        Ok(action) => Ok(action),
                        Err(RunShellCommandError::Preparation(error)) => Err(error),
                        Err(RunShellCommandError::Parse(error))
                            if crate::handler::control_support::legacy_control_response_stream(
                            ) =>
                        {
                            Err(source_file_context_error(
                                error,
                                &command_for_hooks,
                                context,
                            ))
                        }
                        Err(RunShellCommandError::Parse(error)) => {
                            // tmux reports a callback parser failure after %end.
                            // Retain failure status without cancelling the outer group.
                            error_after_frame = true;
                            let error =
                                source_file_context_error(error, &command_for_hooks, context);
                            Ok(QueueCommandAction::Normal {
                                output: None,
                                error: Some(error),
                                source_file_error: None,
                                exit_status: Some(1),
                            })
                        }
                    }
                } else {
                    boxed(|| {
                        self.execute_queued_run_shell_response(
                            origin,
                            QueuedRunShellResponse {
                                request,
                                explicit_target: explicit_target_run_shell,
                                client_name,
                                target_missing_canfail,
                                special_target_binding: special_target_binding.as_deref(),
                            },
                            context,
                            &command_for_hooks,
                        )
                    })
                    .await
                }
            }
            QueueInvocation::StartServer => Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            }),
            QueueInvocation::ListCommands(command) => self.execute_queued_list_commands(command),
            QueueInvocation::NewWindow(command) => {
                crate::handler::with_expected_stable_target_identities(
                    target_identities,
                    retained_lifecycle_target,
                    retained_lifecycle_identity,
                    // Keep the transaction future and its construction copy out
                    // of every recursive queue frame.
                    boxed(|| self.execute_queued_new_window(requester_pid, command, context)),
                )
                .await
            }
            QueueInvocation::IfShell(command) => {
                boxed(|| {
                    self.execute_queued_if_shell(
                        requester_pid,
                        command,
                        context,
                        special_target_binding.as_deref(),
                    )
                })
                .await
            }
            QueueInvocation::SourceFile(command) => {
                boxed(|| {
                    self.execute_queued_source_file(
                        requester_pid,
                        command,
                        context,
                        special_target_binding.as_deref(),
                    )
                })
                .await
            }
            QueueInvocation::ListPanesAll(command) => {
                self.execute_queued_list_panes_all(command).await
            }
            QueueInvocation::ListWindowsAll(command) => {
                self.execute_queued_list_windows_all(command).await
            }
            QueueInvocation::SplitWindow(command) => {
                boxed(|| {
                    self.execute_guarded_split_window(
                        requester_pid,
                        (&command_for_hooks, command),
                        context,
                        target_identities,
                        retained_lifecycle_target,
                        retained_lifecycle_identity,
                    )
                })
                .await
            }
            QueueInvocation::NewPane(command) => {
                crate::handler::with_expected_stable_target_identities(
                    target_identities,
                    retained_lifecycle_target,
                    retained_lifecycle_identity,
                    boxed(|| self.execute_new_pane(requester_pid, command, context)),
                )
                .await
            }
            QueueInvocation::MoveFloatingPane(command) => {
                crate::handler::with_expected_stable_target_identities(
                    target_identities,
                    retained_lifecycle_target,
                    retained_lifecycle_identity,
                    boxed(|| {
                        self.execute_floating_pane_move(
                            command,
                            floating_move_authority
                                .expect("floating moves capture write authority before dispatch"),
                        )
                    }),
                )
                .await
            }
            QueueInvocation::SetPanePlacement(command) => {
                crate::handler::with_expected_stable_target_identities(
                    target_identities,
                    retained_lifecycle_target,
                    retained_lifecycle_identity,
                    boxed(|| {
                        self.execute_pane_placement(
                            command,
                            floating_move_authority
                                .expect("pane placement captures write authority before dispatch"),
                        )
                    }),
                )
                .await
            }
            QueueInvocation::MouseResizePane(target) => {
                boxed(|| self.execute_queued_mouse_resize_pane(requester_pid, target, context))
                    .await
            }
            QueueInvocation::CommandPrompt(command) => {
                boxed(|| self.execute_queued_command_prompt(requester_pid, command, context)).await
            }
            QueueInvocation::ConfirmBefore(command) => {
                boxed(|| self.execute_queued_confirm_before(requester_pid, command, context)).await
            }
            QueueInvocation::ModeTree(command) => {
                boxed(|| self.execute_queued_mode_tree(requester_pid, command, context)).await
            }
            QueueInvocation::Overlay(command) => {
                boxed(|| self.execute_queued_overlay(requester_pid, command, context)).await
            }
            QueueInvocation::PromptHistory(command) => {
                self.execute_queued_prompt_history(command).await
            }
        };

        if result.is_ok() {
            if let Some((hook, _)) = queued_success_hook {
                self.run_command_success_hook_for_parsed_command(
                    requester_pid,
                    hook,
                    &command_for_hooks,
                    hook_current_target.clone(),
                    attached_session.as_ref(),
                )
                .await;
            }
        }

        if result.is_err() && !request_invocation {
            self.run_command_error_hook_for_parsed_command(
                requester_pid,
                &command_for_hooks,
                hook_current_target,
                attached_session.as_ref(),
            )
            .await;
        }

        result
            .map(|action| QueuedCommandExecution {
                action,
                response_frame_completed,
                stdout_after_frame,
                error_after_frame,
                attached_switch_target,
                current_session_transition: committed_current_session_transition,
                session_rename: committed_session_rename,
            })
            .map_err(|error| source_file_runtime_error(error, &command_for_hooks, context, mode))
    }
}
