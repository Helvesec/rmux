use std::future::Future;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use rmux_proto::{
    encode_frame, CancelSdkWaitResponse, ErrorResponse, HandshakeRequest, HandshakeResponse,
    HasSessionRequest, HasSessionResponse, ListSessionsRequest, ListSessionsResponse,
    PaneOutputSubscriptionStart, PaneTarget, Request, Response, SdkWaitForOutputRequest,
    SdkWaitForOutputResponse, SdkWaitId, SdkWaitOutcome, SessionName, CAPABILITY_HANDSHAKE,
    CAPABILITY_SDK_PANE_BY_ID,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};

use super::{
    allocate_bounded_atomic_id, mix_sdk_wait_owner_id, ActorMessage, DropGuard, OperationDeadline,
    TransportClient, TransportState,
};
use crate::RmuxError;

fn alpha() -> SessionName {
    SessionName::new("alpha").expect("valid session")
}

fn has_session_request() -> Request {
    Request::HasSession(HasSessionRequest { target: alpha() })
}

fn list_sessions_request() -> Request {
    Request::ListSessions(ListSessionsRequest {
        format: None,
        filter: None,
        sort_order: None,
        reversed: false,
    })
}

fn handshake_request() -> Request {
    Request::Handshake(HandshakeRequest::requiring(["capability.future"]))
}

fn sdk_wait_request(wait_id: SdkWaitId) -> Request {
    Request::SdkWaitForOutput(SdkWaitForOutputRequest {
        owner_id: rmux_proto::SdkWaitOwnerId::new(7),
        wait_id,
        target: PaneTarget::new(alpha(), 0),
        bytes: b"needle".to_vec(),
        start: PaneOutputSubscriptionStart::Now,
    })
}

fn has_session_response(exists: bool) -> Response {
    Response::HasSession(HasSessionResponse { exists })
}

fn list_sessions_response(stdout: &[u8]) -> Response {
    Response::ListSessions(ListSessionsResponse {
        output: rmux_proto::CommandOutput::from_stdout(stdout),
    })
}

fn sdk_wait_response(wait_id: SdkWaitId, outcome: SdkWaitOutcome) -> Response {
    Response::SdkWaitForOutput(SdkWaitForOutputResponse { wait_id, outcome })
}

fn session_not_found_response() -> Response {
    Response::Error(ErrorResponse {
        error: rmux_proto::RmuxError::SessionNotFound("alpha".to_owned()),
    })
}

fn unsupported_capability_response() -> Response {
    Response::Error(ErrorResponse {
        error: rmux_proto::RmuxError::UnsupportedCapability {
            feature: "capability.future".to_owned(),
            supported: vec![rmux_proto::CAPABILITY_HANDSHAKE.to_owned()],
        },
    })
}

fn unsupported_wire_version_response() -> Response {
    Response::Error(ErrorResponse {
        error: rmux_proto::RmuxError::UnsupportedWireVersion {
            got: rmux_proto::RMUX_WIRE_VERSION + 1,
            minimum: rmux_proto::RMUX_WIRE_VERSION,
            maximum: rmux_proto::RMUX_WIRE_VERSION,
        },
    })
}

async fn read_request(stream: &mut tokio::io::DuplexStream) -> Request {
    let mut decoder = rmux_proto::FrameDecoder::new();
    let mut buffer = [0; 256];
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

async fn write_unknown_response_tag(stream: &mut tokio::io::DuplexStream) {
    let payload = 255_u32.to_le_bytes();
    let mut frame = vec![
        rmux_proto::RMUX_FRAME_MAGIC,
        rmux_proto::RMUX_WIRE_VERSION as u8,
    ];
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&payload);
    stream.write_all(&frame).await.expect("write bad frame");
    stream.flush().await.expect("flush bad frame");
}

fn assert_transport_kind(result: crate::Result<Response>, expected: io::ErrorKind) {
    match result.expect_err("request must fail") {
        RmuxError::Transport { source, .. } => assert_eq!(source.kind(), expected),
        error => panic!("expected transport error, got {error:?}"),
    }
}

fn assert_transport_message(
    result: crate::Result<Response>,
    expected: io::ErrorKind,
    expected_message: &str,
) {
    match result.expect_err("request must fail") {
        RmuxError::Transport { source, .. } => {
            assert_eq!(source.kind(), expected);
            assert!(
                source.to_string().contains(expected_message),
                "transport source `{source}` must contain `{expected_message}`"
            );
        }
        error => panic!("expected transport error, got {error:?}"),
    }
}

fn assert_unsupported_feature(result: crate::Result<Response>, expected: &str) {
    match result.expect_err("request must fail") {
        RmuxError::Unsupported { feature, .. } => assert_eq!(feature, expected),
        error => panic!("expected unsupported error, got {error:?}"),
    }
}

