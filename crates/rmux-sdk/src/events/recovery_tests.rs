#![cfg(any(unix, windows))]

use std::time::Duration;

use rmux_proto::{
    encode_frame, FrameDecoder, HasSessionRequest, HasSessionResponse, PaneOutputSubscriptionId,
    PaneRawBytes, PaneRawRebase as ProtoRebase, PaneRawRebaseReason as ProtoReason,
    PaneRecoveryCoverage as ProtoCoverage, PaneSnapshotCell, PaneSnapshotCursor,
    PaneSnapshotResponse, PaneStreamCursorRequest, PaneStreamCursorResponse, PaneStreamEndReason,
    PaneStreamEvent, PaneStreamLifecycleEvent as ProtoLifecycle, PaneStreamMode,
    PaneSurfaceDynamicColors, PaneSurfaceFrame, PaneSurfaceSnapshot, PaneTarget, PaneTargetRef,
    Request, Response, SessionName, SubscribePaneStreamRequest, SubscribePaneStreamResponse,
    UnsubscribePaneStreamRequest, UnsubscribePaneStreamResponse,
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

fn session(name: &str) -> SessionName {
    SessionName::new(name).expect("valid session name")
}

fn pane_id() -> PaneId {
    PaneId::new(1)
}

fn subscribed(event: PaneStreamEvent) -> SubscribePaneStreamResponse {
    SubscribePaneStreamResponse {
        subscription_id: subscription_id(),
        target: target(),
        pane_id: pane_id(),
        event,
    }
}

fn initial_subscribed() -> SubscribePaneStreamResponse {
    subscribed(PaneStreamEvent::RawRebase(Box::new(rebase(
        1,
        ProtoReason::Initial,
    ))))
}

fn glyph_cell() -> PaneSnapshotCell {
    PaneSnapshotCell {
        text: "x".to_owned(),
        width: 1,
        padding: false,
        attributes: 0,
        fg: 8,
        bg: 8,
        us: 8,
        link: 0,
    }
}

/// A well-formed frame from the sibling Surface projection, used as a negative
/// control: a raw stream must refuse it before its own mapper is consulted.
fn surface_frame() -> PaneSurfaceFrame {
    PaneSurfaceFrame {
        epoch: 1,
        revision: 1,
        next_output_sequence: 5,
        snapshot: PaneSurfaceSnapshot {
            cols: 1,
            rows: 1,
            cells: vec![glyph_cell()],
            hyperlinks: Vec::new(),
            cursor: PaneSnapshotCursor {
                row: 0,
                col: 0,
                visible: true,
                style: 0,
            },
            title: String::new(),
            path: String::new(),
            dynamic_colors: PaneSurfaceDynamicColors::default(),
            metadata_complete: true,
            mode_bits: 0,
            alternate: false,
            scroll_top: 0,
            scroll_bottom: 0,
            history_size: 0,
            history_bytes: 0,
            revision: 1,
        },
    }
}

async fn serve_subscribe_response(
    server: &mut DuplexStream,
    response: SubscribePaneStreamResponse,
) {
    assert!(matches!(
        read_request(server).await,
        Request::SubscribePaneStream(_)
    ));
    write_response(server, Response::SubscribePaneStream(Box::new(response))).await;
}

/// Asserts that a rejection released the reserved subscription exactly once.
async fn expect_single_unsubscribe(server: &mut DuplexStream, context: &str) {
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), read_request(server))
            .await
            .unwrap_or_else(|_| panic!("{context} must schedule cleanup")),
        Request::UnsubscribePaneStream(UnsubscribePaneStreamRequest {
            subscription_id: subscription_id(),
        })
    );
    write_response(
        server,
        Response::UnsubscribePaneStream(UnsubscribePaneStreamResponse {
            subscription_id: subscription_id(),
            removed: true,
        }),
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), read_request(server))
            .await
            .is_err(),
        "{context} must emit exactly one unsubscribe"
    );
}

