//! Single-command admission; payload travels as data, never command text.
use rmux_core::command_parser::{CommandArgument, SOURCE_FILE_MAX_COMMAND_BYTES};
use rmux_proto::{
    Response, RmuxError, SplitWindowCommandV1Request, SplitWindowCommandV1Response,
    CAPABILITY_SPLIT_WINDOW_KEY_INPUT_V1,
};

use super::parser_context::command_parser_from_state;
use super::queue::{QueueCommandAction, QueueExecutionContext, QueueMode};
use super::{RequestHandler, QUEUED_COMMAND_CONTEXT};

impl RequestHandler {
    pub(in crate::handler) async fn handle_split_window_command_v1(
        &self,
        requester_pid: u32,
        offer: SplitWindowCommandV1Request,
    ) -> Response {
        let result = match self
            .admit_split_window_command_v1(requester_pid, offer)
            .await
        {
            Ok(result) => result,
            Err(error) => SplitWindowCommandV1Response::NotAdmitted { error },
        };
        Response::SplitWindowCommandV1(Box::new(result))
    }

    async fn admit_split_window_command_v1(
        &self,
        requester_pid: u32,
        offer: SplitWindowCommandV1Request,
    ) -> Result<SplitWindowCommandV1Response, RmuxError> {
        offer
            .handshake
            .validate_against(&super::super::dispatch_support::supported_capabilities())?;
        if !offer
            .handshake
            .required_capabilities
            .iter()
            .any(|cap| cap == CAPABILITY_SPLIT_WINDOW_KEY_INPUT_V1)
        {
            return Err(RmuxError::Server(
                "missing split/key input capability".to_owned(),
            ));
        }
        let parsed = {
            let state = self.state.lock().await;
            command_parser_from_state(&state)
                .with_max_command_bytes(SOURCE_FILE_MAX_COMMAND_BYTES)
                .parse_arguments_with_assignments(&offer.arguments)
                .map_err(super::command_parse_error_to_rmux)?
        };
        let [command] = parsed.commands() else {
            return Err(RmuxError::Server(
                "split offer requires exactly one command".to_owned(),
            ));
        };
        if command.name() != "split-window"
            || !parsed.assignments().is_empty()
            || command
                .arguments()
                .iter()
                .any(|arg| matches!(arg, CommandArgument::Commands(_)))
        {
            return Err(RmuxError::Server(
                "split offer requires a single split command without assignments or sublists"
                    .to_owned(),
            ));
        }
        let context = QueueExecutionContext::new(offer.caller_cwd).for_direct_cli();
        // Validate only payload shape here, never execute an alias or mutate an
        // option to discover the entry. Target/permission/identity are validated
        // again at the ordinary queue's commit boundary.
        if let Some(payload) = &offer.stdin_payload {
            let super::queue::QueueInvocation::SplitWindow(mut split) = self
                .runtime_cli_invocation(requester_pid, command.clone(), &context)
                .await?
            else {
                return Err(RmuxError::Server(
                    "split input requires a split command".to_owned(),
                ));
            };
            split.install_stdin_payload(payload.clone())?;
        }
        let execution = QUEUED_COMMAND_CONTEXT
            .scope(
                context.clone(),
                self.execute_queued_command_scoped(
                    requester_pid,
                    command.clone(),
                    &context,
                    QueueMode::Detached,
                    None,
                    offer.stdin_payload,
                ),
            )
            .await;
        self.publish_applied_window_resizes().await;
        // This is the terminal boundary: all errors after admission are Complete.
        let (stdout, error, status) = match execution {
            Ok(execution) => match execution.action {
                QueueCommandAction::Normal {
                    output,
                    error,
                    exit_status,
                    ..
                } => (
                    output
                        .map(|output| output.stdout().to_vec())
                        .unwrap_or_default(),
                    error,
                    exit_status,
                ),
                QueueCommandAction::InsertAfter { .. } => {
                    unreachable!("single split cannot enqueue a command")
                }
            },
            Err(error) => (Vec::new(), Some(error), None),
        };
        Ok(SplitWindowCommandV1Response::Complete {
            stdout,
            exit_status: status.unwrap_or(i32::from(error.is_some())),
            error,
        })
    }
}
