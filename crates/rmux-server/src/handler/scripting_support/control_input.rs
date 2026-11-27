use rmux_core::command_parser::ParsedCommands;
use rmux_proto::RmuxError;

use crate::control::MAX_CONTROL_LINE_BYTES;
use crate::handler::RequestHandler;

use super::{command_parse_error_to_rmux, command_parser_from_state};

impl RequestHandler {
    pub(crate) async fn parse_control_commands(
        &self,
        command: &str,
    ) -> Result<ParsedCommands, RmuxError> {
        let state = self.state.lock().await;
        // Control commands arrive as bounded text lines, rather than argv.
        // Keep the transport's existing bound through lexical admission.
        command_parser_from_state(&state)
            .with_max_command_bytes(MAX_CONTROL_LINE_BYTES)
            .parse_one_group(command)
            .map_err(command_parse_error_to_rmux)
    }
}
