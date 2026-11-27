use super::*;
use crate::transport::TransportClient;
use rmux_proto::{
    encode_frame, FrameDecoder, HandshakeRequest, HandshakeResponse, NewSessionResponse,
    ProcessCommand, CAPABILITY_HANDSHAKE, CAPABILITY_SDK_PROCESS_COMMAND, RMUX_WIRE_VERSION,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn duplicate_race_retries_only_the_typed_duplicate_failure() {
    for duplicate in [true, false] {
        let (stream, mut peer) = tokio::io::duplex(4096);
        let client = TransportClient::spawn(stream);
        let builder = EnsureSession::try_named("alpha").unwrap();
        let server = async {
            assert!(matches!(
                read_request(&mut peer).await,
                Request::HasSession(_)
            ));
            write_response(
                &mut peer,
                &Response::HasSession(rmux_proto::HasSessionResponse { exists: false }),
            )
            .await;
            let Request::NewSessionExt(request) = read_request(&mut peer).await else {
                panic!("new session")
            };
            assert!(!request.attach_if_exists);
            let error = if duplicate {
                rmux_proto::RmuxError::DuplicateSession("alpha".into())
            } else {
                rmux_proto::RmuxError::Server("duplicate session: alpha".into())
            };
            write_response(
                &mut peer,
                &Response::Error(rmux_proto::ErrorResponse { error }),
            )
            .await;
            if duplicate {
                let Request::NewSessionExt(request) = read_request(&mut peer).await else {
                    panic!("retry")
                };
                assert!(request.attach_if_exists);
                write_response(
                    &mut peer,
                    &Response::NewSession(NewSessionResponse {
                        session_name: SessionName::new("alpha").unwrap(),
                        detached: true,
                        output: None,
                    }),
                )
                .await;
            }
        };
        let (result, ()) = tokio::join!(create_or_reuse_session(&client, &builder), server);
        if duplicate {
            assert_eq!(result.unwrap(), (SessionName::new("alpha").unwrap(), false));
        } else {
            assert!(matches!(result.unwrap_err().protocol_source(),
                Some(rmux_proto::RmuxError::Server(message)) if message == "duplicate session: alpha"));
        }
    }
}

#[tokio::test]
async fn ensure_transport_errors_redact_sdk_fields_display_debug_and_diagnostics() {
    use rmux_proto::RmuxError as Proto;
    use std::error::Error;
    let secret = "SDK_SECRET_NAME=hidden-value";
    for raw in [
        Proto::invalid_target(secret, "SDK_SECRET_NAME exposed hidden-value"),
        Proto::Server(secret.into()),
        Proto::Message(secret.into()),
        Proto::InvalidSetOption(secret.into()),
        Proto::Encode(secret.into()),
        Proto::Decode(secret.into()),
        Proto::spawn_failed(secret),
        Proto::UnsupportedCapability {
            feature: "capability.future".into(),
            supported: vec![secret.into()],
        },
    ] {
        let (stream, mut peer) = tokio::io::duplex(4096);
        let client = TransportClient::spawn(stream);
        let builder = EnsureSession::try_named("alpha")
            .unwrap()
            .environment([secret]);
        let server = async {
            assert!(matches!(
                read_request(&mut peer).await,
                Request::NewSessionExt(_)
            ));
            write_response(
                &mut peer,
                &Response::Error(rmux_proto::ErrorResponse { error: raw.clone() }),
            )
            .await;
        };
        let (result, ()) = tokio::join!(create_session(&client, &builder, false), server);
        let error = result.unwrap_err();
        for rendered in [
            error.to_string(),
            format!("{error:?}"),
            format!("{:?}", crate::protocol_diagnostic(&error)),
        ] {
            assert!(!rendered.contains("SDK_SECRET_NAME"), "{rendered}");
            assert!(!rendered.contains("hidden-value"), "{rendered}");
            assert!(
                rendered.contains("[redacted process environment]"),
                "{rendered}"
            );
        }
        if let RmuxError::Protocol { source } = &error {
            assert!(error.source().unwrap().is::<crate::ProtocolError>());
            assert!(!source.to_string().contains("hidden-value"));
            assert_eq!(
                std::mem::discriminant(error.protocol_source().unwrap()),
                std::mem::discriminant(&raw)
            );
            if let Some((value, reason)) = source.invalid_target() {
                assert_eq!(value, "[redacted process environment]");
                assert_eq!(
                    reason,
                    "[redacted process environment] exposed [redacted process environment]"
                );
            }
        } else if matches!(raw, Proto::SpawnFailed { .. }) {
            assert!(matches!(error, RmuxError::SpawnFailed { .. }));
        } else {
            assert_eq!(error.feature(), Some("capability.future"));
        }
    }
}

#[tokio::test]
async fn process_command_session_requires_capability_before_request() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);
    let builder = EnsureSession::try_named("capability-process")
        .expect("valid session")
        .create_only()
        .argv(["sh", "-c", "printf ready"]);

    let client_future = create_session(&client, &builder, false);
    let server_future = async {
        assert_capability_handshake(read_request(&mut server_stream).await);
        write_response(
            &mut server_stream,
            &Response::Handshake(HandshakeResponse {
                wire_version: RMUX_WIRE_VERSION,
                capabilities: vec![CAPABILITY_HANDSHAKE.to_owned()],
            }),
        )
        .await;
    };
    let (result, ()) = tokio::join!(client_future, server_future);

    match result.expect_err("missing process-command capability must fail before new-session") {
        RmuxError::Unsupported { feature, .. } => {
            assert_eq!(feature, CAPABILITY_SDK_PROCESS_COMMAND);
        }
        error => panic!("expected unsupported process-command capability, got {error:?}"),
    }
}