#[tokio::test]
async fn sdk_wait_owner_ids_are_distinct_and_wait_ids_are_per_transport() {
    let (first_stream, _first_peer) = tokio::io::duplex(64);
    let (second_stream, _second_peer) = tokio::io::duplex(64);
    let first = TransportClient::spawn(first_stream);
    let second = TransportClient::spawn(second_stream);

    assert_ne!(first.sdk_wait_owner_id(), second.sdk_wait_owner_id());
    assert_eq!(first.allocate_sdk_wait_id(), SdkWaitId::new(1));
    assert_eq!(first.allocate_sdk_wait_id(), SdkWaitId::new(2));
    assert_eq!(second.allocate_sdk_wait_id(), SdkWaitId::new(1));
}

#[test]
fn sdk_wait_owner_ids_mix_process_seed_with_local_counter() {
    assert_ne!(
        mix_sdk_wait_owner_id(0x1111_2222_3333_4444, 1),
        mix_sdk_wait_owner_id(0x5555_6666_7777_8888, 1)
    );
    assert_ne!(
        mix_sdk_wait_owner_id(0x1111_2222_3333_4444, 1),
        mix_sdk_wait_owner_id(0x1111_2222_3333_4444, 2)
    );
    assert_ne!(mix_sdk_wait_owner_id(0, 0), 0);
}

#[test]
fn bounded_sdk_wait_id_allocator_panics_instead_of_wrapping() {
    let counter = AtomicU64::new(2);

    assert_eq!(allocate_bounded_atomic_id(&counter, 2, "exhausted"), 2);
    assert_eq!(counter.load(Ordering::Relaxed), 3);

    let result = catch_unwind(AssertUnwindSafe(|| {
        allocate_bounded_atomic_id(&counter, 2, "exhausted");
    }));
    assert!(result.is_err());
    assert_eq!(counter.load(Ordering::Relaxed), 3);
}

fn spawn_request(
    client: &TransportClient,
    request: Request,
) -> JoinHandle<crate::Result<Response>> {
    let client = client.clone();
    tokio::spawn(async move { client.request(request).await })
}

async fn join_request(handle: JoinHandle<crate::Result<Response>>) -> crate::Result<Response> {
    handle.await.expect("request task must not panic")
}

async fn assert_cancelled_transport_rejects_follow_up(client: &TransportClient) {
    let result = timeout(
        Duration::from_secs(1),
        client.request(list_sessions_request()),
    )
    .await
    .expect("a cancelled ordered request must not leave the following RPC pending");

    assert_transport_message(
        result,
        io::ErrorKind::BrokenPipe,
        "cancelled while its ordered response was pending",
    );
}

#[tokio::test]
async fn dropping_request_before_send_succeeds_keeps_transport_usable() {
    let (commands, mut command_receiver) = tokio::sync::mpsc::channel(1);
    assert!(
        commands
            .try_send(ActorMessage::BestEffort {
                request: has_session_request(),
            })
            .is_ok(),
        "fixture command queue must start full"
    );
    let actor_task = tokio::spawn(std::future::pending::<()>());
    let client = TransportClient {
        commands,
        actor: actor_task.abort_handle(),
        state: Arc::new(TransportState::default()),
        default_timeout: None,
        operation_deadline: None,
        fixture_transport: false,
    };

    let mut request = Box::pin(client.request(list_sessions_request()));
    let mut context = Context::from_waker(Waker::noop());
    assert!(
        request.as_mut().poll(&mut context).is_pending(),
        "request must wait while the actor command queue is full"
    );
    drop(request);

    assert!(
        client.state.terminal_failure().is_none(),
        "cancellation before a successful send must leave the transport healthy"
    );
    assert!(matches!(
        command_receiver.recv().await,
        Some(ActorMessage::BestEffort {
            request: Request::HasSession(_),
        })
    ));

    client.try_send_best_effort(list_sessions_request());
    assert!(matches!(
        command_receiver.recv().await,
        Some(ActorMessage::BestEffort {
            request: Request::ListSessions(_),
        })
    ));

    actor_task.abort();
    assert!(actor_task
        .await
        .expect_err("fixture actor must be cancelled")
        .is_cancelled());
}

#[tokio::test(start_paused = true)]
async fn dropping_sent_request_drains_its_response_and_keeps_transport_usable() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);
    let request = spawn_request(&client, has_session_request());

    assert_eq!(
        read_request(&mut server_stream).await,
        has_session_request()
    );
    request.abort();
    assert!(
        request
            .await
            .expect_err("request task must be cancelled")
            .is_cancelled(),
        "request task must report external cancellation"
    );

    let follow_up = spawn_request(&client, list_sessions_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        list_sessions_request()
    );
    write_response(&mut server_stream, &has_session_response(false)).await;
    write_response(&mut server_stream, &list_sessions_response(b"alpha\n")).await;

    assert_eq!(
        join_request(follow_up).await.expect("follow-up succeeds"),
        list_sessions_response(b"alpha\n")
    );
    assert!(client.state.terminal_failure().is_none());
}

#[tokio::test(start_paused = true)]
async fn dropping_sent_armed_request_invalidates_the_ordered_transport() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);
    let wait_id = SdkWaitId::new(42);
    let client_for_wait = client.clone();
    let arm = tokio::spawn(async move {
        client_for_wait
            .armed_request(sdk_wait_request(wait_id))
            .await
    });

    assert_eq!(
        read_request(&mut server_stream).await,
        sdk_wait_request(wait_id)
    );
    arm.abort();
    assert!(
        matches!(arm.await, Err(error) if error.is_cancelled()),
        "armed task must report external cancellation"
    );

    assert_cancelled_transport_rejects_follow_up(&client).await;
}

