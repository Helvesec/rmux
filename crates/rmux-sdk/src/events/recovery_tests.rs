#![cfg(any(unix, windows))]

use std::time::Duration;

use rmux_proto::{
    encode_frame, FrameDecoder, PaneOutputSubscriptionId, PaneRawBytes,
    PaneRawRebase as ProtoRebase, PaneRawRebaseReason as ProtoReason, PaneStreamCursorRequest,
    PaneStreamCursorResponse, PaneStreamEndReason, PaneStreamEvent, PaneStreamMode, PaneTarget,
    PaneTargetRef, Request, Response, SessionName, SubscribePaneStreamRequest,
    SubscribePaneStreamResponse, UnsubscribePaneStreamRequest,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

use super::{
    PaneRecoveryApplyError, PaneRecoveryEvent, PaneRecoveryOptions, PaneRecoveryState,
    PaneRecoveryStream,
};
use crate::transport::TransportClient;
use crate::{PaneId, PaneStreamEndReason as SdkEndReason};

fn target() -> PaneTarget {
    PaneTarget::with_window(
        SessionName::new("recovery").expect("valid session name"),
        0,
        0,
    )
}

fn subscription_id() -> PaneOutputSubscriptionId {
    PaneOutputSubscriptionId::new(73)
}

fn rebase(epoch: u64, reason: ProtoReason) -> ProtoRebase {
    ProtoRebase {
        epoch,
        generation: 1,
        invalidation_revision: epoch.saturating_sub(1),
        next_sequence: 5,
        cols: 80,
        rows: 24,
        keyframe: b"\x1b[2J\x1b[Hready".to_vec(),
        alternate: false,
        snapshot: None,
        reason,
    }
}

async fn read_request(stream: &mut DuplexStream) -> Request {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 1024];
    loop {
        if let Some(request) = decoder.next_frame().expect("request frame") {
            return request;
        }
        let read = stream.read(&mut buffer).await.expect("read request");
        assert_ne!(read, 0, "transport closed before request");
        decoder.push_bytes(&buffer[..read]);
    }
}

async fn write_response(stream: &mut DuplexStream, response: Response) {
    stream
        .write_all(&encode_frame(&response).expect("response frame"))
        .await
        .expect("write response");
}

async fn open_stream(client: DuplexStream) -> PaneRecoveryStream {
    PaneRecoveryStream::open(
        TransportClient::spawn(client),
        PaneTargetRef::slot(target()),
        PaneRecoveryOptions::default(),
    )
    .await
    .expect("open recovery stream")
}

async fn serve_subscribe(server: &mut DuplexStream) {
    let request = read_request(server).await;
    assert_eq!(
        request,
        Request::SubscribePaneStream(SubscribePaneStreamRequest {
            target: PaneTargetRef::slot(target()),
            mode: PaneStreamMode::Raw,
            include_snapshot: false,
        })
    );
    write_response(
        server,
        Response::SubscribePaneStream(Box::new(SubscribePaneStreamResponse {
            subscription_id: subscription_id(),
            target: target(),
            pane_id: PaneId::new(1),
            event: PaneStreamEvent::RawRebase(Box::new(rebase(1, ProtoReason::Initial))),
        })),
    )
    .await;
}

