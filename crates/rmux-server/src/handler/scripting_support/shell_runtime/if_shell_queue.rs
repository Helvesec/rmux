//! Queued if-shell commands, in the foreground and in the background.

use rmux_core::command_parser::ParsedCommands;
use rmux_proto::{IfShellRequest, RmuxError, Target};

use super::super::command_args::CommandListArgument;
use super::super::queue::{QueueCommandAction, QueueExecutionContext, QueueTerminalClient};
use super::super::queue_parse::ParsedIfShellCommand;
use super::super::queue_special_target::QueueSpecialTargetBinding;
use super::super::runtime::shell_condition_is_true;
use crate::handler::control_support::current_control_queue_identity;
use crate::handler::{
    current_expected_attach_identity, validate_expected_attach_identity, RequestHandler,
};
use crate::hook_runtime::{current_hook_execution, current_hook_formats, with_hook_execution};
use crate::terminal::TerminalProfile;

use super::{
    base_environment_for_target, if_shell_format_condition_is_true, if_shell_runs_in_background,
    with_background_client_identities, IfShellFormatTargetPolicy,
};

impl RequestHandler {
    pub(in crate::handler::scripting_support) async fn execute_queued_if_shell(
        &self,
        requester_pid: u32,
        command: ParsedIfShellCommand,
        context: &QueueExecutionContext,
        target_binding: Option<&QueueSpecialTargetBinding>,
    ) -> Result<QueueCommandAction, RmuxError> {
        if let Some(target_binding) = target_binding {
            target_binding.require_live_for(self).await?;
        }
        let historical_retired =
            target_binding.is_some_and(QueueSpecialTargetBinding::is_historical_retired);
        let historical_profile =
            target_binding.and_then(QueueSpecialTargetBinding::historical_retired_profile);
        if if_shell_runs_in_background(command.background, command.format_mode) {
            let control_identity = current_control_queue_identity(requester_pid);
            let attach_identity = current_expected_attach_identity();
            let follow_attached_session = attach_identity.is_some()
                && command.target.is_none()
                && !context.uses_explicit_current_target()
                && !historical_retired;
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

        let effective_target = if historical_retired {
            None
        } else {
            command
                .target
                .clone()
                .or_else(|| context.current_target.clone())
        };
        let profile = if command.format_mode {
            None
        } else {
            Some(match historical_profile {
                Some(profile) => profile.clone(),
                None => {
                    self.queued_if_shell_profile(&command, effective_target.as_ref())
                        .await?
                }
            })
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
                context.client_loop_snapshots(),
                if historical_retired {
                    IfShellFormatTargetPolicy::RetainedHistorical
                } else {
                    IfShellFormatTargetPolicy::PreferredLive
                },
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
        }
        .for_if_shell_commands();

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
        if context.terminal_client == QueueTerminalClient::NonTerminal {
            context.terminal_client = QueueTerminalClient::Clientless;
        }
        if let Some(target_binding) = target_binding.as_ref() {
            target_binding.require_live_for(self).await?;
        }
        let historical_retired = target_binding
            .as_ref()
            .is_some_and(|binding| binding.is_historical_retired());
        let historical_profile = target_binding
            .as_ref()
            .and_then(|binding| binding.historical_retired_profile());
        let expected_attach = validate_expected_attach_identity(self, requester_pid).await?;
        if context.follows_attached_session() && !historical_retired {
            let identity = expected_attach.ok_or_else(|| {
                RmuxError::Server("background command lost its attached client identity".to_owned())
            })?;
            let target = self.followed_attached_pane_target(identity).await?;
            context.rebase_current_target_after_attached_switch(Target::Pane(target));
        }
        let effective_target = if historical_retired {
            None
        } else {
            command
                .target
                .clone()
                .or_else(|| context.current_target.clone())
        };
        let profile = if command.format_mode {
            None
        } else {
            Some(match historical_profile {
                Some(profile) => profile.clone(),
                None => {
                    self.queued_if_shell_profile(&command, effective_target.as_ref())
                        .await?
                }
            })
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
                context.client_loop_snapshots(),
                if historical_retired {
                    IfShellFormatTargetPolicy::RetainedHistorical
                } else {
                    IfShellFormatTargetPolicy::PreferredLive
                },
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
