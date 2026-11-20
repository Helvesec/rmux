//! File-backed source code and finished binary stdin use distinct channels.
use crate::cli::ExitFailure;
use rmux_client::Connection;
use rmux_core::command_parser::{CommandParser, ParsedCommand};
use rmux_proto::{Request, Response, SourceFileInputV1Request, CAPABILITY_SOURCE_FILE_INPUT_V1};
use std::io::{IsTerminal, Read};

pub(super) fn discover(
    connection: &mut Connection,
    canonical: &str,
    original: Vec<String>,
) -> Result<Option<i32>, ExitFailure> {
    let parsed = CommandParser::new()
        .with_command_aliases(std::iter::empty::<String>())
        .with_exact_commands(rmux_core::command_inventory::RMUX_EXTENSION_COMMANDS)
        .parse_one_group(canonical)
        .map_err(|error| ExitFailure::new(1, error.to_string()))?;
    let [command] = parsed.commands() else {
        return Ok(None);
    };
    if command.name() != "source-file"
        || !parsed.assignments().is_empty()
        || !uses_finished_input(command)
        || std::io::stdin().is_terminal()
    {
        return Ok(None);
    }
    if !connection
        .supports_capability(CAPABILITY_SOURCE_FILE_INPUT_V1)
        .map_err(ExitFailure::from_client)?
    {
        return Ok(None);
    }
    execute(connection, original).map(Some)
}

fn uses_finished_input(command: &ParsedCommand) -> bool {
    let Some(arguments) = command
        .arguments()
        .iter()
        .map(|argument| argument.as_string().map(std::ffi::OsString::from))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    let argv = [
        std::ffi::OsString::from("rmux"),
        std::ffi::OsString::from("source-file"),
    ]
    .into_iter()
    .chain(arguments);
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
    arguments: Vec<String>,
) -> Result<i32, ExitFailure> {
    if !connection
        .supports_capability(CAPABILITY_SOURCE_FILE_INPUT_V1)
        .map_err(ExitFailure::from_client)?
    {
        return Err(ExitFailure::new(
            1,
            "source-file input is not supported; no commands executed",
        ));
    }
    let queue_status = connection
        .supports_capability(rmux_proto::CAPABILITY_SOURCE_FILE_QUEUE_STATUS_V1)
        .map_err(ExitFailure::from_client)?;
    let payload = read_finished_input(std::io::stdin().lock())?;
    let mut request =
        SourceFileInputV1Request::new(arguments, std::env::current_dir().ok(), Some(payload));
    if queue_status {
        request = request.requiring_queue_status();
    }
    rmux_proto::encode_frame(&Request::SourceFileInputV1(Box::new(request.clone())))
        .map_err(|error| ExitFailure::new(1, format!("source-file input not sent: {error}")))?;
    let response = connection
        .source_file_input_v1(request)
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

fn read_finished_input(source: impl Read) -> Result<Vec<u8>, ExitFailure> {
    let maximum = rmux_proto::DEFAULT_MAX_DETACHED_FRAME_LENGTH;
    let mut bytes = Vec::new();
    source
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            ExitFailure::new(1, format!("source-file: failed to read stdin: {error}"))
        })?;
    if bytes.len() > maximum {
        return Err(ExitFailure::new(
            1,
            "source-file input exceeds one frame; nothing sent",
        ));
    }
    Ok(bytes)
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
            let parsed = CommandParser::new().parse(command).unwrap();
            assert_eq!(
                uses_finished_input(&parsed.commands()[0]),
                input,
                "{command}"
            );
        }
    }

    #[test]
    fn source_input_reader_preserves_binary_eof_and_refuses_oversize() {
        assert_eq!(
            read_finished_input(&b"\0\xff\x1b\n\r"[..]).unwrap(),
            b"\0\xff\x1b\n\r"
        );
        assert!(read_finished_input(&b""[..]).unwrap().is_empty());
        let bytes = vec![0; rmux_proto::DEFAULT_MAX_DETACHED_FRAME_LENGTH + 1];
        assert!(read_finished_input(bytes.as_slice()).is_err());
    }
}
