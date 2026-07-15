use super::*;
use crate::transport::TransportClient;
use rmux_proto::{encode_frame, CancelSdkWaitResponse, FrameDecoder, SdkWaitForOutputResponse};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn read_request(stream: &mut tokio::io::DuplexStream) -> Request {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 512];

    loop {
        if let Some(request) = decoder
            .next_frame::<Request>()
            .expect("request frame decodes")
        {
            return request;
        }

        let read = stream.read(&mut buffer).await.expect("read request");
        assert_ne!(read, 0, "stream closed before request");
        decoder.push_bytes(&buffer[..read]);
    }
}

async fn write_response(stream: &mut tokio::io::DuplexStream, response: Response) {
    let frame = encode_frame(&response).expect("response encodes");
    stream.write_all(&frame).await.expect("write response");
    stream.flush().await.expect("flush response");
}

#[tokio::test]
async fn drop_guard_sends_cancel_request_once_when_wait_future_is_dropped() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);
    let owner_id = client.sdk_wait_owner_id();
    let wait_id = client.allocate_sdk_wait_id();
    let guard = DropGuard::best_effort(
        client,
        Request::CancelSdkWait(CancelSdkWaitRequest { owner_id, wait_id }),
    );

    drop(guard);

    assert_eq!(
        read_request(&mut server_stream).await,
        Request::CancelSdkWait(CancelSdkWaitRequest { owner_id, wait_id })
    );
    write_response(
        &mut server_stream,
        Response::CancelSdkWait(CancelSdkWaitResponse {
            wait_id,
            removed: true,
        }),
    )
    .await;
}

#[tokio::test]
async fn disarmed_drop_guard_does_not_send_stale_cancel() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let client = TransportClient::spawn(client_stream);
    let owner_id = client.sdk_wait_owner_id();
    let mut guard = DropGuard::best_effort(
        client,
        Request::CancelSdkWait(CancelSdkWaitRequest {
            owner_id,
            wait_id: SdkWaitId::new(9),
        }),
    );
    guard.disarm();
    drop(guard);

    let mut buffer = [0_u8; 1];
    let read = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        server_stream.read(&mut buffer),
    )
    .await;
    match read {
        Err(_) => {}
        Ok(Ok(0)) => {}
        Ok(other) => panic!("disarmed guard must not write cancel, got {other:?}"),
    }
}

#[test]
fn sdk_wait_response_rejects_mismatched_wait_id() {
    let result = sdk_wait_response_to_result(
        Response::SdkWaitForOutput(SdkWaitForOutputResponse {
            wait_id: SdkWaitId::new(10),
            outcome: SdkWaitOutcome::Matched,
        }),
        SdkWaitId::new(9),
    );

    match result.expect_err("mismatched wait id must fail") {
        RmuxError::Protocol {
            source: ProtoError::Server(message),
            ..
        } => assert!(message.contains("did not match request id 9")),
        error => panic!("expected protocol mismatch, got {error:?}"),
    }
}

#[test]
fn duration_max_resolves_to_no_timeout_for_wait_operations() {
    assert_eq!(resolved_wait_timeout(Some(Duration::MAX)), None);
    assert_eq!(
        resolved_wait_timeout_override(Some(Duration::MAX), Some(Duration::from_millis(1))),
        None,
        "an explicit no-timeout override must beat a finite handle default"
    );
    assert_eq!(wait_deadline(Some(Duration::MAX)), None);
}

#[tokio::test]
async fn finite_wait_timeout_surfaces_typed_timeout_error() {
    let error = with_wait_timeout(
        "test wait operation",
        Some(Duration::from_millis(1)),
        std::future::pending::<Result<()>>(),
    )
    .await
    .expect_err("pending wait must time out");

    match error {
        RmuxError::Transport { operation, source } => {
            assert_eq!(operation, "test wait operation");
            assert_eq!(source.kind(), io::ErrorKind::TimedOut);
        }
        other => panic!("expected typed transport timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn no_timeout_branch_awaits_future_directly() {
    let value = with_wait_timeout("test no timeout", None, async { Ok(7_u8) })
        .await
        .expect("untimed ready future completes");

    assert_eq!(value, 7);
}

#[tokio::test]
async fn finite_wait_deadline_bounds_an_in_flight_rpc() {
    let timeout = Duration::from_millis(10);
    let error = with_wait_deadline(
        "test snapshot RPC",
        Some(timeout),
        Some(Instant::now() + timeout),
        std::future::pending::<Result<()>>(),
    )
    .await
    .expect_err("pending RPC must not outlive the overall wait deadline");

    match error {
        RmuxError::Transport { operation, source } => {
            assert_eq!(operation, "test snapshot RPC");
            assert_eq!(source.kind(), io::ErrorKind::TimedOut);
        }
        other => panic!("expected typed transport timeout, got {other:?}"),
    }
}
