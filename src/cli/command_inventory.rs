#[cfg(test)]
use rmux_core::command_inventory::list_command_names;
use rmux_core::command_inventory::render_list_commands;

#[cfg(test)]
pub(super) use rmux_core::command_inventory::render_list_commands_line;

use super::{write_lines_output, ExitFailure};
#[cfg(test)]
use crate::cli_args::implemented_command_surface;
use crate::cli_args::ListCommandsArgs;

pub(super) fn run_list_commands(args: ListCommandsArgs) -> Result<i32, ExitFailure> {
    let lines = render_list_commands(args.format.as_deref(), args.command.as_deref())
        .map_err(|error| ExitFailure::new(1, error.to_string()))?;
    write_lines_output(&lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_inventory_matches_implemented_cli_surface_order() {
        let expected = implemented_command_surface()
            .iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        let actual = list_command_names().collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }
}