#[tokio::test(start_paused = true)]
async fn dropping_pending_armed_response_invalidates_the_ordered_transport() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);
    let wait_id = SdkWaitId::new(42);
    let client_for_wait = client.clone();
    let arm = tokio::spawn(async move {
        client_for_wait
            .armed_request(sdk_wait_request(wait_id))
            .await
    });

    assert_eq!(
        read_request(&mut server_stream).await,
        sdk_wait_request(wait_id)
    );
    write_response(
        &mut server_stream,
        &Response::CancelSdkWait(CancelSdkWaitResponse::armed_ack(wait_id)),
    )
    .await;
    let pending = arm
        .await
        .expect("armed task must not panic")
        .expect("daemon armed ack succeeds");

    drop(pending);

    assert_cancelled_transport_rejects_follow_up(&client).await;
}

#[tokio::test(start_paused = true)]
async fn operation_timeout_invalidates_the_ordered_transport() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let reusable = TransportClient::spawn(client_stream);
    let client = reusable.with_operation_deadline(OperationDeadline::from_timeout(Some(
        Duration::from_millis(50),
    )));

    let request = spawn_request(&client, has_session_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        has_session_request()
    );
    let following = spawn_request(&reusable, list_sessions_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        list_sessions_request()
    );
    tokio::time::advance(Duration::from_millis(50)).await;

    assert_transport_kind(join_request(request).await, io::ErrorKind::TimedOut);
    assert_transport_kind(join_request(following).await, io::ErrorKind::TimedOut);
    assert_transport_kind(
        reusable.request(list_sessions_request()).await,
        io::ErrorKind::TimedOut,
    );

    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    let mut byte = [0_u8; 1];
    assert_eq!(
        server_stream
            .read(&mut byte)
            .await
            .expect("timed-out client closes its peer"),
        0,
        "a timed-out FIFO transport must close before a late response can be reassigned"
    );
    let late_response = encode_frame(&has_session_response(false)).expect("late response encodes");
    assert!(
        server_stream.write_all(&late_response).await.is_err(),
        "the peer must reject a late response after the timed-out FIFO transport closes"
    );
}

#[tokio::test(start_paused = true)]
async fn operation_deadline_is_shared_across_sequential_requests() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream).with_operation_deadline(
        OperationDeadline::from_timeout(Some(Duration::from_millis(100))),
    );

    let first = spawn_request(&client, has_session_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        has_session_request()
    );
    tokio::time::advance(Duration::from_millis(60)).await;
    write_response(&mut server_stream, &has_session_response(false)).await;
    assert_eq!(
        join_request(first).await.expect("first response succeeds"),
        has_session_response(false)
    );

    let second = spawn_request(&client, list_sessions_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        list_sessions_request()
    );
    tokio::time::advance(Duration::from_millis(40)).await;
    assert_transport_kind(join_request(second).await, io::ErrorKind::TimedOut);
}

#[tokio::test(start_paused = true)]
async fn duration_max_operation_deadline_is_unbounded() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream)
        .with_operation_deadline(OperationDeadline::from_timeout(Some(Duration::MAX)));

    let request = spawn_request(&client, has_session_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        has_session_request()
    );
    tokio::time::advance(Duration::from_secs(24 * 60 * 60)).await;
    assert!(
        !request.is_finished(),
        "Duration::MAX must not arm the transport deadline"
    );
    write_response(&mut server_stream, &has_session_response(true)).await;
    assert_eq!(
        join_request(request)
            .await
            .expect("unbounded response succeeds"),
        has_session_response(true)
    );
}

#[tokio::test]
async fn armed_request_waits_for_daemon_armed_ack_and_keeps_final_pending() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);
    let wait_id = SdkWaitId::new(42);
    let client_for_wait = client.clone();
    let mut arm = tokio::spawn(async move {
        client_for_wait
            .armed_request(sdk_wait_request(wait_id))
            .await
    });

    assert_eq!(
        read_request(&mut server_stream).await,
        sdk_wait_request(wait_id)
    );
    assert!(
        timeout(Duration::from_millis(50), &mut arm).await.is_err(),
        "armed_request must not complete when only the client frame was written"
    );

    write_response(
        &mut server_stream,
        &Response::CancelSdkWait(CancelSdkWaitResponse::armed_ack(wait_id)),
    )
    .await;
    let mut pending = arm
        .await
        .expect("armed task must not panic")
        .expect("daemon armed ack succeeds");
    assert!(
        timeout(Duration::from_millis(50), &mut pending)
            .await
            .is_err(),
        "Armed ack must not complete the final SDK wait response"
    );

    write_response(
        &mut server_stream,
        &sdk_wait_response(wait_id, SdkWaitOutcome::Matched),
    )
    .await;
    assert_eq!(
        pending.await.expect("final wait response succeeds"),
        sdk_wait_response(wait_id, SdkWaitOutcome::Matched)
    );
}

