use std::time::Duration;

use rmux_core::command_parser::ParsedCommands;
use rmux_core::formats::{is_truthy, FormatContext};
use rmux_proto::{
    CommandOutput, ErrorResponse, IfShellRequest, IfShellResponse, PaneTarget, Response, RmuxError,
    RunShellRequest, RunShellResponse, Target,
};

use super::super::RequestHandler;
use super::command_args::CommandListArgument;
use super::format_context::format_context_for_target;
use super::queue::{QueueCommandAction, QueueExecutionContext};
use super::queue_parse::ParsedIfShellCommand;
use super::runtime::{
    run_shell_foreground, run_shell_status_error, shell_condition_is_true, spawn_background_async,
};
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::terminal::TerminalProfile;

impl RequestHandler {
    pub(in crate::handler) async fn handle_run_shell(&self, request: RunShellRequest) -> Response {
        if request.background {
            let handler = self.clone();
            spawn_background_async("rmux-run-shell", move || async move {
                let _ = handler.run_shell_task(request).await;
            });
            return Response::RunShell(RunShellResponse::background());
        }

        match self.run_shell_task(request).await {
            Ok(Some(output)) => Response::RunShell(RunShellResponse::from_output(output)),
            Ok(None) => Response::RunShell(RunShellResponse::background()),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(in crate::handler) async fn handle_if_shell(
        &self,
        requester_pid: u32,
        request: IfShellRequest,
    ) -> Response {
        if request.background {
            let handler = self.clone();
            spawn_background_async("rmux-if-shell", move || async move {
                let _ = handler.if_shell_task(requester_pid, request).await;
            });
            return Response::IfShell(IfShellResponse::no_output());
        }

        match self.if_shell_task(requester_pid, request).await {
            Ok(Some(output)) if !output.stdout().is_empty() => {
                Response::IfShell(IfShellResponse::from_output(output))
            }
            Ok(_) => Response::IfShell(IfShellResponse::no_output()),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    async fn run_shell_task(
        &self,
        request: RunShellRequest,
    ) -> Result<Option<CommandOutput>, RmuxError> {
        if let Some(delay_seconds) = request.delay_seconds {
            tokio::time::sleep(Duration::from_secs_f64(delay_seconds.as_secs_f64())).await;
        }

        if request.command.is_empty() {
            return Ok(None);
        }

        if request.as_commands {
            let parsed = self
                .parse_command_string_one_group(&request.command)
                .await?;
            let output = self
                .execute_parsed_commands(
                    std::process::id(),
                    parsed,
                    QueueExecutionContext::new(request.start_directory.clone())
                        .with_current_target(request.target.map(Target::Pane)),
                )
                .await?;
            return Ok((!output.stdout().is_empty()).then_some(output));
        }

        let profile = self.run_shell_profile(&request).await?;
        let command = self
            .expand_run_shell_command(&request.command, request.target.as_ref())
            .await?;
        let output = run_shell_foreground(command, &profile, request.show_stderr).await?;
        if !output.status.success() {
            return Err(RmuxError::Server(run_shell_status_error(&output)));
        }

        let mut stdout = output.stdout;
        if request.show_stderr {
            stdout.extend_from_slice(&output.stderr);
        }
        Ok((!stdout.is_empty()).then_some(CommandOutput::from_stdout(stdout)))
    }

    async fn if_shell_task(
        &self,
        requester_pid: u32,
        request: IfShellRequest,
    ) -> Result<Option<CommandOutput>, RmuxError> {
        let expanded_condition = self.expand_if_shell_condition(&request).await?;

        let condition_is_true = if request.format_mode {
            is_truthy(&expanded_condition)
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
                QueueExecutionContext::new(request.caller_cwd).with_current_target(request.target),
            )
            .await?;
        Ok((!output.stdout().is_empty()).then_some(output))
    }

    async fn expand_run_shell_command(
        &self,
        command: &str,
        target: Option<&PaneTarget>,
    ) -> Result<String, RmuxError> {
        let attached_count = if let Some(target) = target {
            self.attached_count(target.session_name()).await
        } else {
            0
        };

        let state = self.state.lock().await;
        let context = match target {
            Some(target) => {
                format_context_for_target(&state, &Target::Pane(target.clone()), attached_count)?
            }
            None => RuntimeFormatContext::new(FormatContext::new()).with_state(&state),
        };
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

        TerminalProfile::for_run_shell(
            &state.environment,
            &state.options,
            session_name,
            session_id,
            &self.socket_path(),
            !self.config_loading_active(),
            request.start_directory.as_deref(),
        )
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

        TerminalProfile::for_run_shell(
            &state.environment,
            &state.options,
            session_name,
            session_id,
            &self.socket_path(),
            !self.config_loading_active(),
            request.caller_cwd.as_deref(),
        )
    }

    async fn expand_if_shell_condition(
        &self,
        request: &IfShellRequest,
    ) -> Result<String, RmuxError> {
        let attached_count = if let Some(target) = &request.target {
            self.attached_count(target.session_name()).await
        } else {
            0
        };

        let state = self.state.lock().await;
        let context = match &request.target {
            Some(target) => format_context_for_target(&state, target, attached_count)?,
            None => RuntimeFormatContext::new(FormatContext::new()).with_state(&state),
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
            let handler = self.clone();
            let command = command.clone();
            let context = context.clone();
            spawn_background_async("rmux-if-shell-queue", move || async move {
                let _ = handler
                    .execute_queued_if_shell_background(requester_pid, command, context)
                    .await;
            });
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
            });
        }

        let profile = if command.format_mode {
            None
        } else {
            Some(self.queued_if_shell_profile(&command).await?)
        };
        let expanded_condition = self
            .expand_if_shell_condition(&IfShellRequest {
                condition: command.condition.clone(),
                format_mode: command.format_mode,
                then_command: String::new(),
                else_command: None,
                target: command
                    .target
                    .clone()
                    .or_else(|| context.current_target.clone()),
                caller_cwd: command.caller_cwd.clone(),
                background: false,
            })
            .await?;

        let condition_is_true = if command.format_mode {
            is_truthy(&expanded_condition)
        } else {
            shell_condition_is_true(
                expanded_condition,
                profile
                    .as_ref()
                    .expect("profile exists for shell-mode if-shell"),
            )
            .await?
        };

        let selected_commands = if condition_is_true {
            Some(command.then_commands)
        } else {
            command.else_commands
        };
        let Some(selected_commands) = selected_commands else {
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
            });
        };

        Ok(QueueCommandAction::InsertAfter {
            batches: vec![(
                self.resolve_command_list_argument(selected_commands)
                    .await?,
                context.clone(),
            )],
            output: None,
            error: None,
        })
    }