/// Drives one fail-closed admission: the daemon answers `response`, the open
/// is refused with `expected`, and the reserved subscription is released once.
async fn expect_rejected_admission(
    requested: PaneTargetRef,
    response: SubscribePaneStreamResponse,
    expected: &str,
) {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let transport = TransportClient::spawn(client);
    let shared_transport = transport.clone();
    let subscribe_target = requested.clone();
    let open = tokio::spawn(async move {
        PaneRecoveryStream::open(transport, requested, PaneRecoveryOptions::default()).await
    });

    assert_eq!(
        read_request(&mut server).await,
        Request::SubscribePaneStream(SubscribePaneStreamRequest {
            target: subscribe_target,
            mode: PaneStreamMode::Raw,
            include_snapshot: false,
        })
    );
    write_response(
        &mut server,
        Response::SubscribePaneStream(Box::new(response)),
    )
    .await;

    let error = open
        .await
        .expect("stream open task joins")
        .expect_err("a rejected recovery response must not open a stream");
    assert!(
        error.to_string().contains(expected),
        "unexpected error: {error}"
    );
    expect_single_unsubscribe(&mut server, "a refused recovery admission").await;
    drop(shared_transport);
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
async fn raw_slow_consumer_end_is_terminal_in_the_sdk_stream() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        serve_subscribe(&mut server).await;
        assert!(matches!(
            read_request(&mut server).await,
            Request::PaneStreamCursor(PaneStreamCursorRequest {
                subscription_id: id,
                ..
            }) if id == subscription_id()
        ));
        write_response(
            &mut server,
            Response::PaneStreamCursor(Box::new(PaneStreamCursorResponse {
                subscription_id: subscription_id(),
                events: vec![PaneStreamEvent::End(PaneStreamEndReason::SlowConsumer)],
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
        write_response(
            &mut server,
            Response::UnsubscribePaneStream(UnsubscribePaneStreamResponse {
                subscription_id: subscription_id(),
                removed: true,
            }),
        )
        .await;
    });

    let mut stream = open_stream(client).await;
    assert!(matches!(
        stream.next().await.expect("initial event"),
        Some(PaneRecoveryEvent::Rebase(_))
    ));
    assert_eq!(
        stream.poll_once().await.expect("typed Raw end"),
        vec![PaneRecoveryEvent::End(SdkEndReason::SlowConsumer)]
    );
    assert_eq!(stream.next().await.expect("closed Raw stream"), None);
    drop(stream);
    server_task.await.expect("Raw server task");
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

/// Pins the atomic rebase/raw-byte boundary proposed by Prabir Shrestha in #139.
#[test]
fn recovery_state_owns_rebase_bytes_lifecycle_and_end_ordering() {
    let mut state = PaneRecoveryState::default();
    let initial = PaneRecoveryEvent::Rebase(
        super::rebase_from_proto(rebase(1, ProtoReason::Initial)).expect("initial rebase"),
    );
    state.apply(&initial).expect("initial rebase");
    assert_eq!(state.next_sequence(), Some(5));
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

#[tokio::test]
async fn recovery_rejects_a_response_for_a_foreign_pane_id() {
    let mut response = initial_subscribed();
    response.pane_id = PaneId::new(9);

    expect_rejected_admission(
        PaneTargetRef::by_id(session("recovery"), pane_id()),
        response,
        "opened a pane stream for pane %9 instead of requested pane %1",
    )
    .await;
}

#[tokio::test]
async fn recovery_rejects_a_response_for_a_foreign_slot_target() {
    let mut response = initial_subscribed();
    response.target = PaneTarget::with_window(session("recovery"), 3, 4);

    expect_rejected_admission(
        PaneTargetRef::slot(target()),
        response,
        "opened a pane stream for recovery:3.4 instead of requested recovery:0.0",
    )
    .await;
}

#[tokio::test]
async fn recovery_rejects_an_initial_event_that_is_neither_a_rebase_nor_a_typed_end() {
    let events = [
        (
            PaneStreamEvent::RawBytes(PaneRawBytes {
                epoch: 1,
                sequence: 5,
                bytes: b"live".to_vec(),
            }),
            "raw-bytes",
        ),
        (
            PaneStreamEvent::Lifecycle(ProtoLifecycle::ProcessExited {
                output_sequence: Some(5),
            }),
            "lifecycle",
        ),
        (
            PaneStreamEvent::SurfaceReset(Box::new(surface_frame())),
            "surface-reset",
        ),
        (
            PaneStreamEvent::SurfacePatch(Box::new(surface_frame())),
            "surface-patch",
        ),
    ];

    for (event, kind) in events {
        expect_rejected_admission(
            PaneTargetRef::slot(target()),
            subscribed(event),
            &format!(
                "raw recovery stream opened with a {kind} event instead of a rebase or a typed end"
            ),
        )
        .await;
    }
}

/// A revocation that lands between admission and response is a deliberate,
/// typed server decision: the daemon has already removed the subscription and
/// answers the opening request with its terminal reason instead of a keyframe.
#[tokio::test]
async fn recovery_admits_a_revoked_access_end_without_exposing_or_releasing_anything() {
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

    serve_subscribe_response(
        &mut server,
        subscribed(PaneStreamEvent::End(PaneStreamEndReason::AccessRevoked)),
    )
    .await;

    let mut stream = open
        .await
        .expect("stream open task joins")
        .expect("a revoked opening response is a typed end, not protocol drift");
    assert_eq!(
        stream.next().await.expect("typed revocation"),
        Some(PaneRecoveryEvent::End(SdkEndReason::AccessRevoked))
    );
    assert_eq!(stream.next().await.expect("closed recovery stream"), None);
    drop(stream);

    assert!(
        tokio::time::timeout(Duration::from_millis(30), read_request(&mut server))
            .await
            .is_err(),
        "a subscription the daemon already released must be neither polled nor unsubscribed"
    );
    drop(shared_transport);
}

#[tokio::test]
async fn recovery_rejects_an_initial_rebase_that_is_not_the_stream_start() {
    expect_rejected_admission(
        PaneTargetRef::slot(target()),
        subscribed(PaneStreamEvent::RawRebase(Box::new(rebase(
            1,
            ProtoReason::Lag,
        )))),
        "raw recovery stream opened with a Lag rebase instead of an initial one",
    )
    .await;
}

#[tokio::test]
async fn recovery_rejects_a_keyframe_without_geometry_or_bytes() {
    let mut without_geometry = rebase(1, ProtoReason::Initial);
    without_geometry.cols = 0;
    expect_rejected_admission(
        PaneTargetRef::slot(target()),
        subscribed(PaneStreamEvent::RawRebase(Box::new(without_geometry))),
        "raw recovery rebase reported an empty 0x24 keyframe geometry",
    )
    .await;

    let mut without_bytes = rebase(1, ProtoReason::Initial);
    without_bytes.keyframe = Vec::new();
    expect_rejected_admission(
        PaneTargetRef::slot(target()),
        subscribed(PaneStreamEvent::RawRebase(Box::new(without_bytes))),
        "raw recovery rebase carried no keyframe bytes",
    )
    .await;
}

#[tokio::test]
async fn recovery_rejects_a_snapshot_that_contradicts_the_keyframe_geometry() {
    let mut mismatched = rebase(1, ProtoReason::Initial);
    mismatched.snapshot = Some(PaneSnapshotResponse {
        cols: 2,
        rows: 1,
        cells: vec![glyph_cell(), glyph_cell()],
        cursor: PaneSnapshotCursor {
            row: 0,
            col: 0,
            visible: true,
            style: 0,
        },
        revision: 1,
    });

    expect_rejected_admission(
        PaneTargetRef::slot(target()),
        subscribed(PaneStreamEvent::RawRebase(Box::new(mismatched))),
        "raw recovery rebase snapshot geometry 2x1 does not match its 80x24 keyframe",
    )
    .await;
}

#[tokio::test]
async fn recovery_rejects_a_noncontiguous_byte_sequence_before_exposing_it() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let transport = TransportClient::spawn(client);
    let shared_transport = transport.clone();
    let consumer = tokio::spawn(async move {
        let mut stream = PaneRecoveryStream::open(
            transport,
            PaneTargetRef::slot(target()),
            PaneRecoveryOptions::default(),
        )
        .await
        .expect("open recovery stream");
        assert!(matches!(
            stream.next().await.expect("initial event"),
            Some(PaneRecoveryEvent::Rebase(_))
        ));
        let error = stream
            .next()
            .await
            .expect_err("a sequence hole must not reach the consumer");
        assert!(
            error
                .to_string()
                .contains("raw recovery expected output sequence 5, got 6"),
            "unexpected error: {error}"
        );
        // The stream stays closed instead of resuming a desynchronized epoch.
        assert_eq!(stream.next().await.expect("closed stream"), None);
        drop(stream);
    });

    serve_subscribe_response(&mut server, initial_subscribed()).await;
    assert!(matches!(
        read_request(&mut server).await,
        Request::PaneStreamCursor(_)
    ));
    write_response(
        &mut server,
        Response::PaneStreamCursor(Box::new(PaneStreamCursorResponse {
            subscription_id: subscription_id(),
            events: vec![PaneStreamEvent::RawBytes(PaneRawBytes {
                epoch: 1,
                sequence: 6,
                bytes: b"hole".to_vec(),
            })],
            limited: false,
        })),
    )
    .await;

    expect_single_unsubscribe(&mut server, "a broken byte sequence").await;
    consumer.await.expect("consumer task");
    drop(shared_transport);
}

#[tokio::test]
async fn recovery_rejects_bytes_from_a_foreign_epoch_before_exposing_them() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let transport = TransportClient::spawn(client);
    let shared_transport = transport.clone();
    let consumer = tokio::spawn(async move {
        let mut stream = PaneRecoveryStream::open(
            transport,
            PaneTargetRef::slot(target()),
            PaneRecoveryOptions::default(),
        )
        .await
        .expect("open recovery stream");
        assert!(matches!(
            stream.next().await.expect("initial event"),
            Some(PaneRecoveryEvent::Rebase(_))
        ));
        let error = stream
            .poll_once()
            .await
            .expect_err("a foreign epoch must not reach the consumer");
        assert!(
            error
                .to_string()
                .contains("raw recovery byte epoch 2 does not match 1"),
            "unexpected error: {error}"
        );
        drop(stream);
    });

    serve_subscribe_response(&mut server, initial_subscribed()).await;
    assert!(matches!(
        read_request(&mut server).await,
        Request::PaneStreamCursor(_)
    ));
    write_response(
        &mut server,
        Response::PaneStreamCursor(Box::new(PaneStreamCursorResponse {
            subscription_id: subscription_id(),
            events: vec![PaneStreamEvent::RawBytes(PaneRawBytes {
                epoch: 2,
                sequence: 5,
                bytes: b"stray".to_vec(),
            })],
            limited: false,
        })),
    )
    .await;

    expect_single_unsubscribe(&mut server, "a foreign byte epoch").await;
    consumer.await.expect("consumer task");
    drop(shared_transport);
}

#[tokio::test]
async fn recovery_rejects_an_in_band_rebase_that_does_not_advance_the_epoch() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let transport = TransportClient::spawn(client);
    let shared_transport = transport.clone();
    let consumer = tokio::spawn(async move {
        let mut stream = PaneRecoveryStream::open(
            transport,
            PaneTargetRef::slot(target()),
            PaneRecoveryOptions::default(),
        )
        .await
        .expect("open recovery stream");
        assert!(matches!(
            stream.next().await.expect("initial event"),
            Some(PaneRecoveryEvent::Rebase(_))
        ));
        let error = stream
            .next()
            .await
            .expect_err("a stale rebase must not reach the consumer");
        assert!(
            error
                .to_string()
                .contains("raw recovery rebase epoch 1 does not advance 1"),
            "unexpected error: {error}"
        );
        drop(stream);
    });

    serve_subscribe_response(&mut server, initial_subscribed()).await;
    assert!(matches!(
        read_request(&mut server).await,
        Request::PaneStreamCursor(_)
    ));
    write_response(
        &mut server,
        Response::PaneStreamCursor(Box::new(PaneStreamCursorResponse {
            subscription_id: subscription_id(),
            events: vec![PaneStreamEvent::RawRebase(Box::new(rebase(
                1,
                ProtoReason::Resize,
            )))],
            limited: false,
        })),
    )
    .await;

    expect_single_unsubscribe(&mut server, "a stale in-band rebase").await;
    consumer.await.expect("consumer task");
    drop(shared_transport);
}

#[tokio::test]
async fn recovery_accepts_a_rekeyed_session_and_a_moved_pane_slot() {
    // A slot subscription keeps its window and pane index while the daemon
    // rekeys a renamed runtime session underneath the opening stream.
    let mut renamed = initial_subscribed();
    renamed.target = PaneTarget::with_window(session("renamed"), 0, 0);
    expect_admitted_open(PaneTargetRef::slot(target()), renamed).await;

    // A stable-id subscription keeps its pane while the resolved slot moves
    // to another window of another session.
    let mut moved = initial_subscribed();
    moved.target = PaneTarget::with_window(session("elsewhere"), 7, 2);
    expect_admitted_open(PaneTargetRef::by_id(session("recovery"), pane_id()), moved).await;
}

/// Opens a stream against a legitimate response and drops it cleanly.
async fn expect_admitted_open(requested: PaneTargetRef, response: SubscribePaneStreamResponse) {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let transport = TransportClient::spawn(client);
    let shared_transport = transport.clone();
    let consumer = tokio::spawn(async move {
        let mut stream =
            PaneRecoveryStream::open(transport, requested, PaneRecoveryOptions::default())
                .await
                .expect("a legitimate rekey must keep the stream open");
        assert!(matches!(
            stream.next().await.expect("initial event"),
            Some(PaneRecoveryEvent::Rebase(_))
        ));
        drop(stream);
    });

    serve_subscribe_response(&mut server, response).await;
    expect_single_unsubscribe(&mut server, "dropping an admitted stream").await;
    consumer.await.expect("consumer task");
    drop(shared_transport);
}

fn lag_rebase() -> ProtoRebase {
    let mut lag = rebase(2, ProtoReason::Lag);
    lag.next_sequence = 40;
    lag
}

#[tokio::test]
async fn recovery_keeps_an_in_band_lag_rebase_and_its_repositioned_bytes() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let transport = TransportClient::spawn(client);
    let shared_transport = transport.clone();
    let consumer = tokio::spawn(async move {
        let mut stream = PaneRecoveryStream::open(
            transport,
            PaneTargetRef::slot(target()),
            PaneRecoveryOptions::default(),
        )
        .await
        .expect("open recovery stream");
        assert!(matches!(
            stream.next().await.expect("initial event"),
            Some(PaneRecoveryEvent::Rebase(_))
        ));
        assert_eq!(
            stream.poll_once().await.expect("in-band lag recovery"),
            vec![
                PaneRecoveryEvent::Bytes {
                    epoch: 1,
                    sequence: 5,
                    bytes: b"before".to_vec(),
                },
                PaneRecoveryEvent::Rebase(
                    super::rebase_from_proto(lag_rebase()).expect("lag rebase"),
                ),
                PaneRecoveryEvent::Bytes {
                    epoch: 2,
                    sequence: 40,
                    bytes: b"after".to_vec(),
                },
            ]
        );
        drop(stream);
    });

    serve_subscribe_response(&mut server, initial_subscribed()).await;
    assert!(matches!(
        read_request(&mut server).await,
        Request::PaneStreamCursor(_)
    ));
    write_response(
        &mut server,
        Response::PaneStreamCursor(Box::new(PaneStreamCursorResponse {
            subscription_id: subscription_id(),
            events: vec![
                PaneStreamEvent::RawBytes(PaneRawBytes {
                    epoch: 1,
                    sequence: 5,
                    bytes: b"before".to_vec(),
                }),
                PaneStreamEvent::RawRebase(Box::new(lag_rebase())),
                PaneStreamEvent::RawBytes(PaneRawBytes {
                    epoch: 2,
                    sequence: 40,
                    bytes: b"after".to_vec(),
                }),
            ],
            limited: false,
        })),
    )
    .await;

    expect_single_unsubscribe(&mut server, "dropping a recovered stream").await;
    consumer.await.expect("consumer task");
    drop(shared_transport);
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

#[test]
fn recovery_state_lets_graphics_events_annotate_the_adjacent_sequence() {
    let mut state = PaneRecoveryState::default();
    state
        .apply(&PaneRecoveryEvent::Rebase(
            super::rebase_from_proto(rebase(1, ProtoReason::Initial)).expect("initial rebase"),
        ))
        .expect("initial rebase");

    let graphics = PaneRecoveryEvent::Graphics {
        epoch: 1,
        sequence: 5,
        kind: rmux_proto::PanePassthroughKind::KittyGraphics,
        cursor_x: 2,
        cursor_y: 0,
        bytes: b"\x1b_Gf=100;AAAA\x1b\\".to_vec(),
    };
    state.apply(&graphics).expect("graphics advance no state");
    assert_eq!(state.next_sequence(), Some(5), "graphics are annotations");

    state
        .apply(&PaneRecoveryEvent::Bytes {
            epoch: 1,
            sequence: 5,
            bytes: b"\x1b_Gf=100;AAAA\x1b\\".to_vec(),
        })
        .expect("the adjacent bytes event advances the sequence");
    assert_eq!(state.next_sequence(), Some(6));
}

#[tokio::test]
async fn graphics_passthrough_events_surface_as_typed_sdk_events() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        serve_subscribe(&mut server).await;
        assert!(matches!(
            read_request(&mut server).await,
            Request::PaneStreamCursor(PaneStreamCursorRequest {
                subscription_id: id,
                ..
            }) if id == subscription_id()
        ));
        write_response(
            &mut server,
            Response::PaneStreamCursor(Box::new(PaneStreamCursorResponse {
                subscription_id: subscription_id(),
                events: vec![
                    PaneStreamEvent::GraphicsPassthrough(rmux_proto::PaneGraphicsPassthrough {
                        epoch: 1,
                        sequence: 5,
                        kind: rmux_proto::PanePassthroughKind::KittyGraphics,
                        cursor_x: 2,
                        cursor_y: 0,
                        bytes: b"\x1b_Gf=100;AAAA\x1b\\".to_vec(),
                    }),
                    PaneStreamEvent::RawBytes(PaneRawBytes {
                        epoch: 1,
                        sequence: 5,
                        bytes: b"\x1b_Gf=100;AAAA\x1b\\".to_vec(),
                    }),
                ],
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
    });

    let mut stream = open_stream(client).await;
    assert!(matches!(
        stream.next().await.expect("initial event"),
        Some(PaneRecoveryEvent::Rebase(_))
    ));

    assert_eq!(
        stream.next().await.expect("graphics event"),
        Some(PaneRecoveryEvent::Graphics {
            epoch: 1,
            sequence: 5,
            kind: rmux_proto::PanePassthroughKind::KittyGraphics,
            cursor_x: 2,
            cursor_y: 0,
            bytes: b"\x1b_Gf=100;AAAA\x1b\\".to_vec(),
        })
    );
    assert_eq!(
        stream.next().await.expect("raw chunk"),
        Some(PaneRecoveryEvent::Bytes {
            epoch: 1,
            sequence: 5,
            bytes: b"\x1b_Gf=100;AAAA\x1b\\".to_vec(),
        })
    );
    drop(stream);
    server_task.await.expect("server task");
}