#[tokio::test(start_paused = true)]
async fn armed_request_deadline_ends_at_the_armed_ack() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream).with_operation_deadline(
        OperationDeadline::from_timeout(Some(Duration::from_millis(50))),
    );
    let wait_id = SdkWaitId::new(42);
    let arm = tokio::spawn(async move { client.armed_request(sdk_wait_request(wait_id)).await });

    assert_eq!(
        read_request(&mut server_stream).await,
        sdk_wait_request(wait_id)
    );
    tokio::time::advance(Duration::from_millis(40)).await;
    write_response(
        &mut server_stream,
        &Response::CancelSdkWait(CancelSdkWaitResponse::armed_ack(wait_id)),
    )
    .await;
    let pending = arm
        .await
        .expect("armed task must not panic")
        .expect("armed ack arrives within the operation deadline");

    tokio::time::advance(Duration::from_millis(20)).await;
    write_response(
        &mut server_stream,
        &sdk_wait_response(wait_id, SdkWaitOutcome::Matched),
    )
    .await;
    assert_eq!(
        pending
            .await
            .expect("final wait uses the ArmedWait timeout, not the expired arm deadline"),
        sdk_wait_response(wait_id, SdkWaitOutcome::Matched)
    );
}

#[tokio::test]
async fn armed_request_rejects_duplicate_armed_ack_as_protocol_drift() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);
    let wait_id = SdkWaitId::new(42);
    let client_for_wait = client.clone();
    let arm = tokio::spawn(async move {
        client_for_wait
            .armed_request(sdk_wait_request(wait_id))
            .await
    });

    assert_eq!(
        read_request(&mut server_stream).await,
        sdk_wait_request(wait_id)
    );
    write_response(
        &mut server_stream,
        &Response::CancelSdkWait(CancelSdkWaitResponse::armed_ack(wait_id)),
    )
    .await;
    let pending = arm
        .await
        .expect("armed task must not panic")
        .expect("daemon armed ack succeeds");

    write_response(
        &mut server_stream,
        &Response::CancelSdkWait(CancelSdkWaitResponse::armed_ack(wait_id)),
    )
    .await;

    assert_transport_message(
        pending.await,
        io::ErrorKind::InvalidData,
        "sent `cancel-sdk-wait` response for pending `sdk-wait-output` request",
    );
}

#[tokio::test]
async fn armed_request_rejects_wrong_wait_id_ack_without_rearming() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);
    let wait_id = SdkWaitId::new(42);
    let client_for_wait = client.clone();
    let arm = tokio::spawn(async move {
        client_for_wait
            .armed_request(sdk_wait_request(wait_id))
            .await
    });

    assert_eq!(
        read_request(&mut server_stream).await,
        sdk_wait_request(wait_id)
    );
    write_response(
        &mut server_stream,
        &Response::CancelSdkWait(CancelSdkWaitResponse::armed_ack(SdkWaitId::new(43))),
    )
    .await;

    match arm.await.expect("armed task must not panic") {
        Err(RmuxError::Transport { source, .. }) => {
            assert_eq!(source.kind(), io::ErrorKind::InvalidData);
            assert!(
                source
                    .to_string()
                    .contains("armed ack id 43 for pending wait id 42"),
                "unexpected error: {source}"
            );
        }
        Err(error) => panic!("expected transport error for invalid armed ack, got {error:?}"),
        Ok(_) => panic!("expected invalid armed ack to fail, got pending response"),
    }
}

#[tokio::test]
async fn actor_correlates_bare_responses_in_fifo_request_order() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);

    let first = spawn_request(&client, has_session_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        has_session_request()
    );

    let second = spawn_request(&client, list_sessions_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        list_sessions_request()
    );

    write_response(&mut server_stream, &has_session_response(true)).await;
    write_response(&mut server_stream, &list_sessions_response(b"alpha\n")).await;

    let (first, second) = tokio::join!(join_request(first), join_request(second));
    assert_eq!(first.expect("first response"), has_session_response(true));
    assert_eq!(
        second.expect("second response"),
        list_sessions_response(b"alpha\n")
    );
}

#[tokio::test]
async fn actor_rejects_out_of_order_response_kinds_and_closes_transport() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);

    let first = spawn_request(&client, has_session_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        has_session_request()
    );

    let second = spawn_request(&client, list_sessions_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        list_sessions_request()
    );

    write_response(&mut server_stream, &list_sessions_response(b"alpha\n")).await;

    let (first, second) = tokio::join!(join_request(first), join_request(second));
    assert_transport_message(
        first,
        io::ErrorKind::InvalidData,
        "sent `list-sessions` response for pending `has-session` request",
    );
    assert_transport_kind(second, io::ErrorKind::InvalidData);
    assert_transport_kind(
        client.request(has_session_request()).await,
        io::ErrorKind::InvalidData,
    );
}

