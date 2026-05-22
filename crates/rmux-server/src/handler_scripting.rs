use rmux_core::{
    command_parser::{CommandParseError, CommandParser, ParsedCommand, ParsedCommands},
    command_queue::CommandQueue,
    LifecycleEvent, ENVIRON_HIDDEN,
};
use rmux_proto::request::Request;
use rmux_proto::{
    CommandOutput, ErrorResponse, HookName, NewWindowRequest, PaneTarget, Response, RmuxError,
    ScopeSelector, Target,
};
use std::collections::VecDeque;

use super::RequestHandler;
use crate::control::ControlCommandResult;
use crate::hook_runtime::{capture_inline_hooks, PendingInlineHookFormat};
use crate::pane_terminals::{NewWindowOptions, WindowSpawnOptions};

#[path = "handler_scripting/buffer_parse.rs"]
mod buffer_parse;
#[path = "handler_scripting/client_parse.rs"]
mod client_parse;
#[path = "handler_scripting/command_args.rs"]
mod command_args;
#[path = "handler_scripting/config_parse.rs"]
mod config_parse;
#[path = "handler_scripting/display_parse.rs"]
mod display_parse;
#[path = "handler_scripting/format_context.rs"]
mod format_context;
#[path = "handler_scripting/hook_commands.rs"]
mod hook_commands;
#[path = "handler_scripting/key_parse.rs"]
mod key_parse;
#[path = "handler_scripting/layout_parse.rs"]
mod layout_parse;
#[path = "handler_scripting/list_parse.rs"]
mod list_parse;
#[path = "handler_scripting/mode_parse.rs"]
mod mode_parse;
#[path = "handler_scripting/pane_parse.rs"]
mod pane_parse;
#[path = "handler_scripting/prompt_parse.rs"]
mod prompt_parse;
#[path = "handler_scripting/prompt_runtime.rs"]
mod prompt_runtime;
#[path = "handler_scripting/queue.rs"]
mod queue;
#[path = "handler_scripting/queue_parse.rs"]
mod queue_parse;
#[path = "handler_scripting/request_parse.rs"]
mod request_parse;
#[path = "handler_scripting/runtime.rs"]
mod runtime;
#[path = "handler_scripting/session_parse.rs"]
mod session_parse;
#[path = "handler_scripting/shell_parse.rs"]
mod shell_parse;
#[path = "handler_scripting/shell_runtime.rs"]
mod shell_runtime;
#[path = "handler_scripting/source_files.rs"]
mod source_files;
#[path = "handler_scripting/source_runtime.rs"]
mod source_runtime;
#[path = "handler_scripting/targets.rs"]
mod targets;
#[path = "handler_scripting/tmux_compat.rs"]
mod tmux_compat;
#[path = "handler_scripting/tokens.rs"]
mod tokens;
#[path = "handler_scripting/values.rs"]
mod values;
#[path = "handler_scripting/wait_for_runtime.rs"]
mod wait_for_runtime;
#[path = "handler_scripting/window_parse.rs"]
mod window_parse;

pub(super) use self::format_context::format_context_for_target;
pub(super) use self::prompt_parse::{ParsedPromptHistoryCommand, PromptHistoryAction};
use self::queue::{queue_action_from_response, remove_group_contexts, QueueInvocation, QueueMode};
pub(super) use self::queue::{QueueCommandAction, QueueExecutionContext};
use self::queue_parse::ParsedNewWindowCommand;
use self::request_parse::parse_queue_invocation;
#[cfg(test)]
pub(crate) use self::request_parse::parse_request_from_parts;
pub(super) use self::runtime::spawn_background_async;
use self::targets::{
    implicit_pane_target, implicit_session_name, implicit_split_target, implicit_window_target,
    is_unsupported_named_layout, parse_layout_name, parse_move_window_target,
    parse_new_window_target_argument, parse_pane_target, parse_select_layout_target,
    parse_session_name, parse_split_window_target, parse_target_arg, parse_window_target,
    queue_target_find_context,
};

const SOURCE_FILE_NESTING_LIMIT: usize = 50;

