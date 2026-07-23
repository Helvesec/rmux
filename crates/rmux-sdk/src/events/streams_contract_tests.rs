//! Pane output stream contract tests.
//!
//! These tests pin the SDK-only stream facade over the v1 pane-output
//! subscription protocol that the daemon already implements.
//!
//! * Both the byte/event stream ([`PaneOutputStream`]) and the rendered
//!   line stream ([`PaneLineStream`]) are reachable through the
//!   `rmux_sdk` re-exports without naming `rmux-client`, `rmux-core`,
//!   `rmux-server`, or `rmux-pty`. They are `Send + 'static`, which is
//!   required so SDK consumers can move them into spawned tasks.
//! * The streams drive `SubscribePaneOutput`, `PaneOutputCursor`, and
//!   `UnsubscribePaneOutput` through the daemon transport without ever
//!   exposing [`rmux_proto::PaneOutputSubscriptionId`] to the SDK's
//!   public API. The contract tests here drive a duplex stream so the
//!   wire flow can be observed deterministically without a live daemon.
//! * Raw streams preserve arbitrary bytes (including non-UTF-8 byte
//!   sequences, embedded NULs, and newlines) and surface daemon-side
//!   gaps as [`PaneOutputChunk::Lag`] notices that carry the daemon's
//!   sequence/recent-bytes report verbatim.
//! * The rendered line stream applies `String::from_utf8_lossy` only
//!   when a complete line is yielded, splits exclusively on `b'\n'`,
//!   and preserves non-LF bytes (including `\r`) inside the rendered
//!   line. A daemon-side lag drops the partial-line buffer and forwards
//!   the lag notice unchanged.
//! * Dropping a stream emits exactly one best-effort
//!   `unsubscribe-pane-output` request through the transport actor; a
//!   refused or late unsubscribe must not surface as an SDK error and
//!   must not close the pane, the window, the session, the underlying
//!   process, or the daemon.

#![cfg(any(unix, windows))]
#![allow(dead_code, clippy::extra_unused_type_parameters)]

use std::fmt::Debug;
use std::time::Duration;

use super::{
    PaneLagNotice, PaneLineItem, PaneLineStream, PaneOutputChunk, PaneOutputStart,
    PaneOutputStream, PaneRecentOutput as SdkRecentOutput,
};
use crate::transport::{OperationDeadline, TransportClient};
use crate::{PaneId, PaneRef, Result};
use rmux_proto::{
    encode_frame, ErrorResponse, FrameDecoder, PaneOutputCursor, PaneOutputCursorRequest,
    PaneOutputCursorResponse, PaneOutputEvent, PaneOutputLagNotice, PaneOutputLagResponse,
    PaneOutputSubscriptionId, PaneOutputSubscriptionStart, PaneRecentOutput, PaneTarget, Request,
    Response, SessionName, SubscribePaneOutputRequest, SubscribePaneOutputResponse,
    UnsubscribePaneOutputRequest,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

const SUBSCRIPTION_ID_RAW: u64 = 17;

fn assert_send<T: Send>() {}
fn assert_static<T: 'static>() {}
fn assert_debug<T: Debug>() {}

fn _assert_bounds() {
    assert_send::<PaneOutputStream>();
    assert_static::<PaneOutputStream>();
    assert_debug::<PaneOutputStream>();

    assert_send::<PaneLineStream>();
    assert_static::<PaneLineStream>();
    assert_debug::<PaneLineStream>();

    assert_send::<PaneOutputChunk>();
    assert_static::<PaneOutputChunk>();
    assert_debug::<PaneOutputChunk>();

    assert_send::<PaneLineItem>();
    assert_static::<PaneLineItem>();
    assert_debug::<PaneLineItem>();

    assert_send::<PaneLagNotice>();
    assert_static::<PaneLagNotice>();

    assert_send::<SdkRecentOutput>();
    assert_static::<SdkRecentOutput>();

    assert_send::<PaneOutputStart>();
    assert_static::<PaneOutputStart>();
}

#[test]
fn pane_output_stream_handles_are_send_and_static() {
    assert_send::<PaneOutputStream>();
    assert_static::<PaneOutputStream>();
    assert_send::<PaneLineStream>();
    assert_static::<PaneLineStream>();
}

fn alpha_target() -> PaneRef {
    PaneRef::new(SessionName::new("alpha").expect("valid session"), 0, 0)
}

fn alpha_proto_target() -> PaneTarget {
    alpha_target().to_proto()
}

fn subscription_id() -> PaneOutputSubscriptionId {
    PaneOutputSubscriptionId::new(SUBSCRIPTION_ID_RAW)
}

fn other_subscription_id() -> PaneOutputSubscriptionId {
    PaneOutputSubscriptionId::new(SUBSCRIPTION_ID_RAW + 1)
}

fn cursor_zero() -> PaneOutputCursor {
    PaneOutputCursor {
        next_sequence: 0,
        missed_events: 0,
    }
}

async fn pane_output_stream_from_duplex<S>(
    target: PaneRef,
    stream: S,
    start: PaneOutputStart,
) -> Result<PaneOutputStream>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let transport = TransportClient::spawn(stream);
    PaneOutputStream::open(
        transport,
        rmux_proto::PaneTargetRef::slot(target.into()),
        start,
    )
    .await
}

