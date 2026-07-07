use crate::handles::pane::state_events::{
    PaneStateEvent, PaneStateEventStream, PaneStateEventsOptions,
};
use crate::transport::TransportClient;
use crate::{PaneRef, RmuxEndpoint, SessionName};
use rmux_proto::{
    encode_frame, FrameDecoder, HandshakeResponse, PaneId, PaneOptionGetResponse,
    PaneStateClosedReason, PaneStateCursorResponse, PaneStateEventDto, PaneStateSnapshot,
    PaneStateSubscriptionId, Request, Response, SubscribePaneStateResponse, CAPABILITY_HANDSHAKE,
    CAPABILITY_SDK_PANE_OPTIONS, CAPABILITY_SDK_PANE_STATE_EVENTS, RMUX_WIRE_VERSION,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::time::{timeout, Duration};

use super::Pane;

fn alpha() -> SessionName {
    SessionName::new("alpha").expect("valid session name")
}

async fn read_request(stream: &mut tokio::io::DuplexStream) -> Request {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0; 512];
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

async fn write_response(stream: &mut tokio::io::DuplexStream, response: Response) {
    let frame = encode_frame(&response).expect("response encodes");
    stream.write_all(&frame).await.expect("write response");
    stream.flush().await.expect("flush response");
}

async fn answer_handshake(stream: &mut tokio::io::DuplexStream, capabilities: Vec<String>) {
    assert!(matches!(read_request(stream).await, Request::Handshake(_)));
    write_response(
        stream,
        Response::Handshake(HandshakeResponse {
            wire_version: RMUX_WIRE_VERSION,
            capabilities,
        }),
    )
    .await;
}

fn state_event_capabilities() -> Vec<String> {
    vec![
        CAPABILITY_HANDSHAKE.to_owned(),
        CAPABILITY_SDK_PANE_STATE_EVENTS.to_owned(),
    ]
}

#[tokio::test]
async fn next_keeps_waiting_after_empty_long_poll_response() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let (cursor_client_stream, mut cursor_server_stream) = tokio::io::duplex(4096);
    let transport = TransportClient::spawn(client_stream);
    let cursor_transport = TransportClient::spawn(cursor_client_stream);
    let pane = Pane::new(
        PaneRef::in_first_window(alpha(), 0),
        RmuxEndpoint::Default,
        None,
        transport,
    );
    let pane_id = PaneId::new(42);
    let subscription_id = PaneStateSubscriptionId::new(7);

    let server = tokio::spawn(async move {
        answer_handshake(&mut server_stream, state_event_capabilities()).await;
    });

    let cursor_server = tokio::spawn(async move {
        answer_handshake(&mut cursor_server_stream, state_event_capabilities()).await;

        assert!(matches!(
            read_request(&mut cursor_server_stream).await,
            Request::SubscribePaneState(_)
        ));
        write_response(
            &mut cursor_server_stream,
            Response::SubscribePaneState(Box::new(SubscribePaneStateResponse {
                subscription_id,
                pane_id,
                snapshot: PaneStateSnapshot {
                    revision: 0,
                    title: Some("initial".to_owned()),
                    options: Vec::new(),
                    foreground: None,
                },
            })),
        )
        .await;

        match read_request(&mut cursor_server_stream).await {
            Request::PaneStateCursor(request) => {
                assert_eq!(request.subscription_id, subscription_id);
                assert_eq!(request.after_revision, 0);
                assert!(request.wait);
            }
            request => panic!("expected pane-state-cursor, got {request:?}"),
        }
        write_response(
            &mut cursor_server_stream,
            Response::PaneStateCursor(PaneStateCursorResponse {
                subscription_id,
                events: Vec::new(),
                next_revision: 0,
            }),
        )
        .await;

        match read_request(&mut cursor_server_stream).await {
            Request::PaneStateCursor(request) => {
                assert_eq!(request.subscription_id, subscription_id);
                assert_eq!(request.after_revision, 0);
                assert!(request.wait);
            }
            request => panic!("expected second pane-state-cursor, got {request:?}"),
        }
        write_response(
            &mut cursor_server_stream,
            Response::PaneStateCursor(PaneStateCursorResponse {
                subscription_id,
                events: vec![PaneStateEventDto::TitleChanged {
                    revision: 1,
                    pane_id,
                    old_title: "initial".to_owned(),
                    new_title: "next".to_owned(),
                }],
                next_revision: 1,
            }),
        )
        .await;
    });

    let mut stream = PaneStateEventStream::open_with_cursor_transport(
        &pane,
        PaneStateEventsOptions::default(),
        cursor_transport,
    )
    .await
    .expect("stream opens");
    assert!(matches!(
        stream.next().await.expect("snapshot succeeds"),
        Some(PaneStateEvent::Snapshot { revision: 0, .. })
    ));

    let event = timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("empty cursor response must not end the stream")
        .expect("next succeeds");
    assert!(matches!(
        event,
        Some(PaneStateEvent::TitleChanged {
            revision: 1,
            old_title,
            new_title,
            ..
        }) if old_title == "initial" && new_title == "next"
    ));
    server.await.expect("server task succeeds");
    cursor_server.await.expect("cursor server task succeeds");
}

