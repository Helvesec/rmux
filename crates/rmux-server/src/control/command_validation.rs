use rmux_core::command_parser::{CommandArgument, ParsedCommand, ParsedCommands};
use rmux_proto::RmuxError;

const CONTROL_COMMAND_MAX_ARGUMENTS: usize = 1000;

pub(super) fn validate_control_command_arguments(
    commands: ParsedCommands,
) -> Result<ParsedCommands, RmuxError> {
    validate_control_commands(&commands)?;
    Ok(commands)
}

fn validate_control_commands(commands: &ParsedCommands) -> Result<(), RmuxError> {
    for command in commands.commands() {
        validate_control_command(command)?;
    }
    Ok(())
}

fn validate_control_command(command: &ParsedCommand) -> Result<(), RmuxError> {
    let argument_count = command.arguments().len();
    if argument_count > CONTROL_COMMAND_MAX_ARGUMENTS {
        return Err(RmuxError::Server(format!(
            "too many arguments: {argument_count} (maximum {CONTROL_COMMAND_MAX_ARGUMENTS})"
        )));
    }

    for argument in command.arguments() {
        if let CommandArgument::Commands(nested) = argument {
            validate_control_commands(nested)?;
        }
    }
    Ok(())
}