async fn pane_line_stream_from_duplex<S>(
    target: PaneRef,
    stream: S,
    start: PaneOutputStart,
) -> Result<PaneLineStream>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let inner = pane_output_stream_from_duplex(target, stream, start).await?;
    Ok(PaneLineStream::wrap(inner))
}

async fn read_request(stream: &mut DuplexStream) -> Request {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 1024];
    loop {
        if let Some(request) = decoder
            .next_frame::<Request>()
            .expect("request frame decodes")
        {
            return request;
        }
        let read = stream.read(&mut buffer).await.expect("read request bytes");
        assert_ne!(read, 0, "transport closed before request arrived");
        decoder.push_bytes(&buffer[..read]);
    }
}

async fn try_read_request(stream: &mut DuplexStream, deadline: Duration) -> Option<Request> {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 1024];
    let deadline_at = tokio::time::Instant::now() + deadline;
    loop {
        if let Some(request) = decoder
            .next_frame::<Request>()
            .expect("request frame decodes")
        {
            return Some(request);
        }
        let remaining = deadline_at.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, stream.read(&mut buffer)).await {
            Ok(Ok(0)) => return None,
            Ok(Ok(read)) => decoder.push_bytes(&buffer[..read]),
            Ok(Err(_)) => return None,
            Err(_) => return None,
        }
    }
}

async fn write_response(stream: &mut DuplexStream, response: &Response) {
    let frame = encode_frame(response).expect("response encodes");
    stream.write_all(&frame).await.expect("write response");
    stream.flush().await.expect("flush response");
}

async fn drive_subscribe_response(
    stream: &mut DuplexStream,
    expected_target: &PaneTarget,
    expected_start: PaneOutputSubscriptionStart,
) {
    match read_request(stream).await {
        Request::SubscribePaneOutput(SubscribePaneOutputRequest { target, start }) => {
            assert_eq!(target, expected_target.clone());
            assert_eq!(start, expected_start);
        }
        other => panic!("expected subscribe-pane-output, got {other:?}"),
    }
    write_response(
        stream,
        &Response::SubscribePaneOutput(SubscribePaneOutputResponse {
            subscription_id: subscription_id(),
            target: expected_target.clone(),
            pane_id: PaneId::new(1),
            cursor: cursor_zero(),
        }),
    )
    .await;
}

async fn drive_cursor_with_events(stream: &mut DuplexStream, events: Vec<PaneOutputEvent>) {
    match read_request(stream).await {
        Request::PaneOutputCursor(PaneOutputCursorRequest {
            subscription_id: id,
            ..
        }) => {
            assert_eq!(id, subscription_id());
        }
        other => panic!("expected pane-output-cursor, got {other:?}"),
    }
    write_cursor_events_response(stream, events).await;
}

async fn write_cursor_events_response(stream: &mut DuplexStream, events: Vec<PaneOutputEvent>) {
    write_response(
        stream,
        &Response::PaneOutputCursor(PaneOutputCursorResponse {
            subscription_id: subscription_id(),
            cursor: cursor_zero(),
            events,
            limited: false,
        }),
    )
    .await;
}

async fn drive_cursor_with_lag(stream: &mut DuplexStream, lag: PaneOutputLagNotice) {
    match read_request(stream).await {
        Request::PaneOutputCursor(_) => {}
        other => panic!("expected pane-output-cursor, got {other:?}"),
    }
    write_response(
        stream,
        &Response::PaneOutputLag(Box::new(PaneOutputLagResponse {
            subscription_id: subscription_id(),
            cursor: cursor_zero(),
            lag,
        })),
    )
    .await;
}

