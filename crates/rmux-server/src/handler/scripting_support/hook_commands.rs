use std::sync::Arc;

use rmux_core::command_parser::ParsedCommands;
use rmux_proto::{RmuxError, SessionName, Target};

use super::super::lifecycle_support::{LeaseResolution, LifecycleTargetLease};
use super::super::RequestHandler;
use super::format_context::{format_context_for_target_with_server_values, ParseTimeFormats};
use super::parser_context::command_parser_from_state;
use super::queue::QueueExecutionContext;
use crate::hook_runtime::current_hook_formats;
use crate::terminal::{spawn_hook_command_with_profile, TerminalProfile};

impl RequestHandler {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(in crate::handler) async fn execute_hook_command(
        &self,
        requester_pid: u32,
        command: &str,
    ) -> Result<(), RmuxError> {
        self.execute_hook_command_with_context(requester_pid, command, None)
            .await
    }

    pub(in crate::handler) async fn execute_hook_command_with_context(
        &self,
        requester_pid: u32,
        command: &str,
        current_target: Option<Target>,
    ) -> Result<(), RmuxError> {
        self.execute_hook_command_with_target_binding(requester_pid, command, current_target, None)
            .await
    }

    pub(in crate::handler) async fn execute_hook_command_with_target_binding(
        &self,
        requester_pid: u32,
        command: &str,
        current_target: Option<Target>,
        retained_target: Option<Arc<LifecycleTargetLease>>,
    ) -> Result<(), RmuxError> {
        self.execute_hook_command_with_retired_policy(
            requester_pid,
            command,
            current_target,
            retained_target,
            false,
        )
        .await
    }

    pub(in crate::handler) async fn execute_historical_hook_command_with_target_binding(
        &self,
        requester_pid: u32,
        command: &str,
        current_target: Option<Target>,
        retained_target: Option<Arc<LifecycleTargetLease>>,
    ) -> Result<(), RmuxError> {
        self.execute_hook_command_with_retired_policy(
            requester_pid,
            command,
            current_target,
            retained_target,
            true,
        )
        .await
    }

    async fn execute_hook_command_with_retired_policy(
        &self,
        requester_pid: u32,
        command: &str,
        current_target: Option<Target>,
        retained_target: Option<Arc<LifecycleTargetLease>>,
        preserve_historical_work: bool,
    ) -> Result<(), RmuxError> {
        let parsed = self
            .parse_hook_command(command, current_target.as_ref(), retained_target.as_ref())
            .await?;
        let context = QueueExecutionContext::without_caller_cwd();
        let context = if parsed.target_state != ParsedHookTargetState::Live {
            // A retired lifecycle target is a tombstone, not a CMD_FIND_CANFAIL
            // fallback. Keeping it implicit makes target-dependent commands fail
            // closed instead of selecting an unrelated live session, while
            // target-independent commands in later hook entries still run.
            context
                .with_implicit_current_target(parsed.current_target)
                .forbid_missing_current_target_fallback()
        } else {
            context.with_current_target(parsed.current_target)
        };
        let context =
            if preserve_historical_work && parsed.target_state == ParsedHookTargetState::Retired {
                context.with_historical_retired_target_work()
            } else {
                context
            };
        let execution = self.execute_parsed_commands(
            requester_pid,
            parsed.commands,
            context
                .with_retained_lifecycle_target(retained_target)
                .with_mouse_event(super::queued_command_mouse_event()),
        );
        execution.await.map(|_| ())
    }

    #[allow(dead_code)]
    async fn parse_hook_command(
        &self,
        command: &str,
        current_target: Option<&Target>,
        retained_target: Option<&Arc<LifecycleTargetLease>>,
    ) -> Result<ParsedHookCommand, RmuxError> {
        let (mut parser, current_target, target_state) = {
            let socket_path = self.socket_path();
            let state = self.state.lock().await;
            let (parser_target, current_target, target_state) = match retained_target {
                Some(lease) => match lease.resolve(&state) {
                    LeaseResolution::Live(target) => (
                        Some(target.clone()),
                        Some(target),
                        ParsedHookTargetState::Live,
                    ),
                    LeaseResolution::Retired(snapshot) => (
                        None,
                        Some(snapshot.original().clone()),
                        ParsedHookTargetState::Retired,
                    ),
                    LeaseResolution::Replaced => (
                        None,
                        current_target.cloned(),
                        ParsedHookTargetState::Replaced,
                    ),
                },
                None => (
                    current_target.cloned(),
                    current_target.cloned(),
                    ParsedHookTargetState::Live,
                ),
            };
            let mut parser = command_parser_from_state(&state);
            if let Some(target) = parser_target.as_ref() {
                let context =
                    format_context_for_target_with_server_values(&state, target, 0, &socket_path)?;
                parser = ParseTimeFormats::of(&context).seed_parser(parser, command);
            }
            (parser, current_target, target_state)
        };
        for (name, value) in current_hook_formats() {
            parser = parser.with_format_value(name, value);
        }
        match parser.parse_one_group(command) {
            Ok(commands) => Ok(ParsedHookCommand {
                commands,
                current_target,
                target_state,
            }),
            Err(error) if error.message().starts_with("unknown command: ") => {
                let profile = self
                    .hook_shell_profile(current_target.as_ref(), retained_target)
                    .await?;
                spawn_hook_command_with_profile(command.to_owned(), &profile).map_err(|error| {
                    RmuxError::Server(format!(
                        "failed to spawn legacy shell hook command: {error}"
                    ))
                })?;
                Ok(ParsedHookCommand {
                    commands: ParsedCommands::default(),
                    current_target,
                    target_state,
                })
            }
            Err(error) => Err(super::command_parse_error_to_rmux(error)),
        }
    }

