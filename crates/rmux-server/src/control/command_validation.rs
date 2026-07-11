use rmux_core::command_parser::{CommandArgument, ParsedCommand, ParsedCommands};
use rmux_proto::RmuxError;

const CONTROL_COMMAND_MAX_ARGUMENTS: usize = 1000;
const CONTROL_COMMAND_MAX_COUNT: usize = 1024;

pub(super) fn validate_control_command_arguments(
    commands: ParsedCommands,
) -> Result<ParsedCommands, RmuxError> {
    let mut command_count = 0;
    validate_control_commands(&commands, &mut command_count)?;
    Ok(commands)
}

fn validate_control_commands(
    commands: &ParsedCommands,
    command_count: &mut usize,
) -> Result<(), RmuxError> {
    for command in commands.commands() {
        *command_count = command_count.saturating_add(1);
        if *command_count > CONTROL_COMMAND_MAX_COUNT {
            return Err(RmuxError::Server(format!(
                "too many commands: {command_count} (maximum {CONTROL_COMMAND_MAX_COUNT})"
            )));
        }
        validate_control_command(command, command_count)?;
    }
    Ok(())
}

fn validate_control_command(
    command: &ParsedCommand,
    command_count: &mut usize,
) -> Result<(), RmuxError> {
    let argument_count = command.arguments().len();
    if argument_count > CONTROL_COMMAND_MAX_ARGUMENTS {
        return Err(RmuxError::Server(format!(
            "too many arguments: {argument_count} (maximum {CONTROL_COMMAND_MAX_ARGUMENTS})"
        )));
    }

    for argument in command.arguments() {
        if let CommandArgument::Commands(nested) = argument {
            validate_control_commands(nested, command_count)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rmux_core::command_parser::CommandParser;

    use super::{validate_control_command_arguments, CONTROL_COMMAND_MAX_COUNT};

    #[test]
    fn aggregate_command_limit_counts_nested_lists() {
        let nested = "start-server;".repeat(CONTROL_COMMAND_MAX_COUNT);
        let parsed = CommandParser::new()
            .parse(&format!("if-shell -F 1 {{ {nested} }}"))
            .expect("large nested command list parses");

        let error = validate_control_command_arguments(parsed)
            .expect_err("outer and nested commands must share one aggregate limit");
        assert!(error.to_string().contains("too many commands"), "{error}");
    }
}