#[tokio::test]
async fn error_response_completes_current_fifo_slot_as_protocol_error() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);

    let request = spawn_request(&client, has_session_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        has_session_request()
    );
    write_response(&mut server_stream, &session_not_found_response()).await;

    match join_request(request).await.expect_err("request must fail") {
        RmuxError::Protocol { source } => {
            assert_eq!(
                source.into_proto(),
                rmux_proto::RmuxError::SessionNotFound("alpha".to_owned())
            );
        }
        error => panic!("expected protocol error, got {error:?}"),
    }
}

#[tokio::test]
async fn unsupported_capability_response_maps_to_stable_sdk_feature() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);

    let request = spawn_request(&client, handshake_request());
    assert_eq!(read_request(&mut server_stream).await, handshake_request());
    write_response(&mut server_stream, &unsupported_capability_response()).await;

    match join_request(request).await.expect_err("request must fail") {
        RmuxError::Unsupported { feature, hint } => {
            assert_eq!(feature, "capability.future");
            assert!(hint.contains(rmux_proto::CAPABILITY_HANDSHAKE));
        }
        error => panic!("expected unsupported error, got {error:?}"),
    }
}

#[tokio::test]
async fn unsupported_wire_version_response_maps_to_stable_sdk_feature() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);

    let request = spawn_request(&client, handshake_request());
    assert_eq!(read_request(&mut server_stream).await, handshake_request());
    write_response(&mut server_stream, &unsupported_wire_version_response()).await;

    match join_request(request).await.expect_err("request must fail") {
        RmuxError::Unsupported { feature, .. } => {
            assert_eq!(feature, crate::FEATURE_PROTOCOL_WIRE_VERSION);
        }
        error => panic!("expected unsupported error, got {error:?}"),
    }
}

#[tokio::test]
async fn handshake_response_decode_mismatch_maps_to_stable_sdk_feature() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);

    let request = spawn_request(&client, handshake_request());
    assert_eq!(read_request(&mut server_stream).await, handshake_request());
    write_unknown_response_tag(&mut server_stream).await;

    assert_unsupported_feature(
        join_request(request).await,
        crate::FEATURE_PROTOCOL_CAPABILITIES,
    );
}

#[tokio::test]
async fn capability_require_caches_handshake_per_transport() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);

    let first_require = {
        let client = client.clone();
        tokio::spawn(async move {
            crate::capabilities::require(&client, &[CAPABILITY_SDK_PANE_BY_ID]).await
        })
    };

    match read_request(&mut server_stream).await {
        Request::Handshake(HandshakeRequest {
            required_capabilities,
            ..
        }) => {
            assert_eq!(required_capabilities, vec![CAPABILITY_HANDSHAKE.to_owned()]);
        }
        request => panic!("expected capability handshake, got {request:?}"),
    }
    write_response(
        &mut server_stream,
        &Response::Handshake(HandshakeResponse {
            wire_version: rmux_proto::RMUX_WIRE_VERSION,
            capabilities: vec![
                CAPABILITY_HANDSHAKE.to_owned(),
                CAPABILITY_SDK_PANE_BY_ID.to_owned(),
            ],
        }),
    )
    .await;
    first_require
        .await
        .expect("require task must not panic")
        .expect("first require succeeds");

    crate::capabilities::require(&client, &[CAPABILITY_SDK_PANE_BY_ID])
        .await
        .expect("second require uses cached capabilities");

    let request = spawn_request(&client, has_session_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        has_session_request(),
        "cached capability check must not send another handshake before the next request"
    );
    write_response(&mut server_stream, &has_session_response(true)).await;
    assert_eq!(
        join_request(request).await.expect("request succeeds"),
        has_session_response(true)
    );
}

#[tokio::test]
async fn unsolicited_response_without_pending_request_closes_transport() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);

    write_response(&mut server_stream, &has_session_response(true)).await;
    timeout(Duration::from_secs(1), async {
        while client.state.terminal_failure().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actor must reject unsolicited response before the next request");

    assert_transport_message(
        client.request(has_session_request()).await,
        io::ErrorKind::InvalidData,
        "sent unsolicited `has-session` response",
    );
}

#[tokio::test(start_paused = true)]
async fn dropping_sent_shutdown_invalidates_the_transport() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);
    let client_for_shutdown = client.clone();
    let shutdown = tokio::spawn(async move { client_for_shutdown.shutdown().await });

    let mut buffer = [0_u8; 1];
    assert_eq!(
        server_stream
            .read(&mut buffer)
            .await
            .expect("read client write-side eof"),
        0,
        "shutdown command must reach the actor before cancellation"
    );
    shutdown.abort();
    assert!(
        shutdown
            .await
            .expect_err("shutdown task must be cancelled")
            .is_cancelled(),
        "shutdown task must report external cancellation"
    );

    assert_cancelled_transport_rejects_follow_up(&client).await;
}

#[tokio::test]
async fn transport_shutdown_waits_for_peer_close() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);
    let client_for_shutdown = client.clone();
    let mut shutdown = tokio::spawn(async move { client_for_shutdown.shutdown().await });

    let mut buffer = [0_u8; 1];
    assert_eq!(
        server_stream
            .read(&mut buffer)
            .await
            .expect("read client eof"),
        0
    );
    assert!(
        timeout(Duration::from_millis(50), &mut shutdown)
            .await
            .is_err(),
        "shutdown must wait for the peer read side to close"
    );

    drop(server_stream);
    shutdown
        .await
        .expect("shutdown task")
        .expect("shutdown succeeds after peer close");
}

