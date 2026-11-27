use super::{alpha, answer_handshake, read_request, state_event_capabilities, write_response};
use crate::handles::pane::state_events::PaneStateEventStream;
use crate::transport::TransportClient;
use crate::{
    Pane, PaneId, PaneRef, PaneStateClosedReason, PaneStateEvent, PaneStateEventsOptions,
    PaneStateOption, RmuxEndpoint,
};
use rmux_proto::{
    CommandOutput, ListPanesResponse, PaneForegroundStateResponse, PaneOptionEntry,
    PaneStateClosedReason as WireReason, PaneStateCursorResponse, PaneStateEventDto,
    PaneStateLagResponse, PaneStateSnapshot, PaneStateSubscriptionId, Request, Response,
    SubscribePaneStateResponse, CAPABILITY_SDK_PANE_BY_ID, CAPABILITY_SDK_PANE_FOREGROUND,
};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};

#[path = "state_value_cases.rs"]
mod fixtures;

fn capabilities() -> Vec<String> {
    let mut capabilities = state_event_capabilities();
    capabilities.push(CAPABILITY_SDK_PANE_FOREGROUND.into());
    capabilities.push(CAPABILITY_SDK_PANE_BY_ID.into());
    capabilities
}

fn pane(transport: TransportClient) -> Pane {
    Pane::new(
        PaneRef::in_first_window(alpha(), 0),
        RmuxEndpoint::Default,
        None,
        transport,
    )
}

#[tokio::test]
async fn foreground_queries_preserve_every_source_and_optional_field() {
    for (wire, expected) in fixtures::cases() {
        let (client, mut server) = tokio::io::duplex(8192);
        let pane = pane(TransportClient::spawn(client));
        let server = tokio::spawn(async move {
            for query in 0..2 {
                assert!(matches!(
                    read_request(&mut server).await,
                    Request::ListPanes(_)
                ));
                write_response(
                    &mut server,
                    Response::ListPanes(ListPanesResponse {
                        output: CommandOutput::from_stdout("0:0:%42\n"),
                    }),
                )
                .await;
                if query == 0 {
                    answer_handshake(&mut server, capabilities()).await;
                }
                match read_request(&mut server).await {
                    Request::PaneForegroundState(request) => assert_eq!(
                        request.target,
                        rmux_proto::PaneTargetRef::by_id(alpha(), PaneId::new(42)),
                    ),
                    other => panic!("expected foreground query, got {other:?}"),
                }
                write_response(
                    &mut server,
                    Response::PaneForegroundState(Box::new(PaneForegroundStateResponse {
                        pane_id: PaneId::new(42),
                        revision: 901,
                        state: wire.clone(),
                    })),
                )
                .await;
            }
        });
        assert_eq!(
            timeout(Duration::from_secs(2), pane.foreground_state())
                .await
                .unwrap()
                .unwrap(),
            expected,
        );
        assert_eq!(
            timeout(
                Duration::from_secs(2),
                pane.foreground_state_with_revision()
            )
            .await
            .unwrap()
            .unwrap(),
            expected.map(|state| (PaneId::new(42), 901, state)),
        );
        server.await.unwrap();
    }
}