impl RequestHandler {
    #[cfg(test)]
    pub(crate) async fn execute_parsed_commands_for_test(
        &self,
        requester_pid: u32,
        commands: ParsedCommands,
    ) -> Result<CommandOutput, RmuxError> {
        self.execute_parsed_commands(
            requester_pid,
            commands,
            QueueExecutionContext::without_caller_cwd(),
        )
        .await
    }

    pub(super) async fn parse_command_string_one_group(
        &self,
        command: &str,
    ) -> Result<ParsedCommands, RmuxError> {
        let state = self.state.lock().await;
        let parser = CommandParser::new().with_environment_store(&state.environment);
        parser
            .parse_one_group(command)
            .map_err(command_parse_error_to_rmux)
    }

    pub(crate) async fn parse_control_commands(
        &self,
        command: &str,
    ) -> Result<ParsedCommands, RmuxError> {
        self.parse_command_string_one_group(command).await
    }

    #[async_recursion::async_recursion]
    pub(super) async fn execute_parsed_commands(
        &self,
        requester_pid: u32,
        commands: ParsedCommands,
        context: QueueExecutionContext,
    ) -> Result<CommandOutput, RmuxError> {
        let result = self
            .execute_command_queue(requester_pid, commands, context, QueueMode::Detached)
            .await;
        match result.error {
            Some(error) => Err(error),
            None => Ok(CommandOutput::from_stdout(result.stdout)),
        }
    }

    pub(crate) async fn execute_control_commands(
        &self,
        requester_pid: u32,
        commands: ParsedCommands,
    ) -> ControlCommandResult {
        self.execute_command_queue(
            requester_pid,
            commands,
            QueueExecutionContext::without_caller_cwd(),
            QueueMode::Control,
        )
        .await
    }

    #[async_recursion::async_recursion]
    async fn execute_command_queue(
        &self,
        requester_pid: u32,
        commands: ParsedCommands,
        context: QueueExecutionContext,
        mode: QueueMode,
    ) -> ControlCommandResult {
        self.apply_parse_time_assignments(&commands).await;
        let mut queue = CommandQueue::from_parsed(commands);
        let mut contexts = VecDeque::from(vec![context; queue.len()]);
        let mut stdout = Vec::new();
        let mut errors = Vec::new();

        while let Some(item) = queue.pop_front() {
            let item_context = contexts
                .pop_front()
                .expect("queue item context must stay aligned");
            match self
                .execute_queued_command(requester_pid, item.command().clone(), &item_context, mode)
                .await
            {
                Ok(QueueCommandAction::Normal {
                    output: Some(output),
                    error,
                }) => {
                    stdout.extend_from_slice(output.stdout());
                    if let Some(error) = error {
                        errors.push(error);
                    }
                }
                Ok(QueueCommandAction::Normal {
                    output: None,
                    error,
                }) => {
                    if let Some(error) = error {
                        errors.push(error);
                    }
                }
                Ok(QueueCommandAction::InsertAfter {
                    batches,
                    output,
                    error,
                }) => {
                    if let Some(output) = output {
                        stdout.extend_from_slice(output.stdout());
                    }
                    if let Some(error) = error {
                        errors.push(error);
                    }
                    for (commands, context) in batches.into_iter().rev() {
                        self.apply_parse_time_assignments(&commands).await;
                        let inserted = commands.commands().len();
                        queue.insert_after_current(commands);
                        for _ in 0..inserted {
                            contexts.push_front(context.clone());
                        }
                    }
                }
                Err(error) => {
                    errors.push(error);
                    remove_group_contexts(&queue, &mut contexts, item.group());
                    queue.remove_group(item.group());
                }
            }
            let _ = self.request_shutdown_if_pending();
        }

        ControlCommandResult {
            stdout,
            error: aggregate_rmux_errors(errors),
        }
    }