#[tokio::test]
async fn transport_shutdown_treats_prior_peer_eof_as_clean_close() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);

    let request = spawn_request(&client, has_session_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        has_session_request()
    );
    write_response(&mut server_stream, &has_session_response(true)).await;
    drop(server_stream);

    assert_eq!(
        join_request(request).await.expect("request response"),
        has_session_response(true)
    );
    timeout(Duration::from_secs(1), async {
        while !client
            .state
            .terminal_failure()
            .is_some_and(|failure| failure.is_eof())
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("transport observes peer close");

    client
        .shutdown()
        .await
        .expect("already closed peer is a clean shutdown");
}

#[tokio::test]
async fn actor_wakes_every_pending_caller_on_eof() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);

    let first = spawn_request(&client, has_session_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        has_session_request()
    );

    let second = spawn_request(&client, list_sessions_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        list_sessions_request()
    );
    drop(server_stream);

    let (first, second) = tokio::join!(join_request(first), join_request(second));
    assert_transport_kind(first, io::ErrorKind::UnexpectedEof);
    assert_transport_kind(second, io::ErrorKind::UnexpectedEof);
    assert_transport_kind(
        client.request(has_session_request()).await,
        io::ErrorKind::UnexpectedEof,
    );
}

#[tokio::test]
async fn actor_wakes_every_pending_caller_on_bad_frame() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);

    let first = spawn_request(&client, has_session_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        has_session_request()
    );

    let second = spawn_request(&client, list_sessions_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        list_sessions_request()
    );
    server_stream
        .write_all(&[0])
        .await
        .expect("write invalid frame byte");

    let (first, second) = tokio::join!(join_request(first), join_request(second));
    assert_transport_kind(first, io::ErrorKind::InvalidData);
    assert_transport_kind(second, io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn actor_wakes_every_pending_caller_on_read_error() {
    let client = TransportClient::spawn(ScriptedIo::read_error_after_writes(2));

    let first = client.request(has_session_request());
    let second = client.request(list_sessions_request());

    let (first, second) = tokio::join!(first, second);
    assert_transport_kind(first, io::ErrorKind::ConnectionReset);
    assert_transport_kind(second, io::ErrorKind::ConnectionReset);
    assert_transport_kind(
        client.request(has_session_request()).await,
        io::ErrorKind::ConnectionReset,
    );
}

#[tokio::test]
async fn actor_wakes_every_pending_caller_on_write_error() {
    let client = TransportClient::spawn(ScriptedIo::write_error_on_call(2));

    let first = client.request(has_session_request());
    let second = client.request(list_sessions_request());

    let (first, second) = tokio::join!(first, second);
    assert_transport_kind(first, io::ErrorKind::BrokenPipe);
    assert_transport_kind(second, io::ErrorKind::BrokenPipe);
    assert_transport_kind(
        client.request(has_session_request()).await,
        io::ErrorKind::BrokenPipe,
    );
}

#[tokio::test]
async fn terminal_read_error_before_request_write_is_reported_explicitly() {
    let client = TransportClient::spawn(ScriptedIo::read_error_after_writes(0));

    assert_transport_kind(
        client.request(has_session_request()).await,
        io::ErrorKind::ConnectionReset,
    );
    assert_transport_kind(
        client.request(list_sessions_request()).await,
        io::ErrorKind::ConnectionReset,
    );
}

#[tokio::test]
async fn drop_guard_uses_nonblocking_best_effort_actor_send() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);

    drop(DropGuard::best_effort(
        client.clone(),
        has_session_request(),
    ));

    assert_eq!(
        read_request(&mut server_stream).await,
        has_session_request()
    );
    write_response(&mut server_stream, &has_session_response(true)).await;

    let follow_up = spawn_request(&client, list_sessions_request());
    assert_eq!(
        read_request(&mut server_stream).await,
        list_sessions_request()
    );
    write_response(&mut server_stream, &list_sessions_response(b"alpha\n")).await;

    assert_eq!(
        join_request(follow_up).await.expect("follow-up response"),
        list_sessions_response(b"alpha\n")
    );
}

#[derive(Clone, Copy)]
enum Script {
    ReadErrorAfterWrites { writes: usize },
    WriteErrorOnCall { call: usize },
}

struct ScriptedIo {
    script: Script,
    state: Arc<Mutex<ScriptedIoState>>,
}

#[derive(Default)]
struct ScriptedIoState {
    write_calls: usize,
    read_waker: Option<Waker>,
}

impl ScriptedIo {
    fn read_error_after_writes(writes: usize) -> Self {
        Self {
            script: Script::ReadErrorAfterWrites { writes },
            state: Arc::new(Mutex::new(ScriptedIoState::default())),
        }
    }

    fn write_error_on_call(call: usize) -> Self {
        Self {
            script: Script::WriteErrorOnCall { call },
            state: Arc::new(Mutex::new(ScriptedIoState::default())),
        }
    }
}

