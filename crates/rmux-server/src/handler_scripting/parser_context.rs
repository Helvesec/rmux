use rmux_core::command_parser::CommandParser;
use rmux_proto::OptionName;

use crate::pane_terminals::HandlerState;

pub(in crate::handler) fn command_parser_from_state(state: &HandlerState) -> CommandParser {
    CommandParser::new()
        .with_environment_store(&state.environment)
        .with_command_aliases(
            state
                .options
                .resolve_array_values(None, OptionName::CommandAlias),
        )
}
