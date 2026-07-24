#![cfg(any(unix, windows))]

use std::time::Duration;

use rmux_proto::{
    encode_frame, FrameDecoder, HasSessionRequest, HasSessionResponse, PaneOutputSubscriptionId,
    PaneRawBytes, PaneRawRebase as ProtoRebase, PaneRawRebaseReason as ProtoReason,
    PaneRecoveryCoverage as ProtoCoverage, PaneSnapshotCursor, PaneSnapshotResponse,
    PaneStreamCursorRequest, PaneStreamCursorResponse, PaneStreamEndReason, PaneStreamEvent,
    PaneStreamMode, PaneTarget, PaneTargetRef, Request, Response, SessionName,
    SubscribePaneStreamRequest, SubscribePaneStreamResponse, UnsubscribePaneStreamRequest,
    UnsubscribePaneStreamResponse,
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
        coverage: ProtoCoverage {
            history_rows_total: 0,
            history_rows_included: 0,
            metadata_complete: true,
        },
        snapshot: None,
        reason,
    }
}

#[test]
fn recovery_rebase_maps_explicit_bounded_coverage() {
    let mut wire = rebase(1, ProtoReason::Initial);
    wire.coverage = ProtoCoverage {
        history_rows_total: 50,
        history_rows_included: 12,
        metadata_complete: false,
    };

    let mapped = super::rebase_from_proto(wire).expect("map bounded recovery coverage");

    assert_eq!(mapped.coverage.history_rows_total, 50);
    assert_eq!(mapped.coverage.history_rows_included, 12);
    assert!(!mapped.coverage.history_complete());
    assert!(!mapped.coverage.metadata_complete);
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
async fn invalid_initial_event_unsubscribes_the_reserved_recovery_stream() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let transport = TransportClient::spawn(client);
    let shared_transport = transport.clone();
    let open = tokio::spawn(async move {
        PaneRecoveryStream::open(
            transport,
            PaneTargetRef::slot(target()),
            PaneRecoveryOptions::default(),
        )
        .await
    });

    assert!(matches!(
        read_request(&mut server).await,
        Request::SubscribePaneStream(_)
    ));
    let mut invalid_rebase = rebase(1, ProtoReason::Initial);
    invalid_rebase.snapshot = Some(PaneSnapshotResponse {
        cols: 1,
        rows: 1,
        cells: Vec::new(),
        cursor: PaneSnapshotCursor {
            row: 0,
            col: 0,
            visible: true,
            style: 0,
        },
        revision: 1,
    });
    write_response(
        &mut server,
        Response::SubscribePaneStream(Box::new(SubscribePaneStreamResponse {
            subscription_id: subscription_id(),
            target: target(),
            pane_id: PaneId::new(1),
            event: PaneStreamEvent::RawRebase(Box::new(invalid_rebase)),
        })),
    )
    .await;
    let error = open
        .await
        .expect("stream open task joins")
        .expect_err("surface event must be rejected by a raw recovery stream");
    assert!(
        error
            .to_string()
            .contains("pane-snapshot response had malformed row-major cell shape"),
        "unexpected error: {error}"
    );

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), read_request(&mut server))
            .await
            .expect("invalid initial event must schedule cleanup"),
        Request::UnsubscribePaneStream(UnsubscribePaneStreamRequest {
            subscription_id: subscription_id(),
        })
    );
    write_response(
        &mut server,
        Response::UnsubscribePaneStream(UnsubscribePaneStreamResponse {
            subscription_id: subscription_id(),
            removed: true,
        }),
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), read_request(&mut server))
            .await
            .is_err(),
        "invalid initial event must emit exactly one unsubscribe"
    );
    drop(shared_transport);
}

