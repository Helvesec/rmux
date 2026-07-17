use rmux_core::command_parser::CommandParser;
use rmux_proto::OptionName;

use crate::pane_terminals::HandlerState;

pub(in crate::handler) fn command_parser_from_state(state: &HandlerState) -> CommandParser {
    let parser = CommandParser::new().with_environment_store(&state.environment);

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