    #[async_recursion::async_recursion]
    async fn execute_queued_command(
        &self,
        requester_pid: u32,
        command: ParsedCommand,
        context: &QueueExecutionContext,
        mode: QueueMode,
    ) -> Result<QueueCommandAction, RmuxError> {
        let command_for_hooks = command.clone();
        let attached_session = self.current_session_candidate(requester_pid).await;
        let invocation = {
            let state = self.state.lock().await;
            let find_context = queue_target_find_context(
                &state.sessions,
                requester_pid,
                attached_session.as_ref(),
                context.current_target.as_ref(),
                context.mouse_target.as_ref(),
            );
            parse_queue_invocation(
                command,
                context.caller_cwd.as_deref(),
                &state.sessions,
                &find_context,
            )
        };
        let invocation = match invocation {
            Ok(invocation) => invocation,
            Err(error) => {
                self.run_command_error_hook_for_parsed_command(
                    requester_pid,
                    &command_for_hooks,
                    context.current_target.clone(),
                    attached_session.as_ref(),
                )
                .await;
                return Err(error);
            }
        };
        let request_invocation = matches!(
            &invocation,
            QueueInvocation::Request(_) | QueueInvocation::NewWindow(_)
        );

        let result = match invocation {
            QueueInvocation::Request(request) => {
                let can_write = self.requester_can_write(requester_pid).await;
                let request = crate::server_access::apply_access_policy(request, can_write)?;
                let request_for_hooks = request.clone();
                let (outcome, inline_hooks) = Box::pin(self.dispatch_captured(
                    requester_pid,
                    u64::from(requester_pid),
                    request,
                ))
                .await;
                self.run_inline_hooks(requester_pid, inline_hooks, Some(&command_for_hooks))
                    .await;
                self.run_request_hooks(
                    requester_pid,
                    &request_for_hooks,
                    &outcome.response,
                    Some(&command_for_hooks),
                )
                .await;
                match mode {
                    QueueMode::Detached => queue_action_from_response(outcome.response),
                    QueueMode::Control => {
                        self.control_queue_action_from_outcome(
                            requester_pid,
                            request_for_hooks,
                            outcome,
                        )
                        .await
                    }
                }
            }
            QueueInvocation::StartServer => Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
            }),
            QueueInvocation::NewWindow(command) => {
                self.execute_queued_new_window(requester_pid, command).await
            }
            QueueInvocation::IfShell(command) => {
                self.execute_queued_if_shell(requester_pid, command, context)
                    .await
            }
            QueueInvocation::SourceFile(command) => {
                self.execute_queued_source_file(requester_pid, command, context)
                    .await
            }
            QueueInvocation::CommandPrompt(command) => {
                self.execute_queued_command_prompt(requester_pid, command, context)
                    .await
            }
            QueueInvocation::ConfirmBefore(command) => {
                self.execute_queued_confirm_before(requester_pid, command, context)
                    .await
            }
            QueueInvocation::ModeTree(command) => {
                self.execute_queued_mode_tree(requester_pid, command, context)
                    .await
            }
            QueueInvocation::Overlay(command) => {
                self.execute_queued_overlay(requester_pid, command, context)
                    .await
            }
            QueueInvocation::PromptHistory(command) => {
                self.execute_queued_prompt_history(command).await
            }
        };

        if result.is_err() && !request_invocation {
            self.run_command_error_hook_for_parsed_command(
                requester_pid,
                &command_for_hooks,
                context.current_target.clone(),
                attached_session.as_ref(),
            )
            .await;
        }

        result
    }

    async fn execute_queued_new_window(
        &self,
        requester_pid: u32,
        command: ParsedNewWindowCommand,
    ) -> Result<QueueCommandAction, RmuxError> {
        let ParsedNewWindowCommand {
            target,
            target_window_index,
            insert_at_target,
            name,
            detached,
            start_directory,
            environment,
            command,
        } = command;

        let can_write = self.requester_can_write(requester_pid).await;
        let request_for_hooks = crate::server_access::apply_access_policy(
            Request::NewWindow(NewWindowRequest {
                target: target.clone(),
                name: name.clone(),
                detached,
                environment: environment.clone(),
                command: command.clone(),
                start_directory: start_directory.clone(),
                target_window_index,
                insert_at_target,
            }),
            can_write,
        )?;

        let socket_path = self.socket_path();
        let process_command = rmux_proto::ProcessCommand::from_legacy_command(command.as_deref());
        let (response, inline_hooks) = capture_inline_hooks(async {
            let response = {
                let mut state = self.state.lock().await;
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
                            environment_overrides: environment.as_deref(),
                            pane_alert_callback: Some(self.pane_alert_callback()),
                            pane_exit_callback: Some(self.pane_exit_callback()),
                        },
                    },
                ) {
                    Ok(response) => Response::NewWindow(response),
                    Err(error) => Response::Error(ErrorResponse { error }),
                }
            };

            if matches!(response, Response::NewWindow(_)) {
                self.sync_session_silence_timers(&target).await;
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
                    self.emit(LifecycleEvent::WindowLinked {
                        session_name: target.clone(),
                        target: Some(success.target.clone()),
                    })
                    .await;
                }
                self.refresh_attached_session(&target).await;
            }

            response
        })
        .await;

        self.run_inline_hooks(requester_pid, inline_hooks, None)
            .await;
        self.run_request_hooks(requester_pid, &request_for_hooks, &response, None)
            .await;

        queue_action_from_response(response)
    }

    async fn apply_parse_time_assignments(&self, commands: &ParsedCommands) {
        if commands.assignments().is_empty() {
            return;
        }

        let mut state = self.state.lock().await;
        for assignment in commands.assignments() {
            state.environment.set_with_flags(
                ScopeSelector::Global,
                assignment.name().to_owned(),
                assignment.value().to_owned(),
                if assignment.hidden() {
                    ENVIRON_HIDDEN
                } else {
                    0
                },
            );
        }
    }
}

