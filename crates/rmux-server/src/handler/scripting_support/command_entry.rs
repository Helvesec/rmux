#[cfg(test)]
use crate::control::QueueExecutionError;

use super::{
    active_session_target, command_parse_error_to_rmux, command_parser_from_state,
    ActiveAttachIdentity, CommandOutput, ControlCommandResult, ParsedCommands,
    QueueExecutionContext, QueueMode, RequestHandler, RmuxError, Target,
};

impl RequestHandler {
    pub(in crate::handler) async fn attached_queue_target_for_registration(
        &self,
        identity: ActiveAttachIdentity,
    ) -> Result<Target, RmuxError> {
        let state = self.state.lock().await;
        let active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                active.id == identity.attach_id()
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| {
                RmuxError::Server(
                    "attached client identity changed before background command execution"
                        .to_owned(),
                )
            })?;
        let session = state
            .sessions
            .session(&active.session_name)
            .filter(|session| session.id() == active.session_id)
            .ok_or_else(|| RmuxError::SessionNotFound(active.session_name.to_string()))?;
        active_session_target(&state.sessions, session.name())
            .ok_or_else(|| RmuxError::Server("attached session has no current target".to_owned()))
    }

    /// Runs `commands` inline on the requester's own attached input path, as a menu item runs,
    /// so a popup they show on that client does not hold the caller; a popup's waiting callers
    /// are exercised through `handle` and `dispatch` instead.
    #[cfg(test)]
    pub(crate) async fn execute_parsed_commands_for_test(
        &self,
        requester_pid: u32,
        commands: ParsedCommands,
    ) -> Result<CommandOutput, RmuxError> {
        crate::handler::attach_support::on_attached_input_path(
            requester_pid,
            self.execute_parsed_commands(
                requester_pid,
                commands,
                QueueExecutionContext::without_caller_cwd(),
            ),
        )
        .await
    }

    pub(in crate::handler) async fn parse_command_string_one_group(
        &self,
        command: &str,
    ) -> Result<ParsedCommands, RmuxError> {
        let state = self.state.lock().await;
        let parser = command_parser_from_state(&state);
        parser
            .parse_one_group(command)
            .map_err(command_parse_error_to_rmux)
    }

    #[async_recursion::async_recursion]
    pub(in crate::handler) async fn execute_parsed_commands(
        &self,
        requester_pid: u32,
        commands: ParsedCommands,
        mut context: QueueExecutionContext,
    ) -> Result<CommandOutput, RmuxError> {
        let result = self
            .execute_command_queue(
                requester_pid,
                commands,
                &mut context,
                QueueMode::Detached,
                None,
            )
            .await;
        match result.error {
            Some(error) => Err(error),
            None => Ok(CommandOutput::from_stdout(result.stdout)),
        }
    }

    pub(crate) async fn execute_control_commands_identity(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        commands: ParsedCommands,
    ) -> ControlCommandResult {
        let mut context = QueueExecutionContext::without_caller_cwd();
        self.execute_command_queue(
            requester_pid,
            commands,
            &mut context,
            QueueMode::Control,
            Some(expected_control_id),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn execute_control_commands(
        &self,
        requester_pid: u32,
        commands: ParsedCommands,
    ) -> ControlCommandResult {
        let expected_control_id = match self.control_queue_client_id(requester_pid).await {
            Ok(control_id) => control_id,
            Err(error) => {
                return ControlCommandResult {
                    stdout: Vec::new(),
                    error: Some(error.clone()),
                    source_file_error: None,
                    execution_errors: vec![QueueExecutionError::Runtime(error)],
                    exit_status: Some(1),
                    server_shutdown_started: false,
                };
            }
        };
        self.execute_control_commands_identity(requester_pid, expected_control_id, commands)
            .await
    }
}
