//! Negotiated, file-backed source execution with one finished input channel.
#[cfg(test)]
#[path = "source_input_admission_tests.rs"]
mod tests;
use super::RequestHandler;
use rmux_core::command_parser::CommandArgument;
use rmux_proto::{
    ErrorResponse, Response, RmuxError, SourceFileInputV1Request, SourceFileRequest,
    INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH, SOURCE_FILE_INPUT_V1_REQUIRED_CAPABILITIES,
};

impl RequestHandler {
    pub(in crate::handler) async fn handle_source_file_input_v1(
        &self,
        pid: u32,
        offer: SourceFileInputV1Request,
    ) -> Response {
        match self.execute_source_file_input_v1(pid, offer).await {
            Ok(response) => response,
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    async fn execute_source_file_input_v1(
        &self,
        pid: u32,
        offer: SourceFileInputV1Request,
    ) -> Result<Response, RmuxError> {
        super::parser_context::validate_input_offer(
            &offer.handshake,
            SOURCE_FILE_INPUT_V1_REQUIRED_CAPABILITIES,
            "source",
        )?;
        if offer.requests_input_relay() && !offer.requests_input_pull() {
            return Err(RmuxError::Server(
                "source-file stdin relay requires an on-demand offer".into(),
            ));
        }
        let input = if offer.requests_input_pull() {
            if offer.stdin_payload.is_some() {
                return Err(RmuxError::Server(
                    "source-file cannot offer both finished and on-demand input".into(),
                ));
            }
            Some(super::invocation_input::InvocationInput::OnDemand(
                crate::listener::source_input::SourceStdin::from_offer()?,
            ))
        } else {
            offer
                .stdin_payload
                .map(super::invocation_input::InvocationInput::Finished)
        };
        let parsed = self.parse_input_offer(&offer.arguments).await?;
        let [command] = parsed.commands() else {
            return Err(RmuxError::Server(
                "source input requires one source-file command".into(),
            ));
        };
        if command.name() != "source-file"
            || !parsed.assignments().is_empty()
            || command
                .arguments()
                .iter()
                .any(|arg| matches!(arg, CommandArgument::Commands(_)))
        {
            return Err(RmuxError::Server(
                "source input requires one file-backed source-file without assignments or sublists"
                    .into(),
            ));
        }
        // Reuse the canonical source entry: it owns inherited source depth,
        // implicit targets, diagnostics, hooks and terminal command status.
        // Its stdin field still contains only source code. The binary channel
        // is installed separately in the context its batches already share.
        let queue_status = offer
            .handshake
            .requires(rmux_proto::CAPABILITY_SOURCE_FILE_QUEUE_STATUS_V1)
            .then(super::source_input_queue_status::SourceInputQueueStatus::new);
        let response = self
            .handle_source_file_with_input(
                pid,
                SourceFileRequest {
                    paths: vec![INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH.to_owned()],
                    quiet: false,
                    parse_only: false,
                    verbose: false,
                    expand_paths: false,
                    target: None,
                    caller_cwd: offer.caller_cwd,
                    stdin: Some(parsed.to_tmux_reparse_string()),
                },
                Some(super::source_runtime::FileSourceOffer {
                    input,
                    queue_status: queue_status.clone(),
                }),
            )
            .await;
        Ok(match (queue_status, response) {
            (Some(status), Response::SourceFile(source)) => {
                Response::SourceFileQueueV1(Box::new(rmux_proto::SourceFileQueueV1Response {
                    stop_queue: status.stop_queue(),
                    source,
                }))
            }
            (_, response) => response,
        })
    }
}