fn aggregate_rmux_errors(errors: Vec<RmuxError>) -> Option<RmuxError> {
    match errors.len() {
        0 => None,
        1 => Some(errors.into_iter().next().expect("single error")),
        _ => Some(RmuxError::Server(
            errors
                .into_iter()
                .map(rmux_error_message)
                .collect::<Vec<_>>()
                .join("\n"),
        )),
    }
}

fn rmux_error_message(error: RmuxError) -> String {
    match error {
        RmuxError::Server(message) => message,
        other => other.to_string(),
    }
}

impl RequestHandler {
    async fn control_queue_action_from_outcome(
        &self,
        requester_pid: u32,
        request: Request,
        outcome: crate::pane_io::HandleOutcome,
    ) -> Result<QueueCommandAction, RmuxError> {
        if let Some(_attach) = outcome.attach
            && matches!(
                request,
                Request::AttachSession(_) | Request::AttachSessionExt(_)
            ) {
                let Response::AttachSession(response) = &outcome.response else {
                    return Err(RmuxError::Server(
                        "attach-session upgrade requires an attach-session response".to_owned(),
                    ));
                };
                {
                    let mut state = self.state.lock().await;
                    if let Some(session) = state.sessions.session_mut(&response.session_name) {
                        session.touch_attached();
                    }
                }
                let _ = self
                    .set_control_session(requester_pid, Some(response.session_name.clone()))
                    .await?;
                self.emit_client_attached(requester_pid, response.session_name.clone())
                    .await;
            }

        queue_action_from_response(outcome.response)
    }
}