#[tokio::test]
async fn cancelled_next_reuses_the_inflight_cursor_and_drop_unsubscribes_once() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let (cursor_seen_tx, cursor_seen_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        serve_subscribe(&mut server).await;
        assert!(matches!(
            read_request(&mut server).await,
            Request::PaneStreamCursor(PaneStreamCursorRequest {
                subscription_id: id,
                ..
            }) if id == subscription_id()
        ));
        let _ = cursor_seen_tx.send(());
        let _ = release_rx.await;
        write_response(
            &mut server,
            Response::PaneStreamCursor(Box::new(PaneStreamCursorResponse {
                subscription_id: subscription_id(),
                events: vec![PaneStreamEvent::RawBytes(PaneRawBytes {
                    epoch: 1,
                    sequence: 5,
                    bytes: b"tail".to_vec(),
                })],
                limited: false,
            })),
        )
        .await;
        assert_eq!(
            read_request(&mut server).await,
            Request::UnsubscribePaneStream(UnsubscribePaneStreamRequest {
                subscription_id: subscription_id(),
            })
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), read_request(&mut server))
                .await
                .is_err(),
            "drop must emit exactly one unsubscribe"
        );
    });

    let mut stream = open_stream(client).await;
    assert!(matches!(
        stream.next().await.expect("initial event"),
        Some(PaneRecoveryEvent::Rebase(_))
    ));

    let mut interrupted = Box::pin(stream.next());
    tokio::select! {
        _ = cursor_seen_rx => {}
        result = &mut interrupted => panic!("cursor completed before cancellation: {result:?}"),
    }
    drop(interrupted);
    let _ = release_tx.send(());

    assert_eq!(
        stream.next().await.expect("resumed cursor"),
        Some(PaneRecoveryEvent::Bytes {
            epoch: 1,
            sequence: 5,
            bytes: b"tail".to_vec(),
        })
    );
    drop(stream);
    server_task.await.expect("server task");
}

#[tokio::test]
async fn in_band_rebase_and_typed_end_are_delivered_before_stream_closes() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        serve_subscribe(&mut server).await;
        let _ = read_request(&mut server).await;
        write_response(
            &mut server,
            Response::PaneStreamCursor(Box::new(PaneStreamCursorResponse {
                subscription_id: subscription_id(),
                events: vec![
                    PaneStreamEvent::RawRebase(Box::new(rebase(2, ProtoReason::Resize))),
                    PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved),
                ],
                limited: false,
            })),
        )
        .await;
        let _ = read_request(&mut server).await;
    });

    let mut stream = open_stream(client).await;
    let _ = stream.next().await.expect("initial");
    assert!(matches!(
        stream.next().await.expect("rebase"),
        Some(PaneRecoveryEvent::Rebase(rebase)) if rebase.epoch == 2
    ));
    assert_eq!(
        stream.next().await.expect("end"),
        Some(PaneRecoveryEvent::End(SdkEndReason::PaneRemoved))
    );
    assert_eq!(stream.next().await.expect("closed"), None);
    drop(stream);
    server_task.await.expect("server task");
}

#[tokio::test]
async fn events_after_typed_end_are_rejected_as_protocol_drift() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        serve_subscribe(&mut server).await;
        let _ = read_request(&mut server).await;
        write_response(
            &mut server,
            Response::PaneStreamCursor(Box::new(PaneStreamCursorResponse {
                subscription_id: subscription_id(),
                events: vec![
                    PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved),
                    PaneStreamEvent::RawBytes(PaneRawBytes {
                        epoch: 1,
                        sequence: 5,
                        bytes: b"impossible".to_vec(),
                    }),
                ],
                limited: false,
            })),
        )
        .await;
    });

    let mut stream = open_stream(client).await;
    let _ = stream.next().await.expect("initial");
    let error = stream
        .next()
        .await
        .expect_err("events after typed end must fail");
    assert!(
        error.to_string().contains("events after a terminal end"),
        "{error}"
    );
    server_task.await.expect("server task");
}

#[tokio::test]
async fn detached_transport_loss_becomes_a_typed_terminal_event() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        serve_subscribe(&mut server).await;
        assert!(matches!(
            read_request(&mut server).await,
            Request::PaneStreamCursor(_)
        ));
        drop(server);
    });

    let mut stream = open_stream(client).await;
    let _ = stream.next().await.expect("initial");
    assert_eq!(
        stream.next().await.expect("typed transport end"),
        Some(PaneRecoveryEvent::End(SdkEndReason::TransportLost))
    );
    assert_eq!(stream.next().await.expect("closed"), None);
    server_task.await.expect("server task");
}

