use super::*;
use rmux_proto::{encode_frame, ErrorResponse, FrameDecoder, Request, RmuxError};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::time::Duration;

#[test]
fn fnp1_transport_never_replays_complete_refusal_unknown_peer_or_ambiguous_eof() {
    let complete = RuntimeCommandExecutionResponse::Complete {
        command_name: "new-pane".to_owned(),
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
                feature: rmux_proto::CAPABILITY_NEW_PANE_FLOATING_EXPLICIT_V1.to_owned(),
                supported: vec![],
            }),
        ))),
        Some(Response::Error(ErrorResponse {
            error: RmuxError::Decode(
                "invalid value: integer `135`, expected variant index 0 <= i < 135".to_owned(),
            ),
        })),
        Some(Response::RuntimeCommandExecution(Box::new(
            RuntimeCommandExecutionResponse::Complete {
                command_name: "new-pane".to_owned(),
                stdout: vec![],
                error: Some(RmuxError::Server("failure after admission".to_owned())),
                exit_status: 17,
            },
        ))),
        None,
    ];
    for (index, response) in responses.into_iter().enumerate() {
        let socket = std::env::temp_dir().join(format!("fnp1-{}-{index}.sock", std::process::id()));
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
            assert_eq!(offer.intent, RuntimeCommandIntent::NewFloatingPane);
            assert_eq!(
                offer.handshake.required_capabilities,
                [
                    rmux_proto::CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION,
                    rmux_proto::CAPABILITY_NEW_PANE_FLOATING_EXPLICIT_V1
                ]
            );
            assert_eq!(
                offer.arguments,
                ["newp", "-t", "%1", "-x30", "-y8", "-X3", "-Y2"]
            );
            if let Some(response) = response {
                stream.write_all(&encode_frame(&response).unwrap()).unwrap();
                assert_eq!(stream.read(&mut bytes).unwrap(), 0, "no replay request");
            }
            listener.set_nonblocking(true).unwrap();
            assert!(listener.accept().is_err(), "no fallback connection");
        });
        let mut connection = rmux_client::connect(&socket).unwrap();
        let result = discover(
            &mut connection,
            "new-pane -t %1 -x30 -y8 -X3 -Y2",
            ["newp", "-t", "%1", "-x30", "-y8", "-X3", "-Y2"]
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

/// FSTDIN1 R2 cause A: a user alias must reach its offer, and the server must
/// receive the argv the user actually wrote.
///
/// The alias is expanded by the server's own list, so the CLI only ever sees
/// its name here; deciding the route from that name is what used to fail with
/// `unknown command`.
#[test]
fn fstdin1_an_alias_reaches_its_offer_and_the_server_sees_the_original_argv() {
    let socket = std::env::temp_dir().join(format!("fstdin1-alias-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut decoder = FrameDecoder::new();
        let mut bytes = [0; 8192];
        // A CLI that refuses the alias on its own sends nothing at all, so this
        // returns `None` rather than failing here: the reason belongs to the
        // client's own result, which the test reads below.
        let request = loop {
            if let Some(request) = decoder.next_frame::<Request>().unwrap() {
                break Some(request);
            }
            let count = stream.read(&mut bytes).unwrap();
            if count == 0 {
                break None;
            }
            decoder.push_bytes(&bytes[..count]);
        };
        let request = request?;
        let Request::RuntimeCommandExecutionV2(offer) = request else {
            panic!("unexpected request {request:?}");
        };
        let arguments = offer.arguments.clone();
        let response = Response::RuntimeCommandExecution(Box::new(
            RuntimeCommandExecutionResponse::Complete {
                command_name: "new-pane".to_owned(),
                stdout: Vec::new(),
                error: None,
                exit_status: 0,
            },
        ));
        stream.write_all(&encode_frame(&response).unwrap()).unwrap();
        Some(arguments)
    });
    let mut connection = rmux_client::connect(&socket).unwrap();
    let result = discover(&mut connection, "new-pane -d", vec!["nf".to_owned()]);
    assert!(
        result.is_ok(),
        "the alias route reaches its offer instead of failing in the CLI: {result:?}"
    );
    drop(connection);
    assert_eq!(
        server.join().unwrap().as_deref(),
        Some(&["nf".to_owned()][..]),
        "the server admits the argv the user wrote"
    );
    std::fs::remove_file(socket).unwrap();
}