fn command_parse_error_to_rmux(error: CommandParseError) -> RmuxError {
    RmuxError::Server(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::source_files::{default_config_paths, default_tmux_fallback_paths};
    #[cfg(windows)]
    use super::source_files::{source_inputs_for_path, SourceReadPolicy};
    use crate::test_env::EnvVarGuard;

    #[cfg(unix)]
    #[test]
    fn default_config_paths_use_rmux_locations() {
        let _lock = crate::test_env::lock_blocking();
        let _home = EnvVarGuard::set("HOME", Some("/tmp/rmux-home"));
        let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", Some("/tmp/rmux-xdg"));

        let paths = default_config_paths();

        assert_eq!(
            paths,
            vec![
                "/etc/rmux.conf".to_owned(),
                "/tmp/rmux-home/.rmux.conf".to_owned(),
                "/tmp/rmux-xdg/rmux/rmux.conf".to_owned(),
                "/tmp/rmux-home/.config/rmux/rmux.conf".to_owned(),
            ]
        );
        assert!(
            paths.iter().all(|path| !path.contains("tmux")),
            "default config search path must not include tmux locations: {paths:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tmux_fallback_paths_use_tmux_locations() {
        let _lock = crate::test_env::lock_blocking();
        let _disable = EnvVarGuard::set("RMUX_DISABLE_TMUX_FALLBACK", None);
        let _home = EnvVarGuard::set("HOME", Some("/tmp/rmux-home"));
        let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", Some("/tmp/rmux-xdg"));

        let paths = default_tmux_fallback_paths();

        assert_eq!(
            paths,
            vec![
                "/etc/tmux.conf".to_owned(),
                "/tmp/rmux-home/.tmux.conf".to_owned(),
                "/tmp/rmux-xdg/tmux/tmux.conf".to_owned(),
                "/tmp/rmux-home/.config/tmux/tmux.conf".to_owned(),
            ]
        );
        assert!(
            paths.iter().all(|path| !path.ends_with("rmux.conf")),
            "tmux fallback paths must not include rmux config files: {paths:?}"
        );
    }

    #[test]
    fn tmux_fallback_paths_can_be_disabled_by_env() {
        let _lock = crate::test_env::lock_blocking();
        let _disable = EnvVarGuard::set("RMUX_DISABLE_TMUX_FALLBACK", Some("1"));

        assert!(default_tmux_fallback_paths().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn default_config_paths_use_documented_windows_locations() {
        let _lock = crate::test_env::lock_blocking();
        let _rmux_config = EnvVarGuard::set("RMUX_CONFIG_FILE", Some(r"C:\rmux\custom.conf"));
        let _appdata = EnvVarGuard::set("APPDATA", Some(r"C:\Users\tester\AppData\Roaming"));
        let _userprofile = EnvVarGuard::set("USERPROFILE", Some(r"C:\Users\tester"));
        let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", Some(r"C:\Users\tester\.config"));

        let paths = default_config_paths();

        assert_eq!(
            paths,
            vec![
                path_string(r"C:\Users\tester\.config\rmux\rmux.conf"),
                path_string(r"C:\Users\tester\.rmux.conf"),
                path_string(r"C:\Users\tester\AppData\Roaming\rmux\rmux.conf"),
                path_string(r"C:\rmux\custom.conf"),
            ]
        );
        assert_eq!(
            paths
                .iter()
                .filter(|path| path.contains("rmux.conf"))
                .count(),
            3,
            "Windows search path must not add undocumented rmux.conf locations: {paths:?}"
        );
        assert!(
            paths.iter().all(|path| !path.contains("tmux")),
            "Windows default config search path must not include tmux locations: {paths:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn tmux_fallback_paths_use_documented_windows_tmux_locations() {
        let _lock = crate::test_env::lock_blocking();
        let _disable = EnvVarGuard::set("RMUX_DISABLE_TMUX_FALLBACK", None);
        let _appdata = EnvVarGuard::set("APPDATA", Some(r"C:\Users\tester\AppData\Roaming"));
        let _userprofile = EnvVarGuard::set("USERPROFILE", Some(r"C:\Users\tester"));
        let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", Some(r"C:\Users\tester\.config"));

        let paths = default_tmux_fallback_paths();

        assert_eq!(
            paths,
            vec![
                path_string(r"C:\Users\tester\.config\tmux\tmux.conf"),
                path_string(r"C:\Users\tester\.tmux.conf"),
                path_string(r"C:\Users\tester\AppData\Roaming\tmux\tmux.conf"),
            ]
        );
        assert!(
            paths.iter().all(|path| !path.ends_with("rmux.conf")),
            "tmux fallback paths must not include rmux config files: {paths:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_nul_config_path_is_empty() {
        let inputs = source_inputs_for_path("NUL", None, false, None, SourceReadPolicy::Strict)
            .expect("NUL should behave like an empty config file");
        assert_eq!(inputs.len(), 1);
        assert!(inputs[0].contents.is_empty());

        let inputs = source_inputs_for_path("nul", None, false, None, SourceReadPolicy::Strict)
            .expect("nul should be case-insensitive");
        assert_eq!(inputs.len(), 1);
        assert!(inputs[0].contents.is_empty());
    }

    #[cfg(windows)]
    fn path_string(path: &str) -> String {
        std::path::PathBuf::from(path)
            .to_string_lossy()
            .into_owned()
    }
}