async fn stream(
    snapshot: PaneStateSnapshot,
    responses: Vec<(u64, Response)>,
) -> (PaneStateEventStream, JoinHandle<()>, JoinHandle<()>) {
    let (main_client, mut main_server) = tokio::io::duplex(8192);
    let (cursor_client, mut cursor_server) = tokio::io::duplex(8192);
    let pane = pane(TransportClient::spawn(main_client));
    let main = tokio::spawn(async move {
        answer_handshake(&mut main_server, capabilities()).await;
    });
    let cursor = tokio::spawn(async move {
        answer_handshake(&mut cursor_server, capabilities()).await;
        match read_request(&mut cursor_server).await {
            Request::SubscribePaneState(request) => {
                assert!(request.include_title);
                assert!(request.include_options);
                assert!(request.include_foreground);
            }
            other => panic!("expected subscription, got {other:?}"),
        }
        write_response(
            &mut cursor_server,
            Response::SubscribePaneState(Box::new(SubscribePaneStateResponse {
                subscription_id: PaneStateSubscriptionId::new(7),
                pane_id: PaneId::new(42),
                snapshot,
            })),
        )
        .await;
        for (after_revision, response) in responses {
            match read_request(&mut cursor_server).await {
                Request::PaneStateCursor(request) => {
                    assert_eq!(request.subscription_id, PaneStateSubscriptionId::new(7));
                    assert_eq!(request.after_revision, after_revision);
                    assert_eq!(request.max_events, Some(256));
                    assert!(request.wait);
                }
                other => panic!("expected cursor, got {other:?}"),
            }
            write_response(&mut cursor_server, response).await;
        }
    });
    let stream = timeout(
        Duration::from_secs(2),
        PaneStateEventStream::open_with_cursor_transport(
            &pane,
            PaneStateEventsOptions {
                include_foreground: true,
                ..Default::default()
            },
            TransportClient::spawn(cursor_client),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    (stream, main, cursor)
}

async fn next(stream: &mut PaneStateEventStream) -> Option<PaneStateEvent> {
    timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
}

fn batch(events: Vec<PaneStateEventDto>, next_revision: u64) -> Response {
    Response::PaneStateCursor(PaneStateCursorResponse {
        subscription_id: PaneStateSubscriptionId::new(7),
        events,
        next_revision,
    })
}

#[tokio::test]
async fn snapshots_rebases_and_foreground_transitions_preserve_literal_values() {
    let cases = fixtures::cases();
    for (index, (wire, expected)) in cases.iter().enumerate() {
        let (new_wire, new_expected) = &cases[(index + 1) % cases.len()];
        let snapshot = PaneStateSnapshot {
            revision: 10,
            title: Some(" initial λ ".into()),
            options: vec![
                PaneOptionEntry {
                    name: "@second".into(),
                    value: " 2 ".into(),
                },
                PaneOptionEntry {
                    name: "@first".into(),
                    value: "".into(),
                },
            ],
            foreground: wire.clone(),
        };
        let lag = Response::PaneStateLag(Box::new(PaneStateLagResponse {
            subscription_id: PaneStateSubscriptionId::new(7),
            missed_from_revision: 10,
            resume_revision: 20,
            snapshot: PaneStateSnapshot {
                revision: 21,
                title: None,
                options: vec![],
                foreground: new_wire.clone(),
            },
        }));
        let events = vec![
            PaneStateEventDto::ForegroundChanged {
                revision: 22,
                pane_id: PaneId::new(42),
                old_state: wire.clone().unwrap_or_default(),
                new_state: new_wire.clone().unwrap_or_default(),
            },
            PaneStateEventDto::ForegroundChanged {
                revision: 23,
                pane_id: PaneId::new(42),
                old_state: new_wire.clone().unwrap_or_default(),
                new_state: wire.clone().unwrap_or_default(),
            },
        ];
        let (mut stream, main, cursor) =
            stream(snapshot, vec![(10, lag), (21, batch(events, 23))]).await;
        assert_eq!(
            next(&mut stream).await,
            Some(PaneStateEvent::Snapshot {
                revision: 10,
                pane_id: PaneId::new(42),
                title: Some(" initial λ ".into()),
                options: vec![
                    PaneStateOption {
                        name: "@second".into(),
                        value: " 2 ".into()
                    },
                    PaneStateOption {
                        name: "@first".into(),
                        value: "".into()
                    },
                ],
                foreground: expected.clone(),
            })
        );
        assert_eq!(
            next(&mut stream).await,
            Some(PaneStateEvent::Lagged {
                missed_from_revision: 10,
                resume_revision: 20,
            })
        );
        assert_eq!(
            next(&mut stream).await,
            Some(PaneStateEvent::Snapshot {
                revision: 21,
                pane_id: PaneId::new(42),
                title: None,
                options: vec![],
                foreground: new_expected.clone(),
            })
        );
        assert_eq!(
            next(&mut stream).await,
            Some(PaneStateEvent::ForegroundChanged {
                revision: 22,
                pane_id: PaneId::new(42),
                old_state: expected.clone().unwrap_or_default(),
                new_state: new_expected.clone().unwrap_or_default(),
            })
        );
        assert_eq!(
            next(&mut stream).await,
            Some(PaneStateEvent::ForegroundChanged {
                revision: 23,
                pane_id: PaneId::new(42),
                old_state: new_expected.clone().unwrap_or_default(),
                new_state: expected.clone().unwrap_or_default(),
            })
        );
        main.await.unwrap();
        cursor.await.unwrap();
    }
}

#[tokio::test]
async fn close_reasons_preserve_identity_revision_and_stream_siblings() {
    for (wire, expected) in [
        (WireReason::Exited, PaneStateClosedReason::Exited),
        (WireReason::DiedKept, PaneStateClosedReason::DiedKept),
        (WireReason::Killed, PaneStateClosedReason::Killed),
    ] {
        let pane_id = PaneId::new(73);
        let events = vec![
            PaneStateEventDto::TitleChanged {
                revision: 11,
                pane_id,
                old_title: " old ".into(),
                new_title: "".into(),
            },
            PaneStateEventDto::OptionSet {
                revision: 12,
                pane_id,
                name: "@x".into(),
                old_value: None,
                new_value: "".into(),
            },
            PaneStateEventDto::OptionSet {
                revision: 13,
                pane_id,
                name: "@x".into(),
                old_value: Some("".into()),
                new_value: " λ ".into(),
            },
            PaneStateEventDto::OptionUnset {
                revision: 14,
                pane_id,
                name: "@x".into(),
                old_value: Some(" λ ".into()),
            },
            PaneStateEventDto::OptionUnset {
                revision: 15,
                pane_id,
                name: "@x".into(),
                old_value: None,
            },
            PaneStateEventDto::Closed {
                revision: 16,
                pane_id,
                reason: wire,
            },
            PaneStateEventDto::TitleChanged {
                revision: 17,
                pane_id,
                old_title: "".into(),
                new_title: "discarded".into(),
            },
        ];
        let expected = [
            PaneStateEvent::Snapshot {
                revision: 10,
                pane_id: PaneId::new(42),
                title: None,
                options: vec![],
                foreground: None,
            },
            PaneStateEvent::TitleChanged {
                revision: 11,
                pane_id,
                old_title: " old ".into(),
                new_title: "".into(),
            },
            PaneStateEvent::OptionSet {
                revision: 12,
                pane_id,
                name: "@x".into(),
                old_value: None,
                new_value: "".into(),
            },
            PaneStateEvent::OptionSet {
                revision: 13,
                pane_id,
                name: "@x".into(),
                old_value: Some("".into()),
                new_value: " λ ".into(),
            },
            PaneStateEvent::OptionUnset {
                revision: 14,
                pane_id,
                name: "@x".into(),
                old_value: Some(" λ ".into()),
            },
            PaneStateEvent::OptionUnset {
                revision: 15,
                pane_id,
                name: "@x".into(),
                old_value: None,
            },
            PaneStateEvent::Closed {
                revision: 16,
                pane_id,
                reason: expected,
            },
        ];
        let (mut stream, main, cursor) = stream(
            PaneStateSnapshot {
                revision: 10,
                ..Default::default()
            },
            vec![(10, batch(events, 17))],
        )
        .await;
        for event in expected {
            assert_eq!(next(&mut stream).await, Some(event));
        }
        assert_eq!(next(&mut stream).await, None);
        assert_eq!(next(&mut stream).await, None);
        main.await.unwrap();
        cursor.await.unwrap();
    }
}