async fn drive_cursor_with_subscription_gone(stream: &mut DuplexStream) {
    match read_request(stream).await {
        Request::PaneOutputCursor(_) => {}
        other => panic!("expected pane-output-cursor, got {other:?}"),
    }
    write_response(
        stream,
        &Response::Error(ErrorResponse {
            error: rmux_proto::RmuxError::Server("subscription not found".to_owned()),
        }),
    )
    .await;
}

async fn drive_cursor_with_mismatched_subscription(stream: &mut DuplexStream) {
    match read_request(stream).await {
        Request::PaneOutputCursor(PaneOutputCursorRequest {
            subscription_id: id,
            ..
        }) => assert_eq!(id, subscription_id()),
        other => panic!("expected pane-output-cursor, got {other:?}"),
    }
    write_response(
        stream,
        &Response::PaneOutputCursor(PaneOutputCursorResponse {
            subscription_id: other_subscription_id(),
            cursor: cursor_zero(),
            events: vec![PaneOutputEvent {
                sequence: 1,
                bytes: b"wrong-id\n".to_vec(),
            }],
            limited: false,
        }),
    )
    .await;
}

async fn drive_lag_with_mismatched_subscription(stream: &mut DuplexStream) {
    match read_request(stream).await {
        Request::PaneOutputCursor(PaneOutputCursorRequest {
            subscription_id: id,
            ..
        }) => assert_eq!(id, subscription_id()),
        other => panic!("expected pane-output-cursor, got {other:?}"),
    }
    write_response(
        stream,
        &Response::PaneOutputLag(Box::new(PaneOutputLagResponse {
            subscription_id: other_subscription_id(),
            cursor: cursor_zero(),
            lag: PaneOutputLagNotice {
                expected_sequence: 1,
                resume_sequence: 2,
                missed_events: 1,
                newest_sequence: 2,
                recent: PaneRecentOutput {
                    bytes: b"recent".to_vec(),
                    oldest_sequence: Some(2),
                    newest_sequence: Some(2),
                },
            },
        })),
    )
    .await;
}

async fn open_output_stream(
    target: PaneRef,
    start: PaneOutputStart,
) -> (PaneOutputStream, DuplexStream) {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let proto_target = target.to_proto();
    let proto_start = match start {
        PaneOutputStart::Now => PaneOutputSubscriptionStart::Now,
        PaneOutputStart::Oldest => PaneOutputSubscriptionStart::Oldest,
    };
    let subscribe = tokio::spawn({
        let target = target.clone();
        async move { pane_output_stream_from_duplex(target, client_stream, start).await }
    });
    drive_subscribe_response(&mut server_stream, &proto_target, proto_start).await;
    let stream = subscribe.await.expect("subscribe task").expect("opens");
    (stream, server_stream)
}

#[tokio::test(start_paused = true)]
async fn output_stream_does_not_retain_its_expired_open_deadline() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let transport = TransportClient::spawn(client_stream).with_operation_deadline(
        OperationDeadline::from_timeout(Some(Duration::from_millis(50))),
    );
    let subscribe = tokio::spawn(async move {
        PaneOutputStream::open(
            transport,
            rmux_proto::PaneTargetRef::slot(target.into()),
            PaneOutputStart::Now,
        )
        .await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe
        .await
        .expect("subscribe task must not panic")
        .expect("stream opens");

    tokio::time::advance(Duration::from_millis(100)).await;
    let mut poll = tokio::spawn(async move { stream.poll_once().await });
    tokio::select! {
        result = &mut poll => panic!("stream retained an expired open deadline: {result:?}"),
        request = read_request(&mut server_stream) => {
            assert!(matches!(request, Request::PaneOutputCursor(_)));
        }
    }
    write_response(
        &mut server_stream,
        &Response::PaneOutputCursor(PaneOutputCursorResponse {
            subscription_id: subscription_id(),
            cursor: cursor_zero(),
            events: Vec::new(),
            limited: false,
        }),
    )
    .await;
    assert!(poll
        .await
        .expect("poll task must not panic")
        .expect("poll starts a fresh operation")
        .is_empty());
}

#[tokio::test]
async fn output_stream_drives_subscribe_then_cursor_then_unsubscribe() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_output_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    let next_task = tokio::spawn(async move {
        let chunk = stream.next().await.expect("cursor poll succeeds");
        (stream, chunk)
    });
    drive_cursor_with_events(
        &mut server_stream,
        vec![PaneOutputEvent {
            sequence: 5,
            bytes: b"\x00hello\xff".to_vec(),
        }],
    )
    .await;
    let (stream, chunk) = next_task.await.expect("next task");
    let chunk = chunk.expect("non-empty chunk");
    match chunk {
        PaneOutputChunk::Bytes { sequence, bytes } => {
            assert_eq!(sequence, 5);
            assert_eq!(bytes, b"\x00hello\xff");
        }
        other => panic!("expected raw bytes chunk, got {other:?}"),
    }

    drop(stream);
    let unsubscribe = read_request(&mut server_stream).await;
    match unsubscribe {
        Request::UnsubscribePaneOutput(UnsubscribePaneOutputRequest {
            subscription_id: id,
        }) => assert_eq!(id, subscription_id()),
        other => panic!("expected unsubscribe-pane-output on drop, got {other:?}"),
    }
}