#[test]
fn recovery_state_owns_rebase_bytes_lifecycle_and_end_ordering() {
    let mut state = PaneRecoveryState::default();
    let initial = PaneRecoveryEvent::Rebase(
        super::rebase_from_proto(rebase(1, ProtoReason::Initial)).expect("initial rebase"),
    );
    state.apply(&initial).expect("initial rebase");
    state
        .apply(&PaneRecoveryEvent::Bytes {
            epoch: 1,
            sequence: 5,
            bytes: b"tail".to_vec(),
        })
        .expect("contiguous bytes");
    state
        .apply(&PaneRecoveryEvent::Lifecycle(
            super::PaneStreamLifecycleEvent::ProcessExited {
                output_sequence: Some(6),
            },
        ))
        .expect("EOF consumes its zero-byte output sequence");
    assert_eq!(state.next_sequence(), Some(7));

    let mut next = rebase(2, ProtoReason::GenerationChanged);
    next.generation = 2;
    next.invalidation_revision = 1;
    next.next_sequence = 7;
    state
        .apply(&PaneRecoveryEvent::Rebase(
            super::rebase_from_proto(next).expect("next rebase"),
        ))
        .expect("new epoch");
    state
        .apply(&PaneRecoveryEvent::End(SdkEndReason::PaneRemoved))
        .expect("typed end");
    assert_eq!(state.ended(), Some(SdkEndReason::PaneRemoved));
    assert_eq!(
        state.apply(&PaneRecoveryEvent::End(SdkEndReason::PaneRemoved)),
        Err(PaneRecoveryApplyError::AlreadyEnded(
            SdkEndReason::PaneRemoved
        ))
    );
}

#[test]
fn recovered_unsequenced_lifecycle_does_not_advance_the_rebase_boundary() {
    let mut state = PaneRecoveryState::default();
    state
        .apply(&PaneRecoveryEvent::Rebase(
            super::rebase_from_proto(rebase(1, ProtoReason::Initial)).expect("initial rebase"),
        ))
        .expect("initial rebase");
    state
        .apply(&PaneRecoveryEvent::Lifecycle(
            super::PaneStreamLifecycleEvent::ProcessExited {
                output_sequence: None,
            },
        ))
        .expect("recovered lifecycle");
    assert_eq!(state.next_sequence(), Some(5));
    state
        .apply(&PaneRecoveryEvent::Bytes {
            epoch: 1,
            sequence: 5,
            bytes: b"after-rebase".to_vec(),
        })
        .expect("rebase still owns exact continuation");
}

#[test]
fn recovery_state_rejects_wrong_epoch_and_noncontiguous_bytes() {
    let mut state = PaneRecoveryState::default();
    assert_eq!(
        state.apply(&PaneRecoveryEvent::Bytes {
            epoch: 1,
            sequence: 5,
            bytes: Vec::new(),
        }),
        Err(PaneRecoveryApplyError::BeforeRebase)
    );
    state
        .apply(&PaneRecoveryEvent::Rebase(
            super::rebase_from_proto(rebase(1, ProtoReason::Initial)).expect("initial rebase"),
        ))
        .expect("initial rebase");
    assert_eq!(
        state.apply(&PaneRecoveryEvent::Bytes {
            epoch: 2,
            sequence: 5,
            bytes: Vec::new(),
        }),
        Err(PaneRecoveryApplyError::EpochMismatch {
            current_epoch: 1,
            received_epoch: 2,
        })
    );
    assert_eq!(
        state.apply(&PaneRecoveryEvent::Bytes {
            epoch: 1,
            sequence: 6,
            bytes: Vec::new(),
        }),
        Err(PaneRecoveryApplyError::SequenceMismatch {
            expected_sequence: 5,
            received_sequence: 6,
        })
    );
}
