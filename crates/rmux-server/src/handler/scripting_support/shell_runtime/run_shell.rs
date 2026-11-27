//! run-shell entry points, the run-shell task and its response output.

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

use rmux_proto::{
    CommandOutput, ErrorResponse, Response, RmuxError, RunShellRequest, RunShellResponse,
};

use super::super::queue::{QueueCommandAction, QueueExecutionContext};
use super::super::queue_special_target::QueueSpecialTargetBinding;
use super::super::runtime::{run_shell_delay_duration, run_shell_foreground};
use crate::client_format::ClientLoopSnapshot;
use crate::handler::control_support::current_control_queue_identity;
use crate::handler::{current_expected_attach_identity, RequestHandler};
use crate::hook_runtime::{current_hook_execution, current_hook_formats, with_hook_execution};

use super::{
    with_background_client_identities, QueuedRunShellState, RunShellCommandError, ShellTargetPolicy,
};

impl RequestHandler {
    pub(in crate::handler) async fn handle_run_shell(
        &self,
        requester_pid: u32,
        request: RunShellRequest,
    ) -> Response {
        let explicit_target = match request.target.as_ref() {
            Some(target) if !request.as_commands => self
                .pane_id_for_slot_target(target)
                .await
                .map(|pane_id| (target.clone(), pane_id)),
            _ => None,
        };
        let mut response = self
            .handle_run_shell_with_client_name(requester_pid, request, None)
            .await;
        if let Some((target, pane_id)) = explicit_target {
            if self
                .deliver_targeted_run_shell_output(requester_pid, &target, pane_id, &response)
                .await
            {
                if let Response::RunShell(response) = &mut response {
                    response.output = None;
                }
            }
        }
        response
    }

    pub(in crate::handler) async fn handle_run_shell_with_client_name(
        &self,
        requester_pid: u32,
        request: RunShellRequest,
        client_name: Option<String>,
    ) -> Response {
        self.handle_run_shell_with_client_name_and_target_state(
            requester_pid,
            request,
            client_name,
            false,
            None,
            None,
        )
        .await
    }

    pub(in crate::handler::scripting_support) async fn handle_queued_run_shell_with_client_name(
        &self,
        requester_pid: u32,
        request: RunShellRequest,
        client_name: Option<String>,
        target_missing_canfail: bool,
        parent_context: &QueueExecutionContext,
        target_binding: Option<&QueueSpecialTargetBinding>,
    ) -> Response {
        self.handle_run_shell_with_client_name_and_target_state(
            requester_pid,
            request,
            client_name,
            target_missing_canfail,
            Some(parent_context.clone()),
            target_binding.cloned(),
        )
        .await
    }