#[tokio::test]
async fn cancelled_output_next_reuses_the_inflight_cursor_response() {
    let target = alpha_target();
    let (mut stream, mut server_stream) = open_output_stream(target, PaneOutputStart::Now).await;

    let mut interrupted = Box::pin(stream.next());
    let request = tokio::select! {
        result = &mut interrupted => panic!("cursor poll completed before cancellation: {result:?}"),
        request = read_request(&mut server_stream) => request,
    };
    assert!(
        matches!(request, Request::PaneOutputCursor(_)),
        "expected pane-output cursor, got {request:?}"
    );
    drop(interrupted);

    write_cursor_events_response(
        &mut server_stream,
        vec![PaneOutputEvent {
            sequence: 1,
            bytes: b"retained".to_vec(),
        }],
    )
    .await;

    let mut resumed = Box::pin(stream.next());
    let chunk = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::select! {
            result = &mut resumed => result,
            request = read_request(&mut server_stream) => {
                panic!("resuming next() must reuse the cursor response, got {request:?}")
            }
        }
    })
    .await
    .expect("retained cursor response completes")
    .expect("resumed next succeeds")
    .expect("retained chunk");
    assert_eq!(
        chunk,
        PaneOutputChunk::Bytes {
            sequence: 1,
            bytes: b"retained".to_vec(),
        }
    );
}

#[tokio::test]
async fn cancelled_poll_once_preserves_buffered_and_inflight_chunks() {
    let target = alpha_target();
    let (mut stream, mut server_stream) = open_output_stream(target, PaneOutputStart::Now).await;

    let first = tokio::spawn(async move {
        let chunk = stream.next().await.expect("first cursor succeeds");
        (stream, chunk)
    });
    drive_cursor_with_events(
        &mut server_stream,
        vec![
            PaneOutputEvent {
                sequence: 1,
                bytes: b"returned".to_vec(),
            },
            PaneOutputEvent {
                sequence: 2,
                bytes: b"buffered".to_vec(),
            },
        ],
    )
    .await;
    let (mut stream, first) = first.await.expect("first next task");
    assert!(matches!(
        first,
        Some(PaneOutputChunk::Bytes { sequence: 1, .. })
    ));

    let mut interrupted = Box::pin(stream.poll_once());
    let request = tokio::select! {
        result = &mut interrupted => panic!("cursor poll completed before cancellation: {result:?}"),
        request = read_request(&mut server_stream) => request,
    };
    assert!(
        matches!(request, Request::PaneOutputCursor(_)),
        "expected pane-output cursor, got {request:?}"
    );
    drop(interrupted);

    write_cursor_events_response(
        &mut server_stream,
        vec![PaneOutputEvent {
            sequence: 3,
            bytes: b"inflight".to_vec(),
        }],
    )
    .await;

    let mut resumed = Box::pin(stream.poll_once());
    let chunks = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::select! {
            result = &mut resumed => result,
            request = read_request(&mut server_stream) => {
                panic!("resuming poll_once() must reuse the cursor response, got {request:?}")
            }
        }
    })
    .await
    .expect("retained cursor response completes")
    .expect("resumed poll succeeds");
    let sequences: Vec<u64> = chunks
        .into_iter()
        .map(|chunk| match chunk {
            PaneOutputChunk::Bytes { sequence, .. } => sequence,
            other => panic!("expected byte chunk, got {other:?}"),
        })
        .collect();
    assert_eq!(sequences, vec![2, 3]);
}