impl AsyncRead for ScriptedIo {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        _buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut state = self.state.lock().expect("scripted state lock");
        match self.script {
            Script::ReadErrorAfterWrites { writes } if state.write_calls >= writes => {
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "scripted read error",
                )))
            }
            _ => {
                state.read_waker = Some(context.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl AsyncWrite for ScriptedIo {
    fn poll_write(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut state = self.state.lock().expect("scripted state lock");
        state.write_calls += 1;
        if matches!(self.script, Script::WriteErrorOnCall { call } if state.write_calls == call) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "scripted write error",
            )));
        }
        if let Some(waker) = state.read_waker.take() {
            waker.wake();
        }
        Poll::Ready(Ok(buffer.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

const PIPELINED_CALLERS: usize = 1000;
const PIPELINED_RESPONSE_PADDING: usize = 4096;

fn numbered_list_sessions_request(index: usize) -> Request {
    Request::ListSessions(ListSessionsRequest {
        format: Some(index.to_string()),
        filter: None,
        sort_order: None,
        reversed: false,
    })
}

fn numbered_list_sessions_stdout(index: &str) -> Vec<u8> {
    let mut stdout = format!("{index}\n").into_bytes();
    stdout.resize(stdout.len() + PIPELINED_RESPONSE_PADDING, b'.');
    stdout
}

/// Serves a connection the way the daemon serves a detached client: it reads
/// one request, writes that request's whole response, and only then reads the
/// next request. A client that stops reading responses therefore stops this
/// peer from reading requests.
async fn serve_requests_serially<S>(mut stream: S)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut decoder = rmux_proto::FrameDecoder::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let request = loop {
            if let Some(request) = decoder
                .next_frame::<Request>()
                .expect("request frame decodes")
            {
                break request;
            }
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(read) => decoder.push_bytes(&buffer[..read]),
            }
        };
        let Request::ListSessions(ListSessionsRequest {
            format: Some(index),
            ..
        }) = request
        else {
            panic!("serial peer expects numbered list-sessions requests, got {request:?}");
        };
        let response = list_sessions_response(&numbered_list_sessions_stdout(&index));
        let frame = encode_frame(&response).expect("response encodes");
        if stream.write_all(&frame).await.is_err() {
            return;
        }
    }
}

