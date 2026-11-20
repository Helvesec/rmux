use rmux_client::Connection;
use rmux_proto::{
    Response, RuntimeCommandExecutionResponse, RuntimeCommandExecutionV2Request,
    RuntimeCommandIntent,
};

pub(super) fn requires_explicit_admission(canonical: &str) -> Result<bool, String> {
    let parsed = rmux_core::command_parser::CommandParser::new()
        .with_command_aliases(std::iter::empty::<String>())
        .with_exact_commands(rmux_core::command_inventory::RMUX_EXTENSION_COMMANDS)
        .parse_one_group(canonical)
        .map_err(|error| error.to_string())?;
    Ok(parsed.commands().iter().any(|command| {
        command.name() == "new-pane"
            || command.name() == "set-pane-placement"
            || rmux_core::cli_entry::split_command_requires_key_admission(command)
    }))
}

/// V2 requires a supporting server. Every refusal or transport error is terminal;
/// only an explicit pre-effect Fallback response retains the CLI helper path.
pub(super) fn execute(
    connection: &mut Connection,
    arguments: Vec<String>,
) -> Result<RuntimeCommandExecutionResponse, String> {
    let offer = RuntimeCommandExecutionV2Request::new(
        arguments,
        std::env::current_dir().ok(),
        RuntimeCommandIntent::GenericCli,
    );
    match connection
        .runtime_command_execution_v2(offer)
        .map_err(|error| error.to_string())?
    {
        Response::RuntimeCommandExecution(result) => Ok(*result),
        Response::Error(error) => Err(error.error.to_string()),
        _ => Err("protocol error: invalid runtime command execution V2 response".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_conversion_always_uses_the_full_offered_cli() {
        assert!(requires_explicit_admission("set-pane-placement -t %1 -a %0 -h tiled").unwrap());
        assert!(requires_explicit_admission(
            "set-environment -g BEFORE yes ; set-pane-placement floating"
        )
        .unwrap());
        assert!(!requires_explicit_admission("resize-pane -t %1 -x 20").unwrap());
    }
}

#[cfg(all(test, unix))]
mod transport_tests {
    use super::*;
    use rmux_proto::{
        encode_frame, ErrorResponse, FrameDecoder, Request, RmuxError,
        CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION,
    };
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn exchange(response: Response) -> Result<RuntimeCommandExecutionResponse, String> {
        exchange_optional(Some(response))
    }

    fn exchange_optional(
        response: Option<Response>,
    ) -> Result<RuntimeCommandExecutionResponse, String> {
        let path = std::env::temp_dir().join(format!(
            "rmux-r2-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let listener = UnixListener::bind(&path).expect("listener");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut decoder = FrameDecoder::new();
            let mut bytes = [0; 8192];
            let request = loop {
                if let Some(request) = decoder.next_frame::<Request>().unwrap() {
                    break request;
                }
                let n = stream.read(&mut bytes).unwrap();
                assert_ne!(n, 0);
                decoder.push_bytes(&bytes[..n]);
            };
            let Request::RuntimeCommandExecutionV2(offer) = request else {
                panic!("extra handshake instead of command: {request:?}")
            };
            assert_eq!(offer.intent, RuntimeCommandIntent::GenericCli);
            assert_eq!(offer.arguments, ["display-message", "-p", "hello"]);
            assert_eq!(
                offer.handshake.required_capabilities,
                [CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION]
            );
            assert!(decoder.next_frame::<Request>().unwrap().is_none());
            if let Some(response) = response {
                stream.write_all(&encode_frame(&response).unwrap()).unwrap();
                assert_eq!(
                    stream.read(&mut bytes).unwrap(),
                    0,
                    "no replay or extra request"
                );
            }
            drop(stream);
            listener.set_nonblocking(true).unwrap();
            assert!(listener.accept().is_err(), "no legacy connection");
        });
        let mut connection = rmux_client::connect(&path).unwrap();
        let result = execute(
            &mut connection,
            vec![
                "display-message".to_owned(),
                "-p".to_owned(),
                "hello".to_owned(),
            ],
        );
        drop(connection);
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
        result
    }

    #[test]
    fn current_execution_has_one_request_including_negotiation() {
        let result = RuntimeCommandExecutionResponse::Complete {
            command_name: "display-message".to_owned(),
            stdout: b"hello\n".to_vec(),
            error: None,
            exit_status: 0,
        };
        assert_eq!(
            exchange(Response::RuntimeCommandExecution(Box::new(result.clone()))).unwrap(),
            result
        );
    }

    #[test]
    fn execution_payloads_preserve_fallback_errors_stdout_and_status_without_replay() {
        for result in [
            RuntimeCommandExecutionResponse::Fallback,
            RuntimeCommandExecutionResponse::Unsupported(RmuxError::UnsupportedCapability {
                feature: "future.execution".to_owned(),
                supported: vec![],
            }),
            RuntimeCommandExecutionResponse::Complete {
                command_name: "run-shell".to_owned(),
                stdout: vec![0, 255, b'\n'],
                error: Some(RmuxError::Decode("command error".to_owned())),
                exit_status: 17,
            },
        ] {
            assert_eq!(
                exchange(Response::RuntimeCommandExecution(Box::new(result.clone()))).unwrap(),
                result
            );
        }
    }

    #[test]
    fn unknown_v2_tag_is_terminal_without_legacy_connection() {
        assert!(exchange(Response::Error(ErrorResponse {
            error: RmuxError::Decode(
                "invalid value: integer `135`, expected variant index 0 <= i < 135".to_owned(),
            ),
        }))
        .is_err());
    }

    #[test]
    fn unavailable_capability_is_terminal_without_legacy_connection() {
        let result =
            RuntimeCommandExecutionResponse::Unsupported(RmuxError::UnsupportedCapability {
                feature: CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION.to_owned(),
                supported: vec![],
            });
        assert_eq!(
            exchange(Response::RuntimeCommandExecution(Box::new(result.clone()))).unwrap(),
            result
        );
    }

    #[test]
    fn ambiguous_eof_is_terminal_without_a_legacy_connection() {
        assert!(exchange_optional(None).is_err());
    }

    #[test]
    fn corrupt_completed_response_is_terminal_not_a_replay() {
        assert!(exchange(Response::Error(ErrorResponse {
            error: RmuxError::Decode("truncated result after execution".to_owned()),
        }))
        .is_err());
    }
}
