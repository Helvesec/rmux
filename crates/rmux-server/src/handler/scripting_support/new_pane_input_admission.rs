//! `new-pane -I` admission: one command, its finished payload, one execution.
use rmux_core::command_parser::{CommandArgument, SOURCE_FILE_MAX_COMMAND_BYTES};
use rmux_proto::{
    NewPaneInputV1Request, NewPaneInputV1Response, Response, RmuxError,
    NEW_PANE_INPUT_V1_REQUIRED_CAPABILITIES,
};

use super::parser_context::command_parser_from_state;
use super::queue::{QueueCommandAction, QueueExecutionContext, QueueMode};
use super::{RequestHandler, QUEUED_COMMAND_CONTEXT};

impl RequestHandler {
    pub(in crate::handler) async fn handle_new_pane_input_v1(
        &self,
        requester_pid: u32,
        offer: NewPaneInputV1Request,
    ) -> Response {
        let result = match self.admit_new_pane_input_v1(requester_pid, offer).await {
            Ok(result) => result,
            Err(error) => NewPaneInputV1Response::NotAdmitted { error },
        };
        Response::NewPaneInputV1(Box::new(result))
    }

    async fn admit_new_pane_input_v1(
        &self,
        requester_pid: u32,
        offer: NewPaneInputV1Request,
    ) -> Result<NewPaneInputV1Response, RmuxError> {
        offer
            .handshake
            .validate_against(&super::super::dispatch_support::supported_capabilities())?;
        // The offer uses all three and none implies another, so the peer has to
        // have asked for all three: the input capability itself, the runtime
        // execution this admission performs, and the explicit floating pane it
        // creates. A missing one is refused here, before anything is parsed.
        for required in NEW_PANE_INPUT_V1_REQUIRED_CAPABILITIES {
            if !offer
                .handshake
                .required_capabilities
                .iter()
                .any(|capability| capability == required)
            {
                return Err(RmuxError::Server(format!(
                    "new-pane input offer does not require {required}"
                )));
            }
        }
        let parsed = {
            let state = self.state.lock().await;
            command_parser_from_state(&state)
                .with_exact_commands(rmux_core::command_inventory::RMUX_EXTENSION_COMMANDS)
                .with_max_command_bytes(SOURCE_FILE_MAX_COMMAND_BYTES)
                .parse_arguments_with_assignments(&offer.arguments)
                .map_err(super::command_parse_error_to_rmux)?
        };
        let [command] = parsed.commands() else {
            return Err(RmuxError::Server(
                "new-pane input offer requires exactly one command".to_owned(),
            ));
        };
        if command.name() != "new-pane"
            || !parsed.assignments().is_empty()
            || command
                .arguments()
                .iter()
                .any(|argument| matches!(argument, CommandArgument::Commands(_)))
        {
            return Err(RmuxError::Server(
                "new-pane input offer requires a single new-pane command without assignments or \
                 sublists"
                    .to_owned(),
            ));
        }
        let context = QueueExecutionContext::new(offer.caller_cwd).for_direct_cli();
        // The ordinary queue owns the parse, the target, the rights and the
        // identity revalidation at commit; the payload only travels with it.
        // The creation authority of `new-pane` is kept, so this offer is not a
        // second way into the float that would skip it.
        let execution = QUEUED_COMMAND_CONTEXT.scope(
            context.clone(),
            self.execute_queued_command_scoped(
                requester_pid,
                command.clone(),
                &context,
                QueueMode::Detached,
                None,
                offer.stdin_payload,
            ),
        );
        let execution = self
            .with_new_pane_authority(requester_pid, true, execution)
            .await;
        self.publish_applied_window_resizes().await;
        // Past admission every outcome is terminal: no fallback, no replay.
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
                    unreachable!("a single new-pane cannot enqueue a command")
                }
            },
            Err(error) => (Vec::new(), Some(error), None),
        };
        Ok(NewPaneInputV1Response::Complete {
            stdout,
            exit_status: status.unwrap_or(i32::from(error.is_some())),
            error,
        })
    }
}
