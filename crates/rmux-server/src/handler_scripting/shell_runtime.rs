use std::future::Future;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

use rmux_core::{command_parser::ParsedCommands, formats::is_truthy, PaneId};
use rmux_proto::{
    CommandOutput, ErrorResponse, IfShellRequest, IfShellResponse, PaneTarget, Response, RmuxError,
    RunShellRequest, RunShellResponse, SessionName, Target,
};

use super::super::control_support::{
    current_control_queue_identity, with_control_queue_identity, ControlClientIdentity,
};
#[cfg(windows)]
use super::super::pane_support::format_references_pane_pid;
use super::super::target_support::{pane_id_target, requester_environment_pane_id};
use super::super::{
    attach_support::ActiveAttachIdentity, current_expected_attach_identity,
    validate_expected_attach_identity, with_expected_attach_registration, RequestHandler,
};
use super::command_args::CommandListArgument;
use super::format_context::{format_context_for_target_with_server_values, global_format_context};
use super::queue::{QueueCommandAction, QueueExecutionContext};
use super::queue_parse::ParsedIfShellCommand;
use super::queue_special_target::QueueSpecialTargetBinding;
use super::runtime::{run_shell_delay_duration, run_shell_foreground, shell_condition_is_true};
use super::targets::active_session_target;
use crate::format_runtime::render_runtime_template;
use crate::hook_runtime::{current_hook_execution, current_hook_formats, with_hook_execution};
use crate::terminal::{SessionBaseEnvironment, TerminalProfile};

async fn with_background_control_identity<T, F>(
    identity: Option<ControlClientIdentity>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    match identity {
        Some(identity) => with_control_queue_identity(identity, future).await,
        None => future.await,
    }
}