#[tokio::test]
async fn cancelled_line_next_reuses_the_inflight_cursor_response() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_line_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    let mut interrupted = Box::pin(stream.next());
    let request = tokio::select! {
        result = &mut interrupted => panic!("cursor poll completed before cancellation: {result:?}"),
        request = read_request(&mut server_stream) => request,
    };
    assert!(
        matches!(request, Request::PaneOutputCursor(_)),
        "expected pane-output cursor, got {request:?}"
    );
    drop(interrupted);

    write_cursor_events_response(
        &mut server_stream,
        vec![PaneOutputEvent {
            sequence: 1,
            bytes: b"retained line\n".to_vec(),
        }],
    )
    .await;

    let mut resumed = Box::pin(stream.next());
    let item = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::select! {
            result = &mut resumed => result,
            request = read_request(&mut server_stream) => {
                panic!("resuming line next() must reuse the cursor response, got {request:?}")
            }
        }
    })
    .await
    .expect("retained cursor response completes")
    .expect("resumed line next succeeds")
    .expect("retained line");
    assert_eq!(
        item,
        PaneLineItem::Line {
            text: "retained line".to_owned(),
        }
    );
}

#[tokio::test]
async fn output_stream_preserves_arbitrary_bytes_and_sequences() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_output_stream_from_duplex(target, client_stream, PaneOutputStart::Oldest).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Oldest,
    )
    .await;
    let stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    let next_task = tokio::spawn(async move {
        let mut stream = stream;
        let mut chunks = Vec::new();
        chunks.push(stream.next().await.expect("first chunk"));
        chunks.push(stream.next().await.expect("second chunk"));
        (stream, chunks)
    });
    drive_cursor_with_events(
        &mut server_stream,
        vec![
            PaneOutputEvent {
                sequence: 1,
                bytes: vec![0xff, 0xfe, b'\n', 0x00],
            },
            PaneOutputEvent {
                sequence: 2,
                bytes: b"trailing".to_vec(),
            },
        ],
    )
    .await;
    let (stream, chunks) = next_task.await.expect("next task");
    let mut iter = chunks.into_iter().map(|chunk| chunk.expect("chunk"));
    match iter.next().unwrap() {
        PaneOutputChunk::Bytes { sequence, bytes } => {
            assert_eq!(sequence, 1);
            assert_eq!(bytes, vec![0xff, 0xfe, b'\n', 0x00]);
        }
        other => panic!("expected first bytes chunk, got {other:?}"),
    }
    match iter.next().unwrap() {
        PaneOutputChunk::Bytes { sequence, bytes } => {
            assert_eq!(sequence, 2);
            assert_eq!(bytes, b"trailing");
        }
        other => panic!("expected second bytes chunk, got {other:?}"),
    }
    drop(stream);
}

#[tokio::test]
async fn output_stream_surfaces_lag_notice_with_recent_bytes() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_output_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    let next_task = tokio::spawn(async move {
        let chunk = stream
            .next()
            .await
            .expect("first chunk")
            .expect("non-empty");
        (stream, chunk)
    });
    drive_cursor_with_lag(
        &mut server_stream,
        PaneOutputLagNotice {
            expected_sequence: 10,
            resume_sequence: 17,
            missed_events: 7,
            newest_sequence: 23,
            recent: PaneRecentOutput {
                bytes: vec![0xfe, b'a', b'b'],
                oldest_sequence: Some(15),
                newest_sequence: Some(17),
            },
        },
    )
    .await;
    let (_stream, chunk) = next_task.await.expect("next task");
    match chunk {
        PaneOutputChunk::Lag(notice) => {
            assert_eq!(notice.expected_sequence, 10);
            assert_eq!(notice.resume_sequence, 17);
            assert_eq!(notice.missed_events, 7);
            assert_eq!(notice.newest_sequence, 23);
            assert_eq!(notice.recent.bytes, vec![0xfe, b'a', b'b']);
            assert_eq!(notice.recent.oldest_sequence, Some(15));
            assert_eq!(notice.recent.newest_sequence, Some(17));
        }
        other => panic!("expected lag chunk, got {other:?}"),
    }
}

#[tokio::test]
async fn output_stream_returns_none_when_subscription_gone() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_output_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    let next_task = tokio::spawn(async move {
        let chunk = stream.next().await.expect("cursor result");
        (stream, chunk)
    });
    drive_cursor_with_subscription_gone(&mut server_stream).await;
    let (_stream, chunk) = next_task.await.expect("next task");
    assert!(
        chunk.is_none(),
        "subscription-gone collapses to end of stream"
    );
}

#[tokio::test]
async fn output_stream_rejects_cursor_response_for_different_subscription() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_output_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    let next_task = tokio::spawn(async move { stream.next().await.expect_err("mismatched id") });
    drive_cursor_with_mismatched_subscription(&mut server_stream).await;
    let error = next_task.await.expect("next task");
    let rendered = error.to_string();
    assert!(
        rendered.contains("subscription id 18") && rendered.contains("subscription 17"),
        "mismatched cursor id must be rejected, got {error}"
    );
}

