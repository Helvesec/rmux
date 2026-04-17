use rmux_core::command_parser::{CommandParser, ParsedCommands};
use rmux_proto::{RmuxError, Target};

use super::super::RequestHandler;
use super::queue::QueueExecutionContext;
use crate::hook_runtime::current_hook_formats;
use crate::terminal::spawn_hook_command;

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
        let parsed = match self
            .parse_hook_command(command, current_target.as_ref())
            .await
        {
            Ok(parsed) => parsed,
            Err(error) => return Err(error),
        };

        self.execute_parsed_commands(
            requester_pid,
            parsed,
            QueueExecutionContext::without_caller_cwd().with_current_target(current_target),
        )
        .await
        .map(|_| ())
    }

    #[allow(dead_code)]
    async fn parse_hook_command(
        &self,
        command: &str,
        _current_target: Option<&Target>,
    ) -> Result<ParsedCommands, RmuxError> {
        let mut parser = {
            let state = self.state.lock().await;
            CommandParser::new().with_environment_store(&state.environment)
        };
        for (name, value) in current_hook_formats() {
            parser = parser.with_format_value(name, value);
        }
        match parser.parse_one_group(command) {
            Ok(parsed) => Ok(parsed),
            Err(error) if error.message().starts_with("unknown command: ") => {
                spawn_hook_command(command.to_owned()).map_err(|error| {
                    RmuxError::Server(format!(
                        "failed to spawn legacy shell hook command: {error}"
                    ))
                })?;
                Ok(ParsedCommands::default())
            }
            Err(error) => Err(super::command_parse_error_to_rmux(error)),
        }
    }
}