#[tokio::test]
async fn cursor_long_poll_uses_dedicated_transport_without_blocking_main_requests() {
    let (main_client_stream, mut main_server_stream) = tokio::io::duplex(4096);
    let (cursor_client_stream, mut cursor_server_stream) = tokio::io::duplex(4096);
    let main_transport = TransportClient::spawn(main_client_stream);
    let cursor_transport = TransportClient::spawn(cursor_client_stream);
    let pane = Pane::new(
        PaneRef::in_first_window(alpha(), 0),
        RmuxEndpoint::Default,
        None,
        main_transport,
    );
    let pane_id = PaneId::new(42);
    let subscription_id = PaneStateSubscriptionId::new(7);
    let (cursor_seen_tx, cursor_seen_rx) = oneshot::channel();
    let (release_cursor_tx, release_cursor_rx) = oneshot::channel();

    let main_server = tokio::spawn(async move {
        let mut capabilities = state_event_capabilities();
        capabilities.push(CAPABILITY_SDK_PANE_OPTIONS.to_owned());
        answer_handshake(&mut main_server_stream, capabilities).await;

        match read_request(&mut main_server_stream).await {
            Request::PaneOptionGet(request) => {
                assert_eq!(request.name, "@agent.kind");
            }
            request => panic!("expected pane option get on main transport, got {request:?}"),
        }
        write_response(
            &mut main_server_stream,
            Response::PaneOptionGet(PaneOptionGetResponse {
                pane_id,
                name: "@agent.kind".to_owned(),
                value: Some("assistant".to_owned()),
            }),
        )
        .await;
    });

    let cursor_server = tokio::spawn(async move {
        answer_handshake(&mut cursor_server_stream, state_event_capabilities()).await;

        assert!(matches!(
            read_request(&mut cursor_server_stream).await,
            Request::SubscribePaneState(_)
        ));
        write_response(
            &mut cursor_server_stream,
            Response::SubscribePaneState(Box::new(SubscribePaneStateResponse {
                subscription_id,
                pane_id,
                snapshot: PaneStateSnapshot {
                    revision: 0,
                    title: Some("initial".to_owned()),
                    options: Vec::new(),
                    foreground: None,
                },
            })),
        )
        .await;

        match read_request(&mut cursor_server_stream).await {
            Request::PaneStateCursor(request) => {
                assert_eq!(request.subscription_id, subscription_id);
                assert!(request.wait);
            }
            request => {
                panic!("expected pane-state cursor on cursor transport, got {request:?}")
            }
        }
        cursor_seen_tx
            .send(())
            .expect("test receives cursor notice");
        release_cursor_rx
            .await
            .expect("test releases cursor response");
        write_response(
            &mut cursor_server_stream,
            Response::PaneStateCursor(PaneStateCursorResponse {
                subscription_id,
                events: vec![PaneStateEventDto::Closed {
                    revision: 1,
                    pane_id,
                    reason: PaneStateClosedReason::Killed,
                }],
                next_revision: 1,
            }),
        )
        .await;
    });

    let mut stream = PaneStateEventStream::open_with_cursor_transport(
        &pane,
        PaneStateEventsOptions::default(),
        cursor_transport,
    )
    .await
    .expect("stream opens");
    assert!(matches!(
        stream.next().await.expect("snapshot succeeds"),
        Some(PaneStateEvent::Snapshot { revision: 0, .. })
    ));

    let next_event = stream.next();
    tokio::pin!(next_event);
    tokio::select! {
        result = &mut next_event => panic!("cursor response should still be pending: {result:?}"),
        received = cursor_seen_rx => received.expect("cursor poll reaches dedicated server"),
    }

    let option = timeout(Duration::from_millis(250), pane.option("@agent.kind"))
        .await
        .expect("main transport must remain usable while cursor long-poll is pending")
        .expect("option request succeeds");
    assert_eq!(option.as_deref(), Some("assistant"));

    release_cursor_tx
        .send(())
        .expect("cursor server still alive");
    assert!(matches!(
        timeout(Duration::from_secs(1), &mut next_event)
            .await
            .expect("released cursor returns")
            .expect("next succeeds"),
        Some(PaneStateEvent::Closed { revision: 1, .. })
    ));
    main_server.await.expect("main server task succeeds");
    cursor_server.await.expect("cursor server task succeeds");
}

#[tokio::test]
async fn stream_open_refuses_cursor_transport_without_state_event_capability() {
    let (main_client_stream, mut main_server_stream) = tokio::io::duplex(4096);
    let (cursor_client_stream, mut cursor_server_stream) = tokio::io::duplex(4096);
    let main_transport = TransportClient::spawn(main_client_stream);
    let cursor_transport = TransportClient::spawn(cursor_client_stream);
    let pane = Pane::new(
        PaneRef::in_first_window(alpha(), 0),
        RmuxEndpoint::Default,
        None,
        main_transport,
    );

    let main_server = tokio::spawn(async move {
        answer_handshake(&mut main_server_stream, state_event_capabilities()).await;
    });
    let cursor_server = tokio::spawn(async move {
        answer_handshake(
            &mut cursor_server_stream,
            vec![CAPABILITY_HANDSHAKE.to_owned()],
        )
        .await;
    });

    let error = PaneStateEventStream::open_with_cursor_transport(
        &pane,
        PaneStateEventsOptions::default(),
        cursor_transport,
    )
    .await
    .expect_err("cursor transport without state-event capability must be refused");

    assert!(matches!(
        error,
        crate::RmuxError::Unsupported { feature, .. }
            if feature == CAPABILITY_SDK_PANE_STATE_EVENTS
    ));
    main_server.await.expect("main server task succeeds");
    cursor_server.await.expect("cursor server task succeeds");
}
