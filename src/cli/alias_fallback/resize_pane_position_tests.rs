use super::*;
use rmux_proto::{encode_frame, ErrorResponse, FrameDecoder, Request, RmuxError};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::time::Duration;

#[test]
fn fmp1_offer_never_falls_back_or_replays_after_any_response_boundary() {
    let complete = RuntimeCommandExecutionResponse::Complete {
        command_name: "resize-pane".to_owned(),
        stdout: vec![],
        exit_status: 0,
        error: None,
    };
    let responses = [
        Some(Response::RuntimeCommandExecution(Box::new(complete))),
        Some(Response::RuntimeCommandExecution(Box::new(
            RuntimeCommandExecutionResponse::Fallback,
        ))),
        Some(Response::RuntimeCommandExecution(Box::new(
            RuntimeCommandExecutionResponse::Unsupported(RmuxError::UnsupportedCapability {
                feature: rmux_proto::CAPABILITY_RESIZE_PANE_POSITION_V1.to_owned(),
                supported: vec![],
            }),
        ))),
        Some(Response::Error(ErrorResponse {
            error: RmuxError::Decode("peer rejected the runtime execution offer".to_owned()),
        })),
        Some(Response::RuntimeCommandExecution(Box::new(
            RuntimeCommandExecutionResponse::Complete {
                command_name: "resize-pane".to_owned(),
                stdout: vec![],
                error: Some(RmuxError::Server("failure after admission".to_owned())),
                exit_status: 17,
            },
        ))),
        None,
    ];
    for (index, response) in responses.into_iter().enumerate() {
        let socket =
            std::env::temp_dir().join(format!("fmp1-offer-{}-{index}.sock", std::process::id()));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut decoder = FrameDecoder::new();
            let mut bytes = [0; 8192];
            let request = loop {
                if let Some(request) = decoder.next_frame::<Request>().unwrap() {
                    break request;
                }
                let count = stream.read(&mut bytes).unwrap();
                assert_ne!(count, 0);
                decoder.push_bytes(&bytes[..count]);
            };
            let Request::RuntimeCommandExecutionV2(offer) = request else {
                panic!("unexpected request {request:?}");
            };
            assert_eq!(offer.intent, RuntimeCommandIntent::MoveFloatingPane);
            assert_eq!(
                offer.handshake.required_capabilities,
                [
                    rmux_proto::CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION,
                    rmux_proto::CAPABILITY_RESIZE_PANE_POSITION_V1,
                ]
            );
            assert_eq!(
                offer.arguments,
                ["resizep", "-t", "%1", "-X", "-3", "-Y", "25"]
            );
            if let Some(response) = response {
                stream.write_all(&encode_frame(&response).unwrap()).unwrap();
                assert_eq!(stream.read(&mut bytes).unwrap(), 0, "no replay request");
            }
            listener.set_nonblocking(true).unwrap();
            assert!(listener.accept().is_err(), "no fallback connection");
        });
        let mut connection = rmux_client::connect(&socket).unwrap();
        let result = execute(
            &mut connection,
            ["resizep", "-t", "%1", "-X", "-3", "-Y", "25"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        );
        assert_eq!(result.is_ok(), index == 0);
        drop(connection);
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
    }
}