#[tokio::test]
async fn output_stream_rejects_lag_response_for_different_subscription() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_output_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    let next_task = tokio::spawn(async move { stream.next().await.expect_err("mismatched id") });
    drive_lag_with_mismatched_subscription(&mut server_stream).await;
    let error = next_task.await.expect("next task");
    let rendered = error.to_string();
    assert!(
        rendered.contains("subscription id 18") && rendered.contains("subscription 17"),
        "mismatched lag id must be rejected, got {error}"
    );
}

#[tokio::test]
async fn output_stream_drop_emits_best_effort_unsubscribe_exactly_once() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_output_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    drop(stream);
    let unsubscribe = read_request(&mut server_stream).await;
    match unsubscribe {
        Request::UnsubscribePaneOutput(UnsubscribePaneOutputRequest {
            subscription_id: id,
        }) => assert_eq!(id, subscription_id()),
        other => panic!("expected unsubscribe on drop, got {other:?}"),
    }

    // Reply with an error: a refused unsubscribe must not surface as an
    // error anywhere — the stream is already gone.
    write_response(
        &mut server_stream,
        &Response::Error(ErrorResponse {
            error: rmux_proto::RmuxError::Server("subscription not found".to_owned()),
        }),
    )
    .await;

    // No additional unsubscribe request should follow within a generous
    // window — the drop fires exactly once.
    let extra = try_read_request(&mut server_stream, Duration::from_millis(80)).await;
    assert!(
        extra.is_none(),
        "drop must emit exactly one unsubscribe; saw {extra:?}"
    );
}

#[tokio::test]
async fn output_stream_drop_after_transport_close_does_not_panic_or_error() {
    let (client_stream, server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let mut server_stream = server_stream;
    let subscribe_task = tokio::spawn(async move {
        pane_output_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    // Closing the daemon side first means a subsequent drop has no live
    // transport. The drop must still complete cleanly.
    drop(server_stream);
    drop(stream);
}

#[tokio::test]
async fn line_stream_buffers_partial_lines_and_renders_lossy_utf8() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_line_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    // Server delivers two cursor batches; the line stream must buffer
    // the partial first line until the LF in the second batch arrives.
    let next_two_lines = tokio::spawn(async move {
        let mut lines = Vec::new();
        lines.push(stream.next().await.expect("first line"));
        lines.push(stream.next().await.expect("second line"));
        (stream, lines)
    });
    drive_cursor_with_events(
        &mut server_stream,
        vec![PaneOutputEvent {
            sequence: 1,
            bytes: b"hel".to_vec(),
        }],
    )
    .await;
    drive_cursor_with_events(
        &mut server_stream,
        vec![PaneOutputEvent {
            sequence: 2,
            bytes: b"lo\r\n\xff!\n".to_vec(),
        }],
    )
    .await;
    let (stream, lines) = next_two_lines.await.expect("next task");
    let mut lines = lines.into_iter().map(|item| item.expect("line"));

    match lines.next().unwrap() {
        PaneLineItem::Line { text } => assert_eq!(text, "hello\r"),
        other => panic!("expected first line, got {other:?}"),
    }
    match lines.next().unwrap() {
        PaneLineItem::Line { text } => {
            assert!(text.contains('\u{FFFD}'), "lossy UTF-8 must render U+FFFD");
            assert!(text.ends_with('!'), "non-LF bytes must survive");
        }
        other => panic!("expected second line, got {other:?}"),
    }
    drop(stream);
}

#[tokio::test]
async fn line_stream_lag_drops_partial_buffer_and_forwards_notice() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_line_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    let next_task = tokio::spawn(async move {
        let mut items = Vec::new();
        items.push(stream.next().await.expect("first item"));
        items.push(stream.next().await.expect("second item"));
        (stream, items)
    });
    drive_cursor_with_events(
        &mut server_stream,
        vec![PaneOutputEvent {
            sequence: 1,
            bytes: b"partial-without-lf".to_vec(),
        }],
    )
    .await;
    drive_cursor_with_lag(
        &mut server_stream,
        PaneOutputLagNotice {
            expected_sequence: 2,
            resume_sequence: 9,
            missed_events: 6,
            newest_sequence: 12,
            recent: PaneRecentOutput {
                bytes: b"recent\n".to_vec(),
                oldest_sequence: Some(8),
                newest_sequence: Some(9),
            },
        },
    )
    .await;
    drive_cursor_with_events(
        &mut server_stream,
        vec![PaneOutputEvent {
            sequence: 9,
            bytes: b"clean\n".to_vec(),
        }],
    )
    .await;
    let (stream, items) = next_task.await.expect("next task");
    let mut items = items.into_iter().map(|item| item.expect("item"));

    match items.next().unwrap() {
        PaneLineItem::Lag(notice) => {
            assert_eq!(notice.expected_sequence, 2);
            assert_eq!(notice.resume_sequence, 9);
            assert_eq!(notice.missed_events, 6);
        }
        other => panic!("expected lag first, got {other:?}"),
    }
    match items.next().unwrap() {
        PaneLineItem::Line { text } => {
            assert_eq!(text, "clean");
            assert!(
                !text.contains("partial"),
                "lag must drop the buffered partial-line bytes"
            );
        }
        other => panic!("expected clean line, got {other:?}"),
    }
    drop(stream);
}

#[tokio::test]
async fn poll_once_returns_empty_without_blocking_when_no_events_ready() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_output_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    let poll_task = tokio::spawn(async move {
        let drained = stream.poll_once().await.expect("poll succeeds");
        (stream, drained)
    });
    drive_cursor_with_events(&mut server_stream, Vec::new()).await;
    let (stream, drained) = poll_task.await.expect("poll task");
    assert!(drained.is_empty(), "empty cursor batch yields empty drain");
    drop(stream);
}