/// Issues `PIPELINED_CALLERS` concurrent requests on one shared transport and
/// requires each caller to receive its own response before the per-request
/// deadline. The outer bound only keeps a regressed actor from hanging CI.
async fn assert_pipelined_callers_complete<S>(client_stream: S, peer_stream: S)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let peer = tokio::spawn(serve_requests_serially(peer_stream));
    let client =
        TransportClient::spawn(client_stream).with_default_timeout(Some(Duration::from_secs(10)));

    let callers: Vec<_> = (0..PIPELINED_CALLERS)
        .map(|index| {
            let client = client.clone();
            tokio::spawn(async move {
                client
                    .request(numbered_list_sessions_request(index))
                    .await
                    .map(|response| (index, response))
            })
        })
        .collect();

    let outcome = timeout(Duration::from_secs(30), async {
        let mut failures = Vec::new();
        for caller in callers {
            match caller.await.expect("caller task") {
                Ok((index, response)) => assert_eq!(
                    response,
                    list_sessions_response(&numbered_list_sessions_stdout(&index.to_string())),
                    "caller {index} must receive its own FIFO response"
                ),
                Err(error) => failures.push(error),
            }
        }
        failures
    })
    .await;
    client.abort();
    peer.abort();

    let failures = outcome.expect("pipelined callers must not hang the shared transport");
    assert!(
        failures.is_empty(),
        "{} of {PIPELINED_CALLERS} pipelined callers failed on a healthy serial peer; first: {:?}",
        failures.len(),
        failures.first()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipelined_callers_on_a_serial_peer_do_not_deadlock_the_actor() {
    let (client_stream, peer_stream) = tokio::io::duplex(8192);
    assert_pipelined_callers_complete(client_stream, peer_stream).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipelined_callers_on_a_serial_unix_socket_peer_do_not_deadlock_the_actor() {
    let (client_stream, peer_stream) = tokio::net::UnixStream::pair().expect("unix stream pair");
    assert_pipelined_callers_complete(client_stream, peer_stream).await;
}

#[tokio::test]
async fn stalled_peer_back_pressures_callers_into_the_bounded_command_queue() {
    let (client_stream, _stalled_peer) = tokio::io::duplex(64);
    let client = TransportClient::spawn(client_stream);
    let callers: Vec<_> = (0..4 * super::ACTOR_QUEUE_CAPACITY)
        .map(|_| spawn_request(&client, list_sessions_request()))
        .collect();

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        client.commands.capacity(),
        0,
        "once its outgoing frame buffer is full, the actor must leave later callers \
         waiting in the bounded command queue"
    );

    client.abort();
    for caller in callers {
        caller.abort();
    }
}

#[tokio::test]
async fn shutdown_closes_the_write_side_only_after_queued_requests_are_written() {
    let peer_buffer = 4;
    let request_frame = encode_frame(&list_sessions_request()).expect("request encodes");
    assert!(request_frame.len() > 2 * peer_buffer);
    let (client_stream, mut server_stream) = tokio::io::duplex(peer_buffer);
    let client = TransportClient::spawn(client_stream);
    let request = spawn_request(&client, list_sessions_request());

    let mut received = vec![0_u8; 1];
    server_stream
        .read_exact(&mut received)
        .await
        .expect("first request byte");

    let (reply, shutdown) = tokio::sync::oneshot::channel();
    client
        .commands
        .send(ActorMessage::Shutdown { reply })
        .await
        .expect("queue shutdown behind the partly written request");
    timeout(Duration::from_secs(1), async {
        while client.commands.capacity() != super::ACTOR_QUEUE_CAPACITY {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actor takes the shutdown while the request frame is still buffered");

    server_stream
        .read_to_end(&mut received)
        .await
        .expect("read until the client closes its write side");
    assert_eq!(
        received, request_frame,
        "the write side must close only after the queued request frame is complete"
    );

    write_response(&mut server_stream, &list_sessions_response(b"alpha\n")).await;
    assert_eq!(
        join_request(request)
            .await
            .expect("queued request response"),
        list_sessions_response(b"alpha\n")
    );
    drop(server_stream);
    shutdown
        .await
        .expect("shutdown reply")
        .expect("shutdown succeeds after the peer closes");
}

/// Starts unbounded callers on a stalled peer until the actor's outgoing
/// frame buffer and the bounded command queue are both full, so that any
/// further caller waits in `send` and its request is never written.
async fn saturate_command_queue(
    client: &TransportClient,
) -> Vec<(usize, JoinHandle<crate::Result<Response>>)> {
    let callers: Vec<_> = (0..4 * super::ACTOR_QUEUE_CAPACITY)
        .map(|index| {
            (
                index,
                spawn_request(client, numbered_list_sessions_request(index)),
            )
        })
        .collect();
    // With paused time, this sleep returns only once every task is idle: the
    // actor has filled its outgoing frames and stopped taking commands.
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(
        client.commands.capacity(),
        0,
        "the stalled peer must leave later callers waiting in the bounded command queue"
    );
    callers
}

fn assert_timed_out<T>(result: crate::Result<T>) {
    match result {
        Ok(_) => panic!("a caller whose deadline expired in the command queue must fail"),
        Err(RmuxError::Transport { source, .. }) => {
            assert_eq!(source.kind(), io::ErrorKind::TimedOut);
        }
        Err(error) => panic!("expected a timed-out transport error, got {error:?}"),
    }
}

/// Serves the saturating callers once the queued caller has timed out, and
/// requires every one of them, then a fresh request, to receive its own
/// response on the same shared handle.
async fn assert_queued_timeout_left_transport_healthy(
    client: &TransportClient,
    peer_stream: tokio::io::DuplexStream,
    callers: Vec<(usize, JoinHandle<crate::Result<Response>>)>,
) {
    assert!(
        client.state.terminal_failure().is_none(),
        "a request that never left the command queue must not poison the shared transport"
    );
    let peer = tokio::spawn(serve_requests_serially(peer_stream));
    for (index, caller) in callers {
        assert_eq!(
            join_request(caller)
                .await
                .unwrap_or_else(|error| panic!("unbounded caller {index} failed: {error:?}")),
            list_sessions_response(&numbered_list_sessions_stdout(&index.to_string())),
            "caller {index} must receive its own FIFO response"
        );
    }
    let fresh = 4 * super::ACTOR_QUEUE_CAPACITY;
    assert_eq!(
        client
            .request(numbered_list_sessions_request(fresh))
            .await
            .expect("a fresh request on the healthy handle succeeds"),
        list_sessions_response(&numbered_list_sessions_stdout(&fresh.to_string()))
    );
    client.abort();
    peer.abort();
}

#[tokio::test(start_paused = true)]
async fn request_deadline_expiring_in_the_command_queue_fails_only_its_caller() {
    let (client_stream, peer_stream) = tokio::io::duplex(64);
    let unbounded = TransportClient::spawn(client_stream).with_default_timeout(None);
    let callers = saturate_command_queue(&unbounded).await;

    let short = unbounded.with_default_timeout(Some(Duration::from_millis(100)));
    assert_timed_out(short.request(has_session_request()).await);

    assert_queued_timeout_left_transport_healthy(&unbounded, peer_stream, callers).await;
}

#[tokio::test(start_paused = true)]
async fn armed_request_deadline_expiring_in_the_command_queue_fails_only_its_caller() {
    let (client_stream, peer_stream) = tokio::io::duplex(64);
    let unbounded = TransportClient::spawn(client_stream).with_default_timeout(None);
    let callers = saturate_command_queue(&unbounded).await;

    let short = unbounded.with_default_timeout(Some(Duration::from_millis(100)));
    assert_timed_out(
        short
            .armed_request(sdk_wait_request(SdkWaitId::new(42)))
            .await,
    );

    assert_queued_timeout_left_transport_healthy(&unbounded, peer_stream, callers).await;
}