async fn with_background_client_identities<T, F>(
    control_identity: Option<ControlClientIdentity>,
    attach_identity: Option<ActiveAttachIdentity>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    let control_scoped = with_background_control_identity(control_identity, future);
    match attach_identity {
        Some(identity) => with_expected_attach_registration(identity, control_scoped).await,
        None => control_scoped.await,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellTargetPolicy {
    Fixed,
    FollowAttachedSession,
}

impl ShellTargetPolicy {
    fn follows_attached_session(self) -> bool {
        self == Self::FollowAttachedSession
    }
}

fn if_shell_runs_in_background(background: bool, format_mode: bool) -> bool {
    background && !format_mode
}

#[derive(Debug, Clone)]
struct QueuedRunShellState {
    parent_depth: usize,
    parent_context: Option<QueueExecutionContext>,
    target_binding: Option<QueueSpecialTargetBinding>,
}

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

    pub(super) async fn handle_queued_run_shell_with_client_name(
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
        let parent_depth = super::queued_command_context()
            .map(|context| context.run_shell_command_depth())
            .unwrap_or_default();
        let parent_context = if request.background {
            parent_context.map(QueueExecutionContext::without_mouse_origin)
        } else {
            parent_context
        };
        let queue_state = QueuedRunShellState {
            parent_depth,
            parent_context,
            target_binding,
        };
        let target_was_captured = request.target.is_some() || target_missing_canfail;
        if request.target.is_none() && !target_missing_canfail {
            request.target = self.inherited_run_shell_target(requester_pid).await;
        }
        if request.background {
            let control_identity = current_control_queue_identity(requester_pid);
            let attach_identity = current_expected_attach_identity();
            let target_policy = if attach_identity.is_some() && !target_was_captured {
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

    async fn inherited_run_shell_target(&self, requester_pid: u32) -> Option<PaneTarget> {
        let pane_id = requester_environment_pane_id(requester_pid, &self.socket_path())?;
        let state = self.state.lock().await;
        match pane_id_target(&state.sessions, pane_id) {
            Some(Target::Pane(target)) => Some(target),
            _ => None,
        }
    }

    async fn followed_attached_pane_target(
        &self,
        identity: ActiveAttachIdentity,
    ) -> Result<PaneTarget, RmuxError> {
        match self
            .attached_queue_target_for_registration(identity)
            .await?
        {
            Target::Pane(target) => Ok(target),
            _ => Err(RmuxError::Server(
                "attached session has no current pane target".to_owned(),
            )),
        }
    }

    pub(in crate::handler) async fn handle_if_shell(
        &self,
        requester_pid: u32,
        request: IfShellRequest,
    ) -> Response {
        self.handle_if_shell_with_client_name(requester_pid, request, None)
            .await
    }

    pub(in crate::handler) async fn handle_if_shell_with_client_name(
        &self,
        requester_pid: u32,
        request: IfShellRequest,
        client_name: Option<String>,
    ) -> Response {
        if if_shell_runs_in_background(request.background, request.format_mode) {
            let control_identity = current_control_queue_identity(requester_pid);
            let attach_identity = current_expected_attach_identity();
            let target_policy = if attach_identity.is_some() && request.target.is_none() {
                ShellTargetPolicy::FollowAttachedSession
            } else {
                ShellTargetPolicy::Fixed
            };
            let detached_request_guard = self.begin_detached_request();
            let requester_access_guard = self
                .begin_inherited_detached_requester_access(requester_pid)
                .await;
            let handler = self.clone();
            let hook_formats = current_hook_formats();
            let hook_execution = current_hook_execution();
            if let Err(error) = self.spawn_background_task("rmux-if-shell", move || async move {
                let task = async move {
                    let _detached_request_guard = detached_request_guard;
                    let _requester_access_guard = requester_access_guard;
                    let _ = handler
                        .if_shell_task(requester_pid, request, client_name, target_policy)
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
            return Response::IfShell(IfShellResponse::no_output());
        }

        match self
            .if_shell_task(
                requester_pid,
                request,
                client_name,
                ShellTargetPolicy::Fixed,
            )
            .await
        {
            Ok(Some(output)) if !output.stdout().is_empty() => {
                Response::IfShell(IfShellResponse::from_output(output))
            }
            Ok(_) => Response::IfShell(IfShellResponse::no_output()),
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
                .await?;
            let output = self
                .execute_parsed_commands(requester_pid, parsed, context)
                .await?;
            return Ok(if output.stdout().is_empty() {
                RunShellTaskOutput::NoOutput
            } else {
                RunShellTaskOutput::CommandOutput(output)
            });
        }

        let profile = self.run_shell_profile(&request).await?;
        if let Some(target_binding) = queue_state.target_binding.as_ref() {
            target_binding.require_live_for(self).await?;
        }
        let command = self
            .expand_run_shell_command(&request, client_name.as_deref(), target_missing_canfail)
            .await?;
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

    pub(super) async fn execute_queued_run_shell_commands(
        &self,
        requester_pid: u32,
        mut request: RunShellRequest,
        client_name: Option<String>,
        target_missing_canfail: bool,
        parent_context: &QueueExecutionContext,
        target_binding: Option<&QueueSpecialTargetBinding>,
    ) -> Result<QueueCommandAction, RmuxError> {
        debug_assert!(request.as_commands);
        debug_assert!(!request.background);
        if let Some(target_binding) = target_binding {
            target_binding.require_live_for(self).await?;
        }
        if request.target.is_none() && !target_missing_canfail {
            request.target = self.inherited_run_shell_target(requester_pid).await;
        }
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

    async fn prepare_run_shell_request(
        &self,
        requester_pid: u32,
        mut request: RunShellRequest,
        target_policy: ShellTargetPolicy,
    ) -> Result<Option<RunShellRequest>, RmuxError> {
        if let Some(delay_seconds) = request.delay_seconds {
            tokio::time::sleep(run_shell_delay_duration(delay_seconds.as_secs_f64())?).await;
        }
        if target_policy.follows_attached_session() {
            let identity = validate_expected_attach_identity(self, requester_pid)
                .await?
                .ok_or_else(|| {
                    RmuxError::Server(
                        "background command lost its attached client identity".to_owned(),
                    )
                })?;
            request.target = Some(self.followed_attached_pane_target(identity).await?);
        } else if request.as_commands {
            // Command-mode jobs can act on the attached registration even
            // with a fixed pane target, so retain the same-PID reuse guard.
            let _ = validate_expected_attach_identity(self, requester_pid).await?;
        }
        Ok((!request.command.is_empty()).then_some(request))
    }

    async fn prepare_run_shell_commands(
        &self,
        requester_pid: u32,
        request: RunShellRequest,
        client_name: Option<String>,
        target_policy: ShellTargetPolicy,
        target_missing_canfail: bool,
        queue_state: &QueuedRunShellState,
    ) -> Result<(ParsedCommands, QueueExecutionContext), RmuxError> {
        let has_fixed_target = request.target.is_some();
        let command = self
            .expand_run_shell_command(&request, client_name.as_deref(), target_missing_canfail)
            .await?;
        let parsed = self.parse_command_string_one_group(&command).await?;
        if parsed_contains_attach_session(&parsed) {
            return Err(RmuxError::Server(
                "open terminal failed: not a terminal".to_owned(),
            ));
        }
        let current_target = if target_missing_canfail {
            None
        } else {
            self.run_shell_commands_current_target(requester_pid, request.target.clone())
                .await
        };
        let context = match queue_state.parent_context.as_ref() {
            Some(context) => match request.start_directory.clone() {
                Some(caller_cwd) => context.clone().with_caller_cwd(Some(caller_cwd)),
                None => context.clone(),
            },
            None => QueueExecutionContext::new(request.start_directory.clone()),
        };
        let context = if let Some(target_binding) = queue_state.target_binding.as_ref() {
            target_binding.child_context(&context)
        } else {
            match target_policy {
                ShellTargetPolicy::FollowAttachedSession => context
                    .with_implicit_current_target(current_target)
                    .following_attached_session(),
                ShellTargetPolicy::Fixed if has_fixed_target => {
                    context.with_current_target(current_target)
                }
                ShellTargetPolicy::Fixed => context.with_implicit_current_target(current_target),
            }
        }
        .with_client_name(client_name)
        .with_mouse_event(super::queued_command_mouse_event().or_else(|| {
            queue_state
                .parent_context
                .as_ref()
                .and_then(|context| context.mouse_event.clone())
        }));
        let context = match request.source_depth {
            Some(depth) => context.for_sourced_commands(depth, None),
            None => context,
        }
        .for_run_shell_commands(queue_state.parent_depth)?;
        Ok((parsed, context))
    }

    async fn run_shell_commands_current_target(
        &self,
        requester_pid: u32,
        target: Option<PaneTarget>,
    ) -> Option<Target> {
        if let Some(target) = target {
            return self
                .existing_target_or_none(Some(Target::Pane(target)))
                .await;
        }

        let session_name = match self.current_session_candidate(requester_pid).await {
            Some(session_name) => Some(session_name),
            None => self.preferred_session_name().await.ok(),
        }?;
        let state = self.state.lock().await;
        active_session_target(&state.sessions, &session_name)
    }

    async fn existing_target_or_none(&self, target: Option<Target>) -> Option<Target> {
        let target = target?;
        let state = self.state.lock().await;
        let exists = match &target {
            Target::Session(session_name) => state.sessions.contains_session(session_name),
            Target::Window(target) => state
                .sessions
                .session(target.session_name())
                .is_some_and(|session| session.window_at(target.window_index()).is_some()),
            Target::Pane(target) => state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .is_some_and(|window| window.pane(target.pane_index()).is_some()),
        };
        exists.then_some(target)
    }

    async fn if_shell_task(
        &self,
        requester_pid: u32,
        mut request: IfShellRequest,
        client_name: Option<String>,
        target_policy: ShellTargetPolicy,
    ) -> Result<Option<CommandOutput>, RmuxError> {
        let expected_attach = validate_expected_attach_identity(self, requester_pid).await?;
        if target_policy.follows_attached_session() {
            let identity = expected_attach.ok_or_else(|| {
                RmuxError::Server("background command lost its attached client identity".to_owned())
            })?;
            request.target = Some(Target::Pane(
                self.followed_attached_pane_target(identity).await?,
            ));
        }
        let expanded_condition = self
            .expand_if_shell_condition(&request, client_name.as_deref())
            .await?;

        let condition_is_true = if request.format_mode {
            if_shell_format_condition_is_true(&expanded_condition)
        } else {
            let profile = self.if_shell_profile(&request).await?;
            shell_condition_is_true(
                expanded_condition,
                &profile,
                Some(self.shell_processes.clone()),
            )
            .await?
        };

        let selected_command = if condition_is_true {
            Some(request.then_command)
        } else {
            request.else_command
        };
        let Some(selected_command) = selected_command else {
            return Ok(None);
        };

        let parsed = self
            .parse_command_string_one_group(&selected_command)
            .await?;
        let current_target = self.existing_target_or_none(request.target).await;
        let context = QueueExecutionContext::new(request.caller_cwd);
        let context = match target_policy {
            ShellTargetPolicy::FollowAttachedSession => context
                .with_implicit_current_target(current_target)
                .following_attached_session(),
            ShellTargetPolicy::Fixed if current_target.is_some() => {
                context.with_current_target(current_target)
            }
            ShellTargetPolicy::Fixed => context.with_implicit_current_target(current_target),
        }
        .with_client_name(client_name);
        let output = self
            .execute_parsed_commands(requester_pid, parsed, context)
            .await?;
        Ok((!output.stdout().is_empty()).then_some(output))
    }

    async fn expand_run_shell_command(
        &self,
        request: &RunShellRequest,
        client_name: Option<&str>,
        target_missing_canfail: bool,
    ) -> Result<String, RmuxError> {
        #[cfg(windows)]
        if format_references_pane_pid(Some(&request.command)) {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
        let target = request.target.as_ref();
        let attached_count = if let Some(target) = target {
            self.attached_count(target.session_name()).await
        } else {
            0
        };

        let hook_formats = current_hook_formats();
        let socket_path = self.socket_path();
        let state = self.state.lock().await;
        let context = match target {
            Some(target) => format_context_for_target_with_server_values(
                &state,
                &Target::Pane(target.clone()),
                attached_count,
                &socket_path,
            )
            .unwrap_or_else(|_| global_format_context(&state, &socket_path)),
            None if !target_missing_canfail => match hook_formats
                .iter()
                .rev()
                .find(|(name, _)| name == "hook_session_name")
                .and_then(|(_, value)| SessionName::new(value.clone()).ok())
                .and_then(|session_name| hook_session_default_target(&state, &session_name))
            {
                Some(target) => {
                    format_context_for_target_with_server_values(&state, &target, 0, &socket_path)?
                }
                None => global_format_context(&state, &socket_path),
            },
            None => global_format_context(&state, &socket_path),
        };
        let context = match client_name {
            Some(client_name) => context.with_named_value("client_name", client_name.to_owned()),
            None => context,
        };
        let context = hook_formats
            .into_iter()
            .fold(context, |context, (name, value)| {
                context.with_named_value(name, value)
            });
        let context = if request.as_commands {
            context
        } else {
            request
                .arguments
                .iter()
                .enumerate()
                .fold(context, |context, (index, value)| {
                    context.with_named_value((index + 1).to_string(), value.clone())
                })
        };
        Ok(render_runtime_template(&request.command, &context, false))
    }

    async fn run_shell_profile(
        &self,
        request: &RunShellRequest,
    ) -> Result<TerminalProfile, RmuxError> {
        let state = self.state.lock().await;
        let (session_name, session_id) = request
            .target
            .as_ref()
            .and_then(|target| {
                state
                    .sessions
                    .session(target.session_name())
                    .map(|session| (Some(target.session_name()), Some(session.id().as_u32())))
            })
            .unwrap_or((None, None));

        let base_environment = request
            .target
            .as_ref()
            .and_then(|target| state.session_base_environment_for_pane_target(target));
        let pane_id = request
            .target
            .as_ref()
            .and_then(|target| pane_id_for_target(&state, target));

        TerminalProfile::for_run_shell_with_base_environment(
            &state.environment,
            &state.options,
            session_name,
            session_id,
            &self.socket_path(),
            base_environment.as_ref(),
            !self.config_loading_active(),
            pane_id,
            request.start_directory.as_deref(),
        )
        .map(|profile| match request.source_depth {
            Some(depth) => profile.with_source_depth(depth),
            None if self.config_loading_active() => profile.with_source_depth(1),
            None => profile,
        })
    }

    async fn if_shell_profile(
        &self,
        request: &IfShellRequest,
    ) -> Result<TerminalProfile, RmuxError> {
        let state = self.state.lock().await;
        let (session_name, session_id) = request
            .target
            .as_ref()
            .and_then(|target| {
                state
                    .sessions
                    .session(target.session_name())
                    .map(|session| (Some(target.session_name()), Some(session.id().as_u32())))
            })
            .unwrap_or((None, None));

        let base_environment = request
            .target
            .as_ref()
            .and_then(|target| base_environment_for_target(&state, target));

        TerminalProfile::for_run_shell_with_base_environment(
            &state.environment,
            &state.options,
            session_name,
            session_id,
            &self.socket_path(),
            base_environment.as_ref(),
            !self.config_loading_active(),
            None,
            request.caller_cwd.as_deref(),
        )
    }

    async fn expand_if_shell_condition(
        &self,
        request: &IfShellRequest,
        client_name: Option<&str>,
    ) -> Result<String, RmuxError> {
        #[cfg(windows)]
        if format_references_pane_pid(Some(&request.condition)) {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
        let fallback_target = if request.target.is_none() {
            self.preferred_session_name().await.ok()
        } else {
            None
        };
        let attached_count = match (&request.target, &fallback_target) {
            (Some(target), _) => self.attached_count(target.session_name()).await,
            (None, Some(session_name)) => self.attached_count(session_name).await,
            (None, None) => 0,
        };

        let socket_path = self.socket_path();
        let state = self.state.lock().await;
        let context = match &request.target {
            Some(target) => format_context_for_target_with_server_values(
                &state,
                target,
                attached_count,
                &socket_path,
            )
            .unwrap_or_else(|_| global_format_context(&state, &socket_path)),
            None => fallback_target
                .as_ref()
                .and_then(|session_name| active_session_target(&state.sessions, session_name))
                .map(|target| {
                    format_context_for_target_with_server_values(
                        &state,
                        &target,
                        attached_count,
                        &socket_path,
                    )
                })
                .transpose()?
                .unwrap_or_else(|| global_format_context(&state, &socket_path)),
        };
        let context = match client_name {
            Some(client_name) => context.with_named_value("client_name", client_name.to_owned()),
            None => context,
        };

        Ok(render_runtime_template(&request.condition, &context, false))
    }

    pub(super) async fn execute_queued_if_shell(
        &self,
        requester_pid: u32,
        command: ParsedIfShellCommand,
        context: &QueueExecutionContext,
        target_binding: Option<&QueueSpecialTargetBinding>,
    ) -> Result<QueueCommandAction, RmuxError> {
        if let Some(target_binding) = target_binding {
            target_binding.require_live_for(self).await?;
        }
        if if_shell_runs_in_background(command.background, command.format_mode) {
            let control_identity = current_control_queue_identity(requester_pid);
            let attach_identity = current_expected_attach_identity();
            let follow_attached_session = attach_identity.is_some()
                && command.target.is_none()
                && !context.uses_explicit_current_target();
            let detached_request_guard = self.begin_detached_request();
            let requester_access_guard = self
                .begin_inherited_detached_requester_access(requester_pid)
                .await;
            let handler = self.clone();
            let command = command.clone();
            let target_binding = target_binding.cloned();
            let context = if follow_attached_session {
                context.clone().following_attached_session()
            } else {
                context.clone()
            }
            .without_mouse_origin();
            let hook_formats = current_hook_formats();
            let hook_execution = current_hook_execution();
            if let Err(error) =
                self.spawn_background_task("rmux-if-shell-queue", move || async move {
                    let task = async move {
                        let _detached_request_guard = detached_request_guard;
                        let _requester_access_guard = requester_access_guard;
                        let _ = handler
                            .execute_queued_if_shell_background(
                                requester_pid,
                                command,
                                context,
                                target_binding,
                            )
                            .await;
                    };
                    with_background_client_identities(
                        control_identity,
                        attach_identity,
                        async move {
                            match hook_execution {
                                Some(execution) => {
                                    with_hook_execution(execution, hook_formats, task).await;
                                }
                                None => task.await,
                            }
                        },
                    )
                    .await;
                })
            {
                return Ok(QueueCommandAction::Normal {
                    output: None,
                    error: Some(error),
                    source_file_error: None,
                    exit_status: None,
                });
            }
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        }

        let effective_target = command
            .target
            .clone()
            .or_else(|| context.current_target.clone());
        let profile = if command.format_mode {
            None
        } else {
            Some(
                self.queued_if_shell_profile(&command, effective_target.as_ref())
                    .await?,
            )
        };
        let expanded_condition = self
            .expand_if_shell_condition(
                &IfShellRequest {
                    condition: command.condition.clone(),
                    format_mode: command.format_mode,
                    then_command: String::new(),
                    else_command: None,
                    target: effective_target,
                    caller_cwd: command.caller_cwd.clone(),
                    background: false,
                },
                context.client_name.as_deref(),
            )
            .await?;

        let condition_is_true = if command.format_mode {
            if_shell_format_condition_is_true(&expanded_condition)
        } else {
            shell_condition_is_true(
                expanded_condition,
                profile
                    .as_ref()
                    .expect("profile exists for shell-mode if-shell"),
                Some(self.shell_processes.clone()),
            )
            .await?
        };

        if let Some(target_binding) = target_binding {
            target_binding.require_live_for(self).await?;
        }

        let branch_target = command.target.clone();
        let selected_commands = if condition_is_true {
            Some(command.then_commands)
        } else {
            command.else_commands
        };
        let Some(selected_commands) = selected_commands else {
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        };

        let branch_context = if let Some(target_binding) = target_binding {
            target_binding.child_context(context)
        } else if branch_target.is_some() {
            context.clone().with_current_target(branch_target)
        } else {
            context.clone()
        };

        Ok(QueueCommandAction::InsertAfter {
            batches: vec![(
                self.resolve_command_list_argument(selected_commands)
                    .await?,
                branch_context,
            )],
            output: None,
            error: None,
            source_file_error: None,
            exit_status: None,
        })
    }

    async fn execute_queued_if_shell_background(
        &self,
        requester_pid: u32,
        command: ParsedIfShellCommand,
        mut context: QueueExecutionContext,
        target_binding: Option<QueueSpecialTargetBinding>,
    ) -> Result<(), RmuxError> {
        if let Some(target_binding) = target_binding.as_ref() {
            target_binding.require_live_for(self).await?;
        }
        let expected_attach = validate_expected_attach_identity(self, requester_pid).await?;
        if context.follows_attached_session() {
            let identity = expected_attach.ok_or_else(|| {
                RmuxError::Server("background command lost its attached client identity".to_owned())
            })?;
            let target = self.followed_attached_pane_target(identity).await?;
            context.rebase_current_target_after_attached_switch(Target::Pane(target));
        }
        let effective_target = command
            .target
            .clone()
            .or_else(|| context.current_target.clone());
        let profile = if command.format_mode {
            None
        } else {
            Some(
                self.queued_if_shell_profile(&command, effective_target.as_ref())
                    .await?,
            )
        };
        let expanded_condition = self
            .expand_if_shell_condition(
                &IfShellRequest {
                    condition: command.condition.clone(),
                    format_mode: command.format_mode,
                    then_command: String::new(),
                    else_command: None,
                    target: effective_target,
                    caller_cwd: command.caller_cwd.clone(),
                    background: false,
                },
                context.client_name.as_deref(),
            )
            .await?;

        let condition_is_true = if command.format_mode {
            if_shell_format_condition_is_true(&expanded_condition)
        } else {
            shell_condition_is_true(
                expanded_condition,
                profile
                    .as_ref()
                    .expect("profile exists for shell-mode if-shell"),
                Some(self.shell_processes.clone()),
            )
            .await?
        };

        if let Some(target_binding) = target_binding.as_ref() {
            target_binding.require_live_for(self).await?;
        }

        let branch_target = command.target.clone();
        let selected_commands = if condition_is_true {
            Some(command.then_commands)
        } else {
            command.else_commands
        };
        let Some(selected_commands) = selected_commands else {
            return Ok(());
        };

        let branch_context = if let Some(target_binding) = target_binding.as_ref() {
            target_binding.child_context(&context)
        } else if branch_target.is_some() {
            context.clone().with_current_target(branch_target)
        } else {
            context.clone()
        };

        let parsed = self
            .resolve_command_list_argument(selected_commands)
            .await?;
        let _ = self
            .execute_parsed_commands(requester_pid, parsed, branch_context)
            .await?;
        Ok(())
    }

    async fn resolve_command_list_argument(
        &self,
        argument: CommandListArgument,
    ) -> Result<ParsedCommands, RmuxError> {
        match argument {
            CommandListArgument::Parsed(commands) => Ok(commands),
            CommandListArgument::String(command) => {
                self.parse_command_string_one_group(&command).await
            }
        }
    }

    async fn queued_if_shell_profile(
        &self,
        command: &ParsedIfShellCommand,
        target: Option<&Target>,
    ) -> Result<TerminalProfile, RmuxError> {
        let state = self.state.lock().await;
        let (session_name, session_id) = target
            .and_then(|target| {
                state
                    .sessions
                    .session(target.session_name())
                    .map(|session| (Some(target.session_name()), Some(session.id().as_u32())))
            })
            .unwrap_or((None, None));

        let base_environment =
            target.and_then(|target| base_environment_for_target(&state, target));

        TerminalProfile::for_run_shell_with_base_environment(
            &state.environment,
            &state.options,
            session_name,
            session_id,
            &self.socket_path(),
            base_environment.as_ref(),
            !self.config_loading_active(),
            None,
            command.caller_cwd.as_deref(),
        )
    }
}

fn parsed_contains_attach_session(parsed: &ParsedCommands) -> bool {
    parsed.commands().iter().any(command_attaches_client)
}

fn command_attaches_client(command: &rmux_core::command_parser::ParsedCommand) -> bool {
    if command.name() == "attach-session" {
        return true;
    }
    // Probed 2026-07-08 against the pinned tmux 3.7b oracle: run-shell -C
    // "new-session" (without -d) fails with "open terminal failed: not a
    // terminal" and creates no session, and the same applies to commands
    // nested in brace bodies.
    if command.name() == "new-session" && !new_session_is_detached(command) {
        return true;
    }
    command.arguments().iter().any(|argument| match argument {
        rmux_core::command_parser::CommandArgument::Commands(nested) => {
            parsed_contains_attach_session(nested)
        }
        rmux_core::command_parser::CommandArgument::String(_) => false,
    })
}

fn new_session_is_detached(command: &rmux_core::command_parser::ParsedCommand) -> bool {
    command
        .arguments()
        .iter()
        .filter_map(rmux_core::command_parser::CommandArgument::as_string)
        .any(|argument| {
            argument.len() >= 2
                && argument.starts_with('-')
                && !argument.starts_with("--")
                && argument
                    .bytes()
                    .skip(1)
                    .all(|byte| byte.is_ascii_alphabetic())
                && argument.bytes().skip(1).any(|byte| byte == b'd')
        })
}

fn pane_id_for_target(
    state: &crate::pane_terminals::HandlerState,
    target: &PaneTarget,
) -> Option<PaneId> {
    state
        .sessions
        .session(target.session_name())?
        .pane_id_in_window(target.window_index(), target.pane_index())
}

fn base_environment_for_target(
    state: &crate::pane_terminals::HandlerState,
    target: &Target,
) -> Option<SessionBaseEnvironment> {
    match target {
        Target::Pane(target) => state.session_base_environment_for_pane_target(target),
        Target::Window(target) => {
            state.session_base_environment_for_window(target.session_name(), target.window_index())
        }
        Target::Session(session_name) => {
            state.session_base_environment_for_active_pane(session_name)
        }
    }
}

fn hook_session_default_target(
    state: &crate::pane_terminals::HandlerState,
    session_name: &SessionName,
) -> Option<Target> {
    active_session_target(&state.sessions, session_name).or_else(|| {
        let session = state.sessions.session(session_name)?;
        session.windows().iter().find_map(|(window_index, window)| {
            window
                .active_pane()
                .or_else(|| window.panes().first())
                .map(|pane| {
                    Target::Pane(PaneTarget::with_window(
                        session_name.clone(),
                        *window_index,
                        pane.index(),
                    ))
                })
        })
    })
}

fn if_shell_format_condition_is_true(value: &str) -> bool {
    is_truthy(value) && !value.starts_with('0')
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
