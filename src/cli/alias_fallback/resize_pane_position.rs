//! FMP1 is an offered mutation: refusal and transport ambiguity never replay it.

use std::io::Write;
use std::path::Path;

use rmux_client::Connection;
use rmux_core::command_parser::{CommandArgument, ParsedCommand};
use rmux_proto::{
    Response, RuntimeCommandExecutionResponse, RuntimeCommandExecutionV2Request,
    RuntimeCommandIntent,
};

use super::super::ExitFailure;
use crate::cli_args::ResizePaneArgs;

#[cfg(all(test, unix))]
#[path = "resize_pane_position_tests.rs"]
mod tests;

pub(in crate::cli) fn run(socket: &Path, args: ResizePaneArgs) -> Result<i32, ExitFailure> {
    let mut arguments = vec!["resize-pane".to_owned()];
    if let Some(target) = args.target {
        arguments.extend(["-t".to_owned(), target.raw().to_owned()]);
    }
    if let Some(x) = args.position_x {
        arguments.extend(["-X".to_owned(), x.to_string()]);
    }
    if let Some(y) = args.position_y {
        arguments.extend(["-Y".to_owned(), y.to_string()]);
    }
    let mut connection = rmux_client::connect(socket)
        .map_err(|error| ExitFailure::from_client_connect(socket, error))?;
    execute(&mut connection, arguments)
}

pub(in crate::cli) fn batch_error() -> ExitFailure {
    ExitFailure::new(
        1,
        "CLI batches containing resize-pane -X/-Y are not admitted; no commands executed",
    )
}

pub(super) fn requests_position(command: &ParsedCommand) -> bool {
    if command.name() != "resize-pane" {
        return false;
    }
    let Some(spec) = rmux_core::command_inventory::command_short_option_spec(command.name()) else {
        return false;
    };
    let mut index = 0;
    while index < command.arguments().len() {
        let Some(argument) = command.arguments()[index].as_string() else {
            index += 1;
            continue;
        };
        if argument == "--" || argument == "-" || !argument.starts_with('-') {
            break;
        }
        let Some(flags) = argument
            .strip_prefix('-')
            .filter(|flags| !flags.starts_with('-'))
        else {
            break;
        };
        let mut chars = flags.char_indices().peekable();
        while let Some((_, flag)) = chars.next() {
            if matches!(flag, 'X' | 'Y') {
                return true;
            }
            if spec.takes_value(flag) {
                if chars.peek().is_none()
                    && command
                        .arguments()
                        .get(index + 1)
                        .and_then(CommandArgument::as_string)
                        .is_some()
                {
                    index += 1;
                }
                break;
            }
            if !spec.is_boolean(flag) {
                break;
            }
        }
        index += 1;
    }
    false
}

pub(super) fn execute(
    connection: &mut Connection,
    arguments: Vec<String>,
) -> Result<i32, ExitFailure> {
    let offer = RuntimeCommandExecutionV2Request::new(
        arguments,
        std::env::current_dir().ok(),
        RuntimeCommandIntent::MoveFloatingPane,
    );
    match connection
        .runtime_command_execution_v2(offer)
        .map_err(ExitFailure::from_client)?
    {
        Response::RuntimeCommandExecution(result) => match *result {
            RuntimeCommandExecutionResponse::Complete {
                stdout,
                error,
                exit_status,
                ..
            } => {
                std::io::stdout()
                    .lock()
                    .write_all(&stdout)
                    .map_err(|error| ExitFailure::new(1, error.to_string()))?;
                match error {
                    Some(error) => Err(ExitFailure::new(exit_status, error.to_string())),
                    None => Ok(exit_status),
                }
            }
            RuntimeCommandExecutionResponse::Unsupported(error) => {
                Err(ExitFailure::new(1, error.to_string()))
            }
            RuntimeCommandExecutionResponse::Fallback => Err(ExitFailure::new(
                1,
                "resize-pane position offer not admitted; no fallback",
            )),
        },
        Response::Error(error) => Err(ExitFailure::new(1, error.error.to_string())),
        _ => Err(ExitFailure::new(
            1,
            "invalid resize-pane position offer response; no replay",
        )),
    }
}
