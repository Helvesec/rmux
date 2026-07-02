#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

use rmux_core::{command_parser::ParsedCommands, formats::is_truthy, PaneId};
use rmux_proto::{
    CommandOutput, ErrorResponse, IfShellRequest, IfShellResponse, PaneTarget, Response, RmuxError,
    RunShellRequest, RunShellResponse, SessionName, Target,
};

use super::super::RequestHandler;
use super::command_args::CommandListArgument;
use super::format_context::{format_context_for_target_with_server_values, global_format_context};
use super::queue::{QueueCommandAction, QueueExecutionContext};
use super::queue_parse::ParsedIfShellCommand;
use super::runtime::{
    run_shell_delay_duration, run_shell_foreground, shell_condition_is_true, spawn_background_async,
};
use super::targets::active_session_target;
use crate::format_runtime::render_runtime_template;
use crate::hook_runtime::current_hook_formats;
use crate::terminal::{SessionBaseEnvironment, TerminalProfile};

impl RequestHandler {
    pub(in crate::handler) async fn handle_run_shell(
        &self,
        requester_pid: u32,
        request: RunShellRequest,
    ) -> Response {
        self.handle_run_shell_with_client_name(requester_pid, request, None)
            .await
    }

    pub(in crate::handler) async fn handle_run_shell_with_client_name(
        &self,
        requester_pid: u32,
        request: RunShellRequest,
        client_name: Option<String>,
    ) -> Response {
        if request.background {
            if let Some(delay_seconds) = request.delay_seconds {
                if let Err(error) = run_shell_delay_duration(delay_seconds.as_secs_f64()) {
                    return Response::Error(ErrorResponse { error });
                }
            }
            let can_write = self.requester_can_write(requester_pid).await;
            let detached_request_guard = self.begin_detached_request();
            let requester_access_guard =
                self.begin_detached_requester_access(requester_pid, can_write);
            let handler = self.clone();
            let hook_formats = current_hook_formats();
            let hook_context_active = crate::hook_runtime::hooks_disabled();
            if let Err(error) = spawn_background_async("rmux-run-shell", move || async move {
                let task = async move {
                    let _detached_request_guard = detached_request_guard;
                    let _requester_access_guard = requester_access_guard;
                    let _ = handler
                        .run_shell_task(requester_pid, request, client_name)
                        .await;
                };
                if hook_context_active {
                    crate::hook_runtime::with_hook_execution(hook_formats, task).await;
                } else {
                    task.await;
                }
            }) {
                return Response::Error(ErrorResponse { error });
            }
            return Response::RunShell(RunShellResponse::background());
        }

        match self
            .run_shell_task(requester_pid, request, client_name)
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
        if request.background {
            let can_write = self.requester_can_write(requester_pid).await;
            let detached_request_guard = self.begin_detached_request();
            let requester_access_guard =
                self.begin_detached_requester_access(requester_pid, can_write);
            let handler = self.clone();
            let hook_formats = current_hook_formats();
            let hook_context_active = crate::hook_runtime::hooks_disabled();
            if let Err(error) = spawn_background_async("rmux-if-shell", move || async move {
                let task = async move {
                    let _detached_request_guard = detached_request_guard;
                    let _requester_access_guard = requester_access_guard;
                    let _ = handler
                        .if_shell_task(requester_pid, request, client_name)
                        .await;
                };
                if hook_context_active {
                    crate::hook_runtime::with_hook_execution(hook_formats, task).await;
                } else {
                    task.await;
                }
            }) {
                return Response::Error(ErrorResponse { error });
            }
            return Response::IfShell(IfShellResponse::no_output());
        }

        match self
            .if_shell_task(requester_pid, request, client_name)
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
    ) -> Result<RunShellTaskOutput, RmuxError> {
        if let Some(delay_seconds) = request.delay_seconds {
            tokio::time::sleep(run_shell_delay_duration(delay_seconds.as_secs_f64())?).await;
        }

        if request.command.is_empty() {
            return Ok(RunShellTaskOutput::NoOutput);
        }

        if request.as_commands {
            let parsed = self
                .parse_command_string_one_group(&request.command)
                .await?;
            let current_target = self
                .run_shell_commands_current_target(requester_pid, request.target)
                .await;
            let context = QueueExecutionContext::new(request.start_directory.clone())
                .with_current_target(current_target)
                .with_client_name(client_name.clone());
            let context = match request.source_depth {
                Some(depth) => context.for_sourced_commands(depth, None),
                None => context,
            };
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
        let command = self
            .expand_run_shell_command(
                &request.command,
                request.target.as_ref(),
                client_name.as_deref(),
            )
            .await?;
        let output = run_shell_foreground(command, &profile, request.show_stderr).await?;
        let exit_status = shell_exit_status(&output.status);
        Ok(RunShellTaskOutput::Shell {
            output: None,
            exit_status,
        })
    }

    async fn run_shell_commands_current_target(
        &self,
        requester_pid: u32,
        target: Option<PaneTarget>,
    ) -> Option<Target> {
        if let Some(target) = target {
            return Some(Target::Pane(target));
        }

        let session_name = match self.current_session_candidate(requester_pid).await {
            Some(session_name) => Some(session_name),
            None => self.preferred_session_name().await.ok(),
        }?;
        let state = self.state.lock().await;
        active_session_target(&state.sessions, &session_name)
    }

    async fn if_shell_task(
        &self,
        requester_pid: u32,
        request: IfShellRequest,
        client_name: Option<String>,
    ) -> Result<Option<CommandOutput>, RmuxError> {
        let expanded_condition = self
            .expand_if_shell_condition(&request, client_name.as_deref())
            .await?;

        let condition_is_true = if request.format_mode {
            if_shell_format_condition_is_true(&expanded_condition)
        } else {
            let profile = self.if_shell_profile(&request).await?;
            shell_condition_is_true(expanded_condition, &profile).await?
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
        let output = self
            .execute_parsed_commands(
                requester_pid,
                parsed,
                QueueExecutionContext::new(request.caller_cwd)
                    .with_current_target(request.target)
                    .with_client_name(client_name),
            )
            .await?;
        Ok((!output.stdout().is_empty()).then_some(output))
    }

    async fn expand_run_shell_command(
        &self,
        command: &str,
        target: Option<&PaneTarget>,
        client_name: Option<&str>,
    ) -> Result<String, RmuxError> {
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
            )?,
            None => match hook_formats
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
        Ok(render_runtime_template(command, &context, false))
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
            )?,
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
    ) -> Result<QueueCommandAction, RmuxError> {
        if command.background {
            let can_write = self.requester_can_write(requester_pid).await;
            let detached_request_guard = self.begin_detached_request();
            let requester_access_guard =
                self.begin_detached_requester_access(requester_pid, can_write);
            let handler = self.clone();
            let command = command.clone();
            let context = context.clone();
            let hook_formats = current_hook_formats();
            let hook_context_active = crate::hook_runtime::hooks_disabled();
            if let Err(error) = spawn_background_async("rmux-if-shell-queue", move || async move {
                let task = async move {
                    let _detached_request_guard = detached_request_guard;
                    let _requester_access_guard = requester_access_guard;
                    let _ = handler
                        .execute_queued_if_shell_background(requester_pid, command, context)
                        .await;
                };
                if hook_context_active {
                    crate::hook_runtime::with_hook_execution(hook_formats, task).await;
                } else {
                    task.await;
                }
            }) {
                return Ok(QueueCommandAction::Normal {
                    output: None,
                    error: Some(error),
                    exit_status: None,
                });
            }
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
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
            )
            .await?
        };

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
                exit_status: None,
            });
        };

        let branch_context = if branch_target.is_some() {
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
            exit_status: None,
        })
    }

    async fn execute_queued_if_shell_background(
        &self,
        requester_pid: u32,
        command: ParsedIfShellCommand,
        context: QueueExecutionContext,
    ) -> Result<(), RmuxError> {
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
            )
            .await?
        };

        let branch_target = command.target.clone();
        let selected_commands = if condition_is_true {
            Some(command.then_commands)
        } else {
            command.else_commands
        };
        let Some(selected_commands) = selected_commands else {
            return Ok(());
        };

        let branch_context = if branch_target.is_some() {
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