#[tokio::test]
async fn process_command_session_sends_request_after_capability() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);
    let session_name = SessionName::new("capability-process").expect("valid session");
    let builder = EnsureSession::named(session_name.clone())
        .create_only()
        .argv(["sh", "-c", "printf ready"]);

    let client_future = create_session(&client, &builder, false);
    let server_future = async {
        assert_capability_handshake(read_request(&mut server_stream).await);
        write_response(
            &mut server_stream,
            &Response::Handshake(HandshakeResponse {
                wire_version: RMUX_WIRE_VERSION,
                capabilities: vec![
                    CAPABILITY_HANDSHAKE.to_owned(),
                    CAPABILITY_SDK_PROCESS_COMMAND.to_owned(),
                ],
            }),
        )
        .await;

        match read_request(&mut server_stream).await {
            Request::NewSessionExt(request) => {
                assert_eq!(
                    request.process_command,
                    Some(ProcessCommand::Argv(vec![
                        "sh".to_owned(),
                        "-c".to_owned(),
                        "printf ready".to_owned(),
                    ]))
                );
                write_response(
                    &mut server_stream,
                    &Response::NewSession(NewSessionResponse {
                        session_name,
                        detached: true,
                        output: None,
                    }),
                )
                .await;
            }
            request => panic!("expected new-session after capability, got {request:?}"),
        }
    };
    let (result, ()) = tokio::join!(client_future, server_future);

    assert_eq!(
        result.expect("new-session succeeds"),
        SessionName::new("capability-process").expect("valid session")
    );
}

#[test]
fn owned_session_id_parser_accepts_exact_single_line_identity() {
    assert_eq!(
        parse_owned_session_id(b"$42\n").expect("valid owned-session identity"),
        SessionId::new(42)
    );
}

#[test]
fn owned_session_id_parser_rejects_ambiguous_or_malformed_output() {
    for output in [
        b"$42".as_slice(),
        b"$42\r\n".as_slice(),
        b"42\n".as_slice(),
        b"$invalid\n".as_slice(),
        b"$1\n$2\n".as_slice(),
        b"$1\n\n".as_slice(),
        b"\xff\n".as_slice(),
    ] {
        assert!(
            parse_owned_session_id(output).is_err(),
            "unexpected accepted output: {output:?}"
        );
    }
}

fn assert_capability_handshake(request: Request) {
    match request {
        Request::Handshake(HandshakeRequest {
            required_capabilities,
            ..
        }) => assert_eq!(required_capabilities, vec![CAPABILITY_HANDSHAKE.to_owned()]),
        request => panic!("expected capability handshake, got {request:?}"),
    }
}

async fn read_request(stream: &mut tokio::io::DuplexStream) -> Request {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 4096];
    loop {
        if let Some(request) = decoder
            .next_frame::<Request>()
            .expect("request frame decodes")
        {
            return request;
        }
        let read = stream.read(&mut buffer).await.expect("read request bytes");
        assert_ne!(read, 0, "client closed before request arrived");
        decoder.push_bytes(&buffer[..read]);
    }
}

async fn write_response(stream: &mut tokio::io::DuplexStream, response: &Response) {
    let frame = encode_frame(response).expect("response encodes");
    stream.write_all(&frame).await.expect("write response");
    stream.flush().await.expect("flush response");
}
