use std::io;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

use super::{broadcast, client_broadcast_initial_batch_size, Input, OwnedInput};
use crate::transport::TransportClient;
use crate::{Pane, PaneId, PaneRef, RmuxEndpoint, RmuxError, SessionName};
use rmux_proto::{
    encode_frame, CommandOutput, FrameDecoder, HandshakeRequest, HandshakeResponse,
    ListPanesResponse, PaneBroadcastInputResponse, PaneBroadcastInputSuccess, PaneInputRequest,
    PaneTarget, PaneTargetRef, Request, Response, SendKeysResponse, CAPABILITY_HANDSHAKE,
    CAPABILITY_SDK_PANE_BROADCAST, CAPABILITY_SDK_PANE_BY_ID,
};

#[test]
fn cloned_fallback_text_shares_its_allocation() {
    let original = OwnedInput::from(Input::Text("large literal"));
    let cloned = original.clone();

    let (OwnedInput::Text(original), OwnedInput::Text(cloned)) = (original, cloned) else {
        panic!("text input must remain text");
    };
    assert!(Arc::ptr_eq(&original, &cloned));
}

#[test]
fn fallback_broadcast_bounds_concurrent_payload_materialization() {
    assert_eq!(client_broadcast_initial_batch_size(0), 0);
    assert_eq!(client_broadcast_initial_batch_size(3), 3);
    assert_eq!(client_broadcast_initial_batch_size(128), 8);
}

#[tokio::test]
async fn broadcast_falls_back_to_client_fanout_when_daemon_batch_is_unsupported() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let transport = TransportClient::spawn(client_stream);
    let session_name = SessionName::new("broadcastfallback").expect("valid session name");
    let pane = Pane::new(
        PaneRef::new(session_name.clone(), 0, 0),
        RmuxEndpoint::Default,
        None,
        transport,
    );
    let broadcast_task =
        tokio::spawn(async move { broadcast(&[pane], Input::Text("printf ok")).await });

    match read_request(&mut server_stream).await {
        Request::Handshake(HandshakeRequest {
            required_capabilities,
            ..
        }) => {
            assert!(required_capabilities
                .iter()
                .any(|capability| capability == CAPABILITY_HANDSHAKE));
            assert!(!required_capabilities
                .iter()
                .any(|capability| capability == CAPABILITY_SDK_PANE_BROADCAST));
        }
        request => panic!("expected broadcast capability handshake, got {request:?}"),
    }
    write_response(
        &mut server_stream,
        Response::Handshake(HandshakeResponse {
            wire_version: rmux_proto::RMUX_WIRE_VERSION,
            capabilities: vec![
                CAPABILITY_HANDSHAKE.to_owned(),
                CAPABILITY_SDK_PANE_BY_ID.to_owned(),
            ],
        }),
    )
    .await;

    match read_request(&mut server_stream).await {
        Request::ListPanes(request) => {
            assert_eq!(request.target, session_name);
            assert_eq!(request.target_window_index, Some(0));
        }
        request => panic!("expected client fallback pane-id lookup, got {request:?}"),
    }
    write_response(
        &mut server_stream,
        Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout("0:0:%1\n"),
        }),
    )
    .await;

    match read_request(&mut server_stream).await {
        Request::ListPanes(request) => {
            assert_eq!(request.target, session_name);
            assert_eq!(request.target_window_index, None);
        }
        request => panic!("expected pinned delivery pane-id lookup, got {request:?}"),
    }
    write_response(
        &mut server_stream,
        Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout("0:0:%1\n"),
        }),
    )
    .await;

    match read_request(&mut server_stream).await {
        Request::PaneInput(PaneInputRequest {
            keys,
            literal,
            target,
        }) => {
            assert_eq!(keys, ["printf ok"]);
            assert!(literal);
            assert_eq!(target, PaneTargetRef::by_id(session_name, PaneId::new(1)));
        }
        request => panic!("expected client-side pane-input fallback, got {request:?}"),
    }
    write_response(
        &mut server_stream,
        Response::SendKeys(SendKeysResponse { key_count: 1 }),
    )
    .await;

    let result = broadcast_task
        .await
        .expect("broadcast task")
        .expect("fallback succeeds");
    assert_eq!(result.len(), 1);
    assert_eq!(result.successes()[0].pane_id(), Some(PaneId::new(1)));
}

#[tokio::test(start_paused = true)]
async fn daemon_broadcast_shares_identity_and_delivery_deadline() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let transport = TransportClient::spawn(client_stream);
    transport
        .cache_capabilities(vec![
            CAPABILITY_HANDSHAKE.to_owned(),
            CAPABILITY_SDK_PANE_BY_ID.to_owned(),
            CAPABILITY_SDK_PANE_BROADCAST.to_owned(),
        ])
        .await;
    let session_name = SessionName::new("broadcastdeadline").expect("valid session name");
    let pane = Pane::new(
        PaneRef::new(session_name.clone(), 0, 0),
        RmuxEndpoint::Default,
        Some(Duration::from_millis(50)),
        transport,
    );
    let server = tokio::spawn(async move {
        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::ListPanes(_)
        ));
        tokio::time::sleep(Duration::from_millis(35)).await;
        write_response(
            &mut server_stream,
            Response::ListPanes(ListPanesResponse {
                output: CommandOutput::from_stdout("0:0:%1\n"),
            }),
        )
        .await;

        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::PaneBroadcastInput(_)
        ));
        tokio::time::sleep(Duration::from_millis(35)).await;
        write_response(
            &mut server_stream,
            Response::PaneBroadcastInput(PaneBroadcastInputResponse {
                key_count: 1,
                successes: vec![PaneBroadcastInputSuccess {
                    target_index: 0,
                    target: PaneTarget::with_window(session_name, 0, 0),
                    pane_id: Some(PaneId::new(1)),
                }],
                failures: Vec::new(),
            }),
        )
        .await;
    });

    let started = Instant::now();
    let error = broadcast(&[pane], Input::Text("hello"))
        .await
        .expect_err("delivery must use the identity lookup's remaining budget");

    assert_eq!(Instant::now() - started, Duration::from_millis(50));
    assert_timed_out(error);
    server.abort();
}

async fn read_request(stream: &mut tokio::io::DuplexStream) -> Request {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 256];

    loop {
        if let Some(request) = decoder
            .next_frame::<Request>()
            .expect("request frame decodes")
        {
            return request;
        }

        let read = stream.read(&mut buffer).await.expect("read request");
        assert_ne!(read, 0, "client closed before request arrived");
        decoder.push_bytes(&buffer[..read]);
    }
}

async fn write_response(stream: &mut tokio::io::DuplexStream, response: Response) {
    let frame = encode_frame(&response).expect("response encodes");
    stream.write_all(&frame).await.expect("write response");
    stream.flush().await.expect("flush response");
}

fn assert_timed_out(error: RmuxError) {
    match error {
        RmuxError::Transport { source, .. } => {
            assert_eq!(source.kind(), io::ErrorKind::TimedOut);
        }
        error => panic!("expected transport timeout, got {error:?}"),
    }
}
