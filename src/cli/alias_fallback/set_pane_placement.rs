//! FPC1 is an offered mutation: refusal and transport ambiguity never replay it.

use std::io::Write;
use std::path::Path;

use rmux_client::Connection;
use rmux_proto::{
    Response, RuntimeCommandExecutionResponse, RuntimeCommandExecutionV2Request,
    RuntimeCommandIntent,
};

use super::super::ExitFailure;

#[cfg(all(test, unix))]
#[path = "set_pane_placement_tests.rs"]
mod tests;

pub(in crate::cli) fn batch_error() -> ExitFailure {
    ExitFailure::new(
        1,
        "CLI batches containing set-pane-placement are not admitted; no commands executed",
    )
}

pub(in crate::cli) fn run(socket: &Path, arguments: Vec<String>) -> Result<i32, ExitFailure> {
    let mut connection = rmux_client::connect(socket)
        .map_err(|error| ExitFailure::from_client_connect(socket, error))?;
    execute(
        &mut connection,
        std::iter::once("set-pane-placement".to_owned())
            .chain(arguments)
            .collect(),
    )
}

pub(super) fn execute(
    connection: &mut Connection,
    arguments: Vec<String>,
) -> Result<i32, ExitFailure> {
    let offer = RuntimeCommandExecutionV2Request::new(
        arguments,
        std::env::current_dir().ok(),
        RuntimeCommandIntent::SetPanePlacement,
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
                "set-pane-placement offer not admitted; no fallback",
            )),
        },
        Response::Error(error) => Err(ExitFailure::new(1, error.error.to_string())),
        _ => Err(ExitFailure::new(
            1,
            "invalid set-pane-placement offer response; no replay",
        )),
    }
}
