use rmux_core::command_parser::{
    CommandEntry, CommandParser, ParsedCommands, SOURCE_FILE_MAX_COMMAND_BYTES,
};
use rmux_proto::{HandshakeRequest, OptionName, RmuxError};

use crate::pane_terminals::HandlerState;

const SERVER_RUNTIME_EXACT_COMMANDS: &[CommandEntry] = &[CommandEntry {
    name: "set-pane-placement",
    alias: None,
}];

pub(in crate::handler) fn command_parser_from_state(state: &HandlerState) -> CommandParser {
    // Server-side command producers share this narrow allowlist. Other RMUX
    // extensions keep their dedicated admission paths and cannot leak here.
    let parser = CommandParser::new()
        .with_exact_commands(SERVER_RUNTIME_EXACT_COMMANDS)
        .with_environment_store(&state.environment);

    #[cfg(windows)]
    let parser = match windows_home_dir(&state.environment) {
        Some(home_dir) => parser.with_home_dir(home_dir),
        None => parser,
    };

    parser.with_command_aliases(
        state
            .options
            .resolve_array_values(None, OptionName::CommandAlias),
    )
}

pub(super) fn validate_input_offer(
    handshake: &HandshakeRequest,
    required: impl IntoIterator<Item = &'static str>,
    command: &str,
) -> Result<(), RmuxError> {
    handshake.validate_against(&super::super::dispatch_support::supported_capabilities())?;
    for id in required {
        if !handshake.requires(id) {
            return Err(RmuxError::Server(format!(
                "{command} input offer does not require {id}"
            )));
        }
    }
    Ok(())
}

impl super::RequestHandler {
    pub(super) async fn parse_input_offer(
        &self,
        arguments: &[String],
    ) -> Result<ParsedCommands, RmuxError> {
        let state = self.state.lock().await;
        command_parser_from_state(&state)
            .with_exact_commands(rmux_core::command_inventory::RMUX_EXTENSION_COMMANDS)
            .with_max_command_bytes(SOURCE_FILE_MAX_COMMAND_BYTES)
            .parse_arguments_with_assignments(arguments)
            .map_err(super::command_parse_error_to_rmux)
    }
}

#[cfg(windows)]
fn windows_home_dir(environment: &rmux_core::EnvironmentStore) -> Option<&str> {
    environment
        .global_value("HOME")
        .filter(|home| !home.is_empty())
        .or_else(|| {
            environment
                .global_value("USERPROFILE")
                .filter(|home| !home.is_empty())
        })
}