#[tokio::test]
async fn line_stream_drop_emits_exactly_one_unsubscribe() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_line_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    drop(stream);
    let unsubscribe = read_request(&mut server_stream).await;
    match unsubscribe {
        Request::UnsubscribePaneOutput(UnsubscribePaneOutputRequest {
            subscription_id: id,
        }) => assert_eq!(id, subscription_id()),
        other => panic!("expected unsubscribe on line-stream drop, got {other:?}"),
    }

    // Wrapping a line stream around the byte stream must not duplicate the
    // unsubscribe — the inner byte stream's drop guard is the sole source.
    let extra = try_read_request(&mut server_stream, Duration::from_millis(80)).await;
    assert!(
        extra.is_none(),
        "line-stream drop must emit exactly one unsubscribe; saw {extra:?}"
    );
}

#[tokio::test]
async fn line_stream_drop_after_transport_close_does_not_panic_or_error() {
    let (client_stream, server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let mut server_stream = server_stream;
    let subscribe_task = tokio::spawn(async move {
        pane_line_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    // Closing the daemon side first means a subsequent drop has no live
    // transport. The drop must still complete cleanly even for the line
    // wrapper — the inner byte stream owns the guard.
    drop(server_stream);
    drop(stream);
}

#[tokio::test]
async fn output_stream_next_after_subscription_gone_is_idempotent_none() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_output_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    let next_task = tokio::spawn(async move {
        let first = stream.next().await.expect("first cursor");
        // Once the cursor reports the subscription is gone, every later
        // call must short-circuit to `None` without driving another
        // cursor request.
        let second = stream.next().await.expect("second cursor");
        let third = stream.next().await.expect("third cursor");
        (stream, first, second, third)
    });
    drive_cursor_with_subscription_gone(&mut server_stream).await;
    let (stream, first, second, third) = next_task.await.expect("next task");
    assert!(first.is_none(), "first call collapses to None");
    assert!(second.is_none(), "subsequent calls remain None");
    assert!(third.is_none(), "subsequent calls remain None");

    // Confirm only the single subscribe + single cursor reached the wire;
    // no extra cursor requests after subscription-gone, plus the drop
    // unsubscribe.
    drop(stream);
    let unsubscribe = read_request(&mut server_stream).await;
    assert!(matches!(unsubscribe, Request::UnsubscribePaneOutput(_)));
    let extra = try_read_request(&mut server_stream, Duration::from_millis(50)).await;
    assert!(
        extra.is_none(),
        "no extra cursor requests should fire after subscription-gone; saw {extra:?}"
    );
}

#[tokio::test]
async fn line_stream_drops_trailing_partial_bytes_when_subscription_ends() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_line_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    // The line stream must not synthesize a final line from bytes the
    // daemon never terminated with `\n` — even when those bytes were the
    // only payload before subscription teardown.
    let next_task = tokio::spawn(async move {
        let item = stream.next().await.expect("cursor result");
        (stream, item)
    });
    drive_cursor_with_events(
        &mut server_stream,
        vec![PaneOutputEvent {
            sequence: 1,
            bytes: b"trailing-without-lf".to_vec(),
        }],
    )
    .await;
    drive_cursor_with_subscription_gone(&mut server_stream).await;
    let (_stream, item) = next_task.await.expect("next task");
    assert!(
        item.is_none(),
        "trailing partial bytes must be dropped silently at end-of-stream; got {item:?}"
    );
}

#[tokio::test]
async fn line_stream_preserves_nul_and_carriage_return_bytes_inside_lines() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_line_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    // Embedded NUL is valid UTF-8 (`U+0000`) and must survive lossy
    // decoding verbatim; only invalid byte sequences become `U+FFFD`.
    let next_task = tokio::spawn(async move {
        let item = stream.next().await.expect("cursor").expect("line item");
        (stream, item)
    });
    drive_cursor_with_events(
        &mut server_stream,
        vec![PaneOutputEvent {
            sequence: 1,
            bytes: b"a\0b\rc\n".to_vec(),
        }],
    )
    .await;
    let (_stream, item) = next_task.await.expect("next task");
    match item {
        PaneLineItem::Line { text } => {
            assert_eq!(text, "a\0b\rc");
            assert!(
                !text.contains('\u{FFFD}'),
                "valid UTF-8 must not produce U+FFFD; got `{text}`"
            );
        }
        other => panic!("expected line item, got {other:?}"),
    }
}