    async fn handle_run_shell_with_client_name_and_target_state(
        &self,
        requester_pid: u32,
        mut request: RunShellRequest,
        client_name: Option<String>,
        target_missing_canfail: bool,
        parent_context: Option<QueueExecutionContext>,
        target_binding: Option<QueueSpecialTargetBinding>,
    ) -> Response {
        if let Some(target_binding) = target_binding.as_ref() {
            if let Err(error) = target_binding.require_live_for(self).await {
                return Response::Error(ErrorResponse { error });
            }
        }
        let historical_retired = target_binding
            .as_ref()
            .is_some_and(QueueSpecialTargetBinding::is_historical_retired);
        let historical_profile = target_binding
            .as_ref()
            .and_then(QueueSpecialTargetBinding::historical_retired_profile)
            .cloned();
        // A historical hook may run shell work, but it must not inherit an
        // attached or preferred live pane as an implicit replacement target.
        let target_missing_canfail = target_missing_canfail || historical_retired;
        let parent_depth = super::super::queued_command_context()
            .map(|context| context.run_shell_command_depth())
            .unwrap_or_default();
        let parent_context = if request.background {
            parent_context.map(QueueExecutionContext::without_mouse_origin)
        } else {
            parent_context
        };
        let parent_client_loop_snapshots = parent_context
            .as_ref()
            .and_then(QueueExecutionContext::client_loop_snapshots)
            .map(<[ClientLoopSnapshot]>::to_vec);
        let queue_state = QueuedRunShellState {
            parent_depth,
            parent_context,
            target_binding,
            dispatch: None,
        };
        let target_was_captured = request.target.is_some() || target_missing_canfail;
        let control_identity = current_control_queue_identity(requester_pid);
        let attach_identity = current_expected_attach_identity();
        let follows_attached_session =
            request.background && attach_identity.is_some() && !target_was_captured;
        if request.target.is_none() && !target_missing_canfail && !follows_attached_session {
            request.target = match self.inherited_run_shell_target(requester_pid).await {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
        }
        if request.background {
            let target_policy = if follows_attached_session {
                ShellTargetPolicy::FollowAttachedSession
            } else {
                ShellTargetPolicy::Fixed
            };
            if target_policy.follows_attached_session() {
                request.target = None;
            }
            if let Some(delay_seconds) = request.delay_seconds {
                if let Err(error) = run_shell_delay_duration(delay_seconds.as_secs_f64()) {
                    return Response::Error(ErrorResponse { error });
                }
            }
            let detached_request_guard = self.begin_detached_request();
            let requester_access_guard = self
                .begin_inherited_detached_requester_access(requester_pid)
                .await;
            let handler = self.clone();
            let hook_formats = current_hook_formats();
            let hook_execution = current_hook_execution();
            if let Err(error) = self.spawn_background_task("rmux-run-shell", move || async move {
                let task = async move {
                    let _detached_request_guard = detached_request_guard;
                    let _requester_access_guard = requester_access_guard;
                    let _ = handler
                        .run_shell_task(
                            requester_pid,
                            request,
                            client_name,
                            target_policy,
                            target_missing_canfail,
                            queue_state,
                        )
                        .await;
                };
                with_background_client_identities(control_identity, attach_identity, async move {
                    match hook_execution {
                        Some(execution) => with_hook_execution(execution, hook_formats, task).await,
                        None => task.await,
                    }
                })
                .await;
            }) {
                return Response::Error(ErrorResponse { error });
            }
            return Response::RunShell(RunShellResponse::background());
        }

        if let Some(delay_seconds) = request.delay_seconds {
            if let Err(error) = run_shell_delay_duration(delay_seconds.as_secs_f64()) {
                return Response::Error(ErrorResponse { error });
            }
        }
        let mut dispatch = match self
            .capture_synchronous_run_shell_dispatch(
                requester_pid,
                &mut request,
                client_name.as_deref(),
                target_missing_canfail,
                parent_client_loop_snapshots.as_deref(),
            )
            .await
        {
            Ok(dispatch) => dispatch,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        if !request.as_commands {
            if let Some(profile) = historical_profile {
                dispatch.shell_profile = Some(profile);
            }
        }
        let queue_state = QueuedRunShellState {
            dispatch: Some(dispatch),
            ..queue_state
        };
        match self
            .run_shell_task(
                requester_pid,
                request,
                client_name,
                ShellTargetPolicy::Fixed,
                target_missing_canfail,
                queue_state,
            )
            .await
        {
            Ok(RunShellTaskOutput::CommandOutput(output)) => {
                Response::RunShell(RunShellResponse::from_output(output))
            }
            Ok(RunShellTaskOutput::Shell {
                output: Some(output),
                exit_status,
            }) => Response::RunShell(RunShellResponse::from_output_and_exit_status(
                output,
                exit_status,
            )),
            Ok(RunShellTaskOutput::Shell {
                output: None,
                exit_status,
            }) => Response::RunShell(RunShellResponse::from_exit_status(exit_status)),
            Ok(RunShellTaskOutput::NoOutput) => Response::RunShell(RunShellResponse::background()),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    async fn run_shell_task(
        &self,
        requester_pid: u32,
        request: RunShellRequest,
        client_name: Option<String>,
        target_policy: ShellTargetPolicy,
        target_missing_canfail: bool,
        queue_state: QueuedRunShellState,
    ) -> Result<RunShellTaskOutput, RmuxError> {
        if let Some(target_binding) = queue_state.target_binding.as_ref() {
            target_binding.require_live_for(self).await?;
        }
        let Some(request) = self
            .prepare_run_shell_request(requester_pid, request, target_policy)
            .await?
        else {
            return Ok(RunShellTaskOutput::NoOutput);
        };
        if let Some(target_binding) = queue_state.target_binding.as_ref() {
            target_binding.require_live_for(self).await?;
        }

        if request.as_commands {
            let (parsed, context) = self
                .prepare_run_shell_commands(
                    requester_pid,
                    request,
                    client_name,
                    target_policy,
                    target_missing_canfail,
                    &queue_state,
                )
                .await
                .map_err(RunShellCommandError::into_error)?;
            let output = self
                .execute_parsed_commands(requester_pid, parsed, context)
                .await?;
            return Ok(if output.stdout().is_empty() {
                RunShellTaskOutput::NoOutput
            } else {
                RunShellTaskOutput::CommandOutput(output)
            });
        }

        let profile = match queue_state
            .dispatch
            .as_ref()
            .and_then(|dispatch| dispatch.shell_profile.clone())
        {
            Some(profile) => profile,
            None => match queue_state
                .target_binding
                .as_ref()
                .and_then(QueueSpecialTargetBinding::historical_retired_profile)
            {
                Some(profile) => profile.clone(),
                None => self.run_shell_profile(&request).await?,
            },
        };
        if let Some(target_binding) = queue_state.target_binding.as_ref() {
            target_binding.require_live_for(self).await?;
        }
        let command = match queue_state.dispatch.as_ref() {
            Some(dispatch) => dispatch.expanded_command.clone(),
            None => {
                self.expand_run_shell_command(
                    &request,
                    client_name.as_deref(),
                    target_missing_canfail,
                    queue_state
                        .parent_context
                        .as_ref()
                        .and_then(QueueExecutionContext::client_loop_snapshots),
                )
                .await?
            }
        };
        if let Some(target_binding) = queue_state.target_binding.as_ref() {
            target_binding.require_live_for(self).await?;
        }
        let output = run_shell_foreground(
            command.clone(),
            &profile,
            request.show_stderr,
            Some(self.shell_processes.clone()),
        )
        .await?;
        let exit_status = shell_exit_status(&output.status);
        let stdout = run_shell_stdout_for_response(
            output.stdout,
            &command,
            exit_status,
            shell_exit_signal(&output.status),
        );
        let output = (!stdout.is_empty()).then(|| CommandOutput::from_stdout(stdout));
        Ok(RunShellTaskOutput::Shell {
            output,
            exit_status,
        })
    }

    pub(in crate::handler::scripting_support) async fn execute_queued_run_shell_commands(
        &self,
        requester_pid: u32,
        mut request: RunShellRequest,
        client_name: Option<String>,
        target_missing_canfail: bool,
        parent_context: &QueueExecutionContext,
        target_binding: Option<&QueueSpecialTargetBinding>,
    ) -> Result<QueueCommandAction, RunShellCommandError> {
        debug_assert!(request.as_commands);
        debug_assert!(!request.background);
        if let Some(target_binding) = target_binding {
            target_binding.require_live_for(self).await?;
        }
        if request.target.is_none() && !target_missing_canfail {
            request.target = self.inherited_run_shell_target(requester_pid).await?;
        }
        if let Some(delay_seconds) = request.delay_seconds {
            run_shell_delay_duration(delay_seconds.as_secs_f64())?;
        }
        let dispatch = self
            .capture_synchronous_run_shell_dispatch(
                requester_pid,
                &mut request,
                client_name.as_deref(),
                target_missing_canfail,
                parent_context.client_loop_snapshots(),
            )
            .await?;
        let Some(request) = self
            .prepare_run_shell_request(requester_pid, request, ShellTargetPolicy::Fixed)
            .await?
        else {
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        };
        if let Some(target_binding) = target_binding {
            target_binding.require_live_for(self).await?;
        }
        let queue_state = QueuedRunShellState {
            parent_depth: parent_context.run_shell_command_depth(),
            parent_context: Some(parent_context.clone()),
            target_binding: target_binding.cloned(),
            dispatch: Some(dispatch),
        };
        let (commands, context) = self
            .prepare_run_shell_commands(
                requester_pid,
                request,
                client_name,
                ShellTargetPolicy::Fixed,
                target_missing_canfail,
                &queue_state,
            )
            .await?;
        if let Some(target_binding) = target_binding {
            target_binding.require_live_for(self).await?;
        }
        Ok(QueueCommandAction::InsertAfter {
            batches: vec![(commands, context)],
            output: None,
            error: None,
            source_file_error: None,
            exit_status: None,
        })
    }
}

enum RunShellTaskOutput {
    NoOutput,
    CommandOutput(CommandOutput),
    Shell {
        output: Option<CommandOutput>,
        exit_status: i32,
    },
}

fn shell_exit_status(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }

    #[cfg(unix)]
    {
        status.signal().map_or(1, |signal| 128 + signal)
    }

    #[cfg(not(unix))]
    {
        1
    }
}

fn shell_exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        status.signal()
    }

    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

fn run_shell_stdout_for_response(
    mut stdout: Vec<u8>,
    command: &str,
    exit_status: i32,
    signal: Option<i32>,
) -> Vec<u8> {
    if !stdout.is_empty() && !stdout.ends_with(b"\n") {
        stdout.push(b'\n');
    }
    if exit_status == 0 {
        return stdout;
    }
    if let Some(signal) = signal {
        stdout.extend_from_slice(format!("'{command}' terminated by signal {signal}\n").as_bytes());
        return stdout;
    }
    stdout.extend_from_slice(format!("'{command}' returned {exit_status}\n").as_bytes());
    stdout
}