    async fn execute_queued_if_shell_background(
        &self,
        requester_pid: u32,
        command: ParsedIfShellCommand,
        context: QueueExecutionContext,
    ) -> Result<(), RmuxError> {
        let profile = if command.format_mode {
            None
        } else {
            Some(self.queued_if_shell_profile(&command).await?)
        };
        let expanded_condition = self
            .expand_if_shell_condition(&IfShellRequest {
                condition: command.condition.clone(),
                format_mode: command.format_mode,
                then_command: String::new(),
                else_command: None,
                target: command
                    .target
                    .clone()
                    .or_else(|| context.current_target.clone()),
                caller_cwd: command.caller_cwd.clone(),
                background: false,
            })
            .await?;

        let condition_is_true = if command.format_mode {
            is_truthy(&expanded_condition)
        } else {
            shell_condition_is_true(
                expanded_condition,
                profile
                    .as_ref()
                    .expect("profile exists for shell-mode if-shell"),
            )
            .await?
        };

        let selected_commands = if condition_is_true {
            Some(command.then_commands)
        } else {
            command.else_commands
        };
        let Some(selected_commands) = selected_commands else {
            return Ok(());
        };

        let parsed = self
            .resolve_command_list_argument(selected_commands)
            .await?;
        let _ = self
            .execute_parsed_commands(requester_pid, parsed, context)
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
    ) -> Result<TerminalProfile, RmuxError> {
        let state = self.state.lock().await;
        let (session_name, session_id) = command
            .target
            .as_ref()
            .and_then(|target| {
                state
                    .sessions
                    .session(target.session_name())
                    .map(|session| (Some(target.session_name()), Some(session.id().as_u32())))
            })
            .unwrap_or((None, None));

        TerminalProfile::for_run_shell(
            &state.environment,
            &state.options,
            session_name,
            session_id,
            &self.socket_path(),
            !self.config_loading_active(),
            command.caller_cwd.as_deref(),
        )
    }
}
