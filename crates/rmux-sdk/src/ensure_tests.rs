use super::*;
use crate::transport::TransportClient;
use rmux_proto::{
    encode_frame, FrameDecoder, HandshakeRequest, HandshakeResponse, NewSessionResponse,
    ProcessCommand, CAPABILITY_HANDSHAKE, CAPABILITY_SDK_PROCESS_COMMAND, RMUX_WIRE_VERSION,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