#[tokio::test]
async fn line_stream_emits_multiple_lines_from_single_event() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_line_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    let next_task = tokio::spawn(async move {
        let mut items = Vec::new();
        for _ in 0..3 {
            items.push(stream.next().await.expect("cursor").expect("item"));
        }
        (stream, items)
    });
    drive_cursor_with_events(
        &mut server_stream,
        vec![PaneOutputEvent {
            sequence: 1,
            bytes: b"alpha\n\nbeta\n".to_vec(),
        }],
    )
    .await;
    let (_stream, items) = next_task.await.expect("next task");
    let texts: Vec<String> = items
        .into_iter()
        .map(|item| match item {
            PaneLineItem::Line { text } => text,
            other => panic!("expected line item, got {other:?}"),
        })
        .collect();
    assert_eq!(
        texts,
        vec!["alpha".to_owned(), String::new(), "beta".to_owned()]
    );
}

#[tokio::test]
async fn output_stream_lag_after_buffered_bytes_returns_buffered_first() {
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_output_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let mut stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    // A single cursor batch carrying multiple events must drain to the
    // caller in protocol order — the SDK never reorders the daemon-issued
    // sequence stream, even when the events back up in the SDK's internal
    // pending queue.
    let next_task = tokio::spawn(async move {
        let mut chunks = Vec::new();
        for _ in 0..3 {
            chunks.push(stream.next().await.expect("cursor").expect("chunk"));
        }
        (stream, chunks)
    });
    drive_cursor_with_events(
        &mut server_stream,
        vec![
            PaneOutputEvent {
                sequence: 100,
                bytes: b"x".to_vec(),
            },
            PaneOutputEvent {
                sequence: 101,
                bytes: b"y".to_vec(),
            },
            PaneOutputEvent {
                sequence: 102,
                bytes: b"z".to_vec(),
            },
        ],
    )
    .await;
    let (_stream, chunks) = next_task.await.expect("next task");
    let sequences: Vec<u64> = chunks
        .into_iter()
        .map(|chunk| match chunk {
            PaneOutputChunk::Bytes { sequence, .. } => sequence,
            other => panic!("expected bytes chunk, got {other:?}"),
        })
        .collect();
    assert_eq!(sequences, vec![100, 101, 102]);
}

#[tokio::test]
async fn output_stream_does_not_expose_subscription_id_in_debug() {
    // PaneOutputStream's Debug must never leak the daemon-assigned
    // subscription id — only the SDK-owned closed flag and the buffered
    // chunk count are observable.
    let (client_stream, mut server_stream) = tokio::io::duplex(8192);
    let target = alpha_target();
    let proto_target = target.to_proto();
    let subscribe_task = tokio::spawn(async move {
        pane_output_stream_from_duplex(target, client_stream, PaneOutputStart::Now).await
    });
    drive_subscribe_response(
        &mut server_stream,
        &proto_target,
        PaneOutputSubscriptionStart::Now,
    )
    .await;
    let stream = subscribe_task
        .await
        .expect("subscribe task")
        .expect("subscribe succeeds");

    let rendered = format!("{stream:?}");
    let id_rendering = SUBSCRIPTION_ID_RAW.to_string();
    assert!(
        !rendered.contains(&id_rendering) && !rendered.contains("subscription_id"),
        "Debug must not leak subscription id; got `{rendered}`"
    );
    drop(stream);
}