#[tokio::test]
async fn cancelled_recoverable_open_unsubscribes_once_and_preserves_shared_transport() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let transport = TransportClient::spawn(client);
    let shared_transport = transport.clone();
    let open = tokio::spawn(async move {
        PaneRecoveryStream::open(
            transport,
            PaneTargetRef::slot(target()),
            PaneRecoveryOptions::default(),
        )
        .await
    });

    assert_eq!(
        read_request(&mut server).await,
        Request::SubscribePaneStream(SubscribePaneStreamRequest {
            target: PaneTargetRef::slot(target()),
            mode: PaneStreamMode::Raw,
            include_snapshot: false,
        })
    );
    open.abort();
    assert!(
        matches!(open.await, Err(error) if error.is_cancelled()),
        "open task must report external cancellation"
    );

    let session = SessionName::new("still-aligned").expect("valid session");
    let follow_up_request = Request::HasSession(HasSessionRequest { target: session });
    let follow_up = tokio::spawn({
        let shared_transport = shared_transport.clone();
        let follow_up_request = follow_up_request.clone();
        async move { shared_transport.request(follow_up_request).await }
    });
    assert_eq!(
        read_request(&mut server).await,
        follow_up_request,
        "the shared transport must keep accepting requests while the open is cancelled"
    );

    write_response(
        &mut server,
        Response::SubscribePaneStream(Box::new(SubscribePaneStreamResponse {
            subscription_id: subscription_id(),
            target: target(),
            pane_id: PaneId::new(1),
            event: PaneStreamEvent::RawRebase(Box::new(rebase(1, ProtoReason::Initial))),
        })),
    )
    .await;
    let cleanup = tokio::time::timeout(Duration::from_secs(1), read_request(&mut server))
        .await
        .expect("cancelled recoverable open must schedule cleanup");
    assert_eq!(
        cleanup,
        Request::UnsubscribePaneStream(UnsubscribePaneStreamRequest {
            subscription_id: subscription_id(),
        })
    );
    write_response(
        &mut server,
        Response::HasSession(HasSessionResponse { exists: false }),
    )
    .await;
    assert_eq!(
        follow_up
            .await
            .expect("follow-up task")
            .expect("shared transport remains usable"),
        Response::HasSession(HasSessionResponse { exists: false })
    );
    write_response(
        &mut server,
        Response::UnsubscribePaneStream(UnsubscribePaneStreamResponse {
            subscription_id: subscription_id(),
            removed: true,
        }),
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), read_request(&mut server))
            .await
            .is_err(),
        "cancelled open must emit exactly one unsubscribe"
    );
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
async fn transcript_mutation_rebase_replaces_a_non_replayable_rep_event() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        serve_subscribe(&mut server).await;
        let _ = read_request(&mut server).await;
        let mut post_rep = rebase(2, ProtoReason::TranscriptMutation);
        post_rep.next_sequence = 7;
        post_rep.keyframe = b"\x1b[2J\x1b[HXXX".to_vec();
        write_response(
            &mut server,
            Response::PaneStreamCursor(Box::new(PaneStreamCursorResponse {
                subscription_id: subscription_id(),
                events: vec![PaneStreamEvent::RawRebase(Box::new(post_rep))],
                limited: false,
            })),
        )
        .await;
        let _ = read_request(&mut server).await;
        write_response(
            &mut server,
            Response::PaneStreamCursor(Box::new(PaneStreamCursorResponse {
                subscription_id: subscription_id(),
                events: vec![PaneStreamEvent::RawBytes(PaneRawBytes {
                    epoch: 2,
                    sequence: 7,
                    bytes: b"tail".to_vec(),
                })],
                limited: false,
            })),
        )
        .await;
        let _ = read_request(&mut server).await;
    });

    let mut stream = open_stream(client).await;
    let _ = stream.next().await.expect("initial");
    assert!(matches!(
        stream.next().await.expect("post-REP rebase"),
        Some(PaneRecoveryEvent::Rebase(rebase))
            if rebase.reason == super::PaneRecoveryRebaseReason::TranscriptMutation
                && rebase.next_sequence == 7
    ));
    assert_eq!(
        stream.next().await.expect("post-rebase tail"),
        Some(PaneRecoveryEvent::Bytes {
            epoch: 2,
            sequence: 7,
            bytes: b"tail".to_vec(),
        })
    );
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