    async fn hook_shell_profile(
        &self,
        current_target: Option<&Target>,
        retained_target: Option<&Arc<LifecycleTargetLease>>,
    ) -> Result<TerminalProfile, RmuxError> {
        let state = self.state.lock().await;
        let retained_resolution = retained_target.map(|lease| lease.resolve(&state));
        let resolved_target = match retained_resolution.as_ref() {
            Some(LeaseResolution::Live(target)) => Some(target),
            Some(LeaseResolution::Retired(snapshot)) => {
                if let Some(profile) = snapshot.terminal_profile() {
                    return Ok(profile.clone());
                }
                None
            }
            Some(LeaseResolution::Replaced) => {
                return Err(RmuxError::Server(
                    "lifecycle hook target was replaced before shell execution".to_owned(),
                ));
            }
            None => current_target,
        };
        let session_name = target_session_name(resolved_target);
        let session_id = session_name
            .and_then(|name| state.sessions.session(name))
            .map(|session| session.id().as_u32());
        let (base_environment, pane_id) = match resolved_target {
            Some(Target::Pane(target)) => (
                state.session_base_environment_for_pane_target(target),
                state
                    .sessions
                    .session(target.session_name())
                    .and_then(|session| session.window_at(target.window_index()))
                    .and_then(|window| window.pane(target.pane_index()))
                    .map(rmux_core::Pane::id),
            ),
            _ => (None, None),
        };

        TerminalProfile::for_run_shell_with_base_environment(
            &state.environment,
            &state.options,
            session_name,
            session_id,
            &self.socket_path(),
            base_environment.as_ref(),
            !self.config_loading_active(),
            pane_id,
            None,
        )
    }
}

struct ParsedHookCommand {
    commands: ParsedCommands,
    current_target: Option<Target>,
    target_state: ParsedHookTargetState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsedHookTargetState {
    Live,
    Retired,
    Replaced,
}

fn target_session_name(target: Option<&Target>) -> Option<&SessionName> {
    match target {
        Some(Target::Session(session_name)) => Some(session_name),
        Some(Target::Window(target)) => Some(target.session_name()),
        Some(Target::Pane(target)) => Some(target.session_name()),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use rmux_proto::{NewSessionRequest, Request, SessionName, Target, TerminalSize};

    use super::super::format_context::parse_time_captures_on_this_thread;
    use super::RequestHandler;

    async fn handler_with_session(name: &str) -> (RequestHandler, Target) {
        let handler = RequestHandler::new();
        let session_name = SessionName::new(name).expect("valid session name");
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session_name.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
        );
        (handler, Target::Session(session_name))
    }

    /// Parses `source` as a hook fired on `target` and returns the command
    /// names it yields with the parse-time captures it made on this thread.
    async fn parse_hook(
        handler: &RequestHandler,
        target: &Target,
        source: &str,
    ) -> (Vec<String>, usize) {
        let captures_before = parse_time_captures_on_this_thread();
        let parsed = handler
            .parse_hook_command(source, Some(target), None)
            .await
            .expect("hook command parses");
        let names = parsed
            .commands
            .commands()
            .iter()
            .map(|command| command.name().to_owned())
            .collect();
        (
            names,
            parse_time_captures_on_this_thread() - captures_before,
        )
    }

    #[tokio::test]
    async fn hook_directives_read_the_target_values_captured_at_parse_time() {
        let (handler, target) = handler_with_session("alpha").await;

        assert_eq!(
            parse_hook(
                &handler,
                &target,
                "%if #{==:#{session_name},alpha}\nlist-sessions\n%else\nlist-windows\n%endif",
            )
            .await,
            (vec!["list-sessions".to_owned()], 1)
        );
        assert_eq!(
            parse_hook(
                &handler,
                &target,
                "%if #{==:#{session_name},beta}\nlist-windows\n%elif #{==:#{session_windows},1}\nlist-sessions\n%endif",
            )
            .await,
            (vec!["list-sessions".to_owned()], 1)
        );
    }

    #[tokio::test]
    async fn hook_without_a_directive_skips_the_parse_time_capture() {
        let (handler, target) = handler_with_session("alpha").await;

        assert_eq!(
            parse_hook(
                &handler,
                &target,
                "display-message -p \"#{session_name}\" ; list-sessions",
            )
            .await,
            (
                vec!["display-message".to_owned(), "list-sessions".to_owned()],
                0
            )
        );
    }
}
