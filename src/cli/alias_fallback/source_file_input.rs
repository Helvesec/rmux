//! Offer redirected stdin separately from a file-backed source invocation.
use crate::cli::ExitFailure;
use rmux_client::Connection;
use rmux_core::command_parser::ParsedCommand;
use rmux_proto::{Response, SourceFileInputV1Request, CAPABILITY_SOURCE_FILE_INPUT_V1};
use std::io::IsTerminal;

pub(super) fn discover(
    connection: &mut Connection,
    socket_path: &std::path::Path,
    command: &ParsedCommand,
    original: Vec<String>,
) -> Result<Option<i32>, ExitFailure> {
    if !accepts_binary_stdin(command) || std::io::stdin().is_terminal() {
        return Ok(None);
    }
    if !connection
        .supports_capability(CAPABILITY_SOURCE_FILE_INPUT_V1)
        .map_err(ExitFailure::from_client)?
    {
        return Ok(None);
    }
    execute(connection, socket_path, original).map(Some)
}

fn accepts_binary_stdin(command: &ParsedCommand) -> bool {
    let Some(arguments) = command
        .arguments()
        .iter()
        .map(|arg| arg.as_string())
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    let argv = ["rmux", "source-file"].into_iter().chain(arguments);
    let Some(crate::cli_args::Command::SourceFile(args)) = crate::cli_args::parse(argv)
        .ok()
        .and_then(|parsed| parsed.command)
    else {
        return false;
    };
    !args.parse_only && !args.paths.iter().any(|path| path == "-")
}

pub(in crate::cli) fn execute(
    connection: &mut Connection,
    socket_path: &std::path::Path,
    arguments: Vec<String>,
) -> Result<i32, ExitFailure> {
    let pull = connection
        .supports_capability(rmux_proto::CAPABILITY_SOURCE_FILE_INPUT_PULL_V1)
        .map_err(ExitFailure::from_client)?;
    let queue_status = connection
        .supports_capability(rmux_proto::CAPABILITY_SOURCE_FILE_QUEUE_STATUS_V1)
        .map_err(ExitFailure::from_client)?;
    let cwd = std::env::current_dir().ok();
    let mut request = if pull {
        SourceFileInputV1Request::on_demand(arguments, cwd)
    } else {
        // Preserve source execution and queue status with older peers. They
        // cannot ask for the caller's stdin, so offer an absent channel.
        SourceFileInputV1Request::new(arguments, cwd, None)
    };
    if queue_status {
        request = request.requiring_queue_status();
    }
    let response = crate::source_stdin::execute(connection, socket_path, request)
        .map_err(ExitFailure::from_client)?;
    finish_response(response, queue_status)
}

fn finish_response(response: Response, queue_status: bool) -> Result<i32, ExitFailure> {
    match response {
        Response::SourceFileQueueV1(response) if queue_status => {
            let status = crate::cli::config_commands::finish_source_file_response(
                Response::SourceFile(response.source),
            )?;
            if response.stop_queue {
                // Output was written byte for byte above; stop without printing it twice.
                Err(ExitFailure::new(if status == 0 { 1 } else { status }, ""))
            } else {
                Ok(status)
            }
        }
        Response::SourceFile(_) if !queue_status => {
            // Old peers cannot distinguish selection failures from sourced errors.
            // Keep their existing behavior; never infer disposition from diagnostics.
            crate::cli::config_commands::finish_source_file_response(response)
        }
        Response::Error(_) => crate::cli::config_commands::finish_source_file_response(response),
        _ => Err(ExitFailure::new(
            1,
            "invalid source-file input response; no replay",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiated_selection_stops_but_internal_status_continues() {
        for status in [None, Some(0), Some(1), Some(7)] {
            for stop_queue in [false, true] {
                let response =
                    Response::SourceFileQueueV1(Box::new(rmux_proto::SourceFileQueueV1Response {
                        stop_queue,
                        source: rmux_proto::SourceFileResponse::no_output()
                            .with_exit_status(status),
                    }));
                let result = finish_response(response, true);
                if stop_queue {
                    let error = result.unwrap_err();
                    assert_eq!(error.exit_code(), status.filter(|s| *s != 0).unwrap_or(1));
                    assert!(error.message().is_empty());
                } else {
                    assert_eq!(result.unwrap(), status.unwrap_or(0));
                }
            }
        }
    }

    #[test]
    fn response_shape_must_match_negotiation_and_errors_still_stop() {
        let source = || rmux_proto::SourceFileResponse::no_output().with_exit_status(Some(1));
        assert!(finish_response(Response::SourceFile(source()), true).is_err());
        assert!(finish_response(
            Response::SourceFileQueueV1(Box::new(rmux_proto::SourceFileQueueV1Response {
                stop_queue: false,
                source: source()
            })),
            false
        )
        .is_err());
        assert_eq!(
            finish_response(Response::SourceFile(source()), false).unwrap(),
            1
        );
        for negotiated in [false, true] {
            let error = finish_response(
                Response::Error(rmux_proto::ErrorResponse {
                    error: rmux_proto::RmuxError::Message("refused".into()),
                }),
                negotiated,
            )
            .unwrap_err();
            assert_eq!(error.exit_code(), 1);
            assert_eq!(error.message(), "refused");
        }
    }

    #[test]
    fn source_input_preserves_code_stdin_parse_only_and_target_operands() {
        for (command, input) in [
            ("source-file input.rmux", true),
            ("source-file -qvF -t named.n input.rmux", true),
            ("source-file -t - input.rmux", true),
            ("source-file -n input.rmux", false),
            ("source-file -qn input.rmux", false),
            ("source-file -- -", false),
            ("source-file -", false),
        ] {
            let parsed = rmux_core::command_parser::CommandParser::new()
                .parse(command)
                .unwrap();
            assert_eq!(
                accepts_binary_stdin(&parsed.commands()[0]),
                input,
                "{command}"
            );
        }
    }
}
