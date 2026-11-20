use std::time::Duration;

use rmux_core::events::PaneOutputSubscriptionKey;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    KillSessionRequest, KillWindowRequest, LinkWindowRequest, MoveWindowRequest, MoveWindowTarget,
    NewSessionRequest, NewWindowRequest, PaneOptionSetRequest, PaneOutputCursorRequest,
    PaneOutputSubscriptionId, PaneOutputSubscriptionStart, PaneStreamCursorRequest,
    PaneStreamEndReason, PaneStreamEvent, PaneStreamLifecycleEvent, PaneStreamMode, PaneTarget,
    PaneTargetRef, Request, RespawnWindowRequest, Response, SessionName, SetOptionMode,
    SplitDirection, SplitWindowRequest, SplitWindowTarget, SubscribePaneOutputRefRequest,
    SubscribePaneStreamRequest, UnlinkWindowRequest, WindowTarget,
};

use super::RequestHandler;

const CONNECTION_ID: u64 = 831;
/// Output published to a pane after its subscription starts and before the
/// command that destroys the pane commits.
const PRE_DESTROY_TAIL: &[u8] = b"pre-destroy-tail";
const DRAIN_CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn kill_session_drains_surface_frame_then_removes_destroyed_subscriptions() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "subscription-destroy-kill-session").await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let (subscription_id, _) = subscribe(&handler, &target).await;
    let (surface_subscription_id, surface_key) = subscribe_surface_stream(&handler, &target).await;
    let tail_sequence = publish_pre_destroy_tail(&handler, &target).await;

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");

    assert_surface_stream_drains_then_closes(
        &handler,
        surface_subscription_id,
        tail_sequence,
        "kill-session",
    )
    .await;
    assert_subscription_drains_then_closes(&handler, subscription_id, "kill-session").await;
    assert_drain_source_released(&handler, &surface_key, "kill-session");
}

#[tokio::test]
async fn kill_window_drains_surface_frame_then_removes_destroyed_subscriptions() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "subscription-destroy-kill-window").await;
    create_window(&handler, &session, 1).await;
    let target = PaneTarget::with_window(session.clone(), 1, 0);
    let (subscription_id, _) = subscribe(&handler, &target).await;
    let (surface_subscription_id, surface_key) = subscribe_surface_stream(&handler, &target).await;
    let tail_sequence = publish_pre_destroy_tail(&handler, &target).await;

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(session, 1),
            kill_all_others: false,
        }))
        .await;
    assert!(matches!(response, Response::KillWindow(_)), "{response:?}");

    assert_surface_stream_drains_then_closes(
        &handler,
        surface_subscription_id,
        tail_sequence,
        "kill-window",
    )
    .await;
    assert_subscription_drains_then_closes(&handler, subscription_id, "kill-window").await;
    assert_drain_source_released(&handler, &surface_key, "kill-window");
}

#[tokio::test]
async fn kill_window_drains_buffered_raw_stream_bytes_before_the_typed_end() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "subscription-destroy-kill-window-raw").await;
    create_window(&handler, &session, 1).await;
    let target = PaneTarget::with_window(session.clone(), 1, 0);
    let subscription_id = subscribe_raw_stream(&handler, &target).await;
    let _ = publish_pre_destroy_tail(&handler, &target).await;

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(session, 1),
            kill_all_others: false,
        }))
        .await;
    assert!(matches!(response, Response::KillWindow(_)), "{response:?}");

    let events = raw_stream_events_until_end(&handler, subscription_id).await;
    let tail = events.iter().position(|event| {
        matches!(event, PaneStreamEvent::RawBytes(bytes) if bytes.bytes == PRE_DESTROY_TAIL)
    });
    let end = events
        .iter()
        .position(|event| matches!(event, PaneStreamEvent::End(_)));
    assert!(
        matches!((tail, end), (Some(tail), Some(end)) if tail < end),
        "kill-window must deliver output published before the pane was destroyed \
         before the terminal stream event: {events:?}"
    );
    assert!(
        matches!(
            events.last(),
            Some(PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved))
        ),
        "kill-window must terminate the raw stream with PaneRemoved: {events:?}"
    );
}

#[tokio::test]
async fn link_window_k_drains_surface_frame_then_removes_replaced_subscriptions() {
    let handler = RequestHandler::new();
    let source = create_session(&handler, "subscription-destroy-link-source").await;
    let destination = create_session(&handler, "subscription-destroy-link-destination").await;
    let destination_target = PaneTarget::with_window(destination.clone(), 0, 0);
    let (subscription_id, destination_pane_id) = subscribe(&handler, &destination_target).await;
    let (surface_subscription_id, surface_key) =
        subscribe_surface_stream(&handler, &destination_target).await;
    let tail_sequence = publish_pre_destroy_tail(&handler, &destination_target).await;

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source, 0),
            target: WindowTarget::with_window(destination.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    assert!(
        pane_target_for_id(&handler, &destination, destination_pane_id)
            .await
            .is_none(),
        "link-window -k must remove the replaced stable pane identity"
    );

    assert_surface_stream_drains_then_closes(
        &handler,
        surface_subscription_id,
        tail_sequence,
        "link-window -k",
    )
    .await;
    assert_subscription_drains_then_closes(&handler, subscription_id, "link-window -k").await;
    assert_drain_source_released(&handler, &surface_key, "link-window -k");
}

#[tokio::test]
async fn move_window_k_drains_surface_frame_then_removes_replaced_subscriptions() {
    let handler = RequestHandler::new();
    let source = create_session(&handler, "subscription-destroy-move-source").await;
    let destination = create_session(&handler, "subscription-destroy-move-destination").await;
    let destination_target = PaneTarget::with_window(destination.clone(), 0, 0);
    let (subscription_id, destination_pane_id) = subscribe(&handler, &destination_target).await;
    let (surface_subscription_id, surface_key) =
        subscribe_surface_stream(&handler, &destination_target).await;
    let tail_sequence = publish_pre_destroy_tail(&handler, &destination_target).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(source, 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(destination.clone(), 0)),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");
    assert!(
        pane_target_for_id(&handler, &destination, destination_pane_id)
            .await
            .is_none(),
        "move-window -k must remove the replaced stable pane identity"
    );

    assert_surface_stream_drains_then_closes(
        &handler,
        surface_subscription_id,
        tail_sequence,
        "move-window -k",
    )
    .await;
    assert_subscription_drains_then_closes(&handler, subscription_id, "move-window -k").await;
    assert_drain_source_released(&handler, &surface_key, "move-window -k");
}

#[tokio::test]
async fn unlink_window_k_drains_surface_frame_then_removes_destroyed_subscriptions() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "subscription-destroy-unlink").await;
    create_window(&handler, &session, 1).await;
    let target = PaneTarget::with_window(session.clone(), 1, 0);
    let (subscription_id, pane_id) = subscribe(&handler, &target).await;
    let (surface_subscription_id, surface_key) = subscribe_surface_stream(&handler, &target).await;
    let tail_sequence = publish_pre_destroy_tail(&handler, &target).await;

    let response = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: WindowTarget::with_window(session.clone(), 1),
            kill_if_last: true,
        }))
        .await;
    assert!(
        matches!(response, Response::UnlinkWindow(_)),
        "{response:?}"
    );
    assert!(
        pane_target_for_id(&handler, &session, pane_id)
            .await
            .is_none(),
        "unlink-window -k must remove the unshared stable pane identity"
    );

    assert_surface_stream_drains_then_closes(
        &handler,
        surface_subscription_id,
        tail_sequence,
        "unlink-window -k",
    )
    .await;
    assert_subscription_drains_then_closes(&handler, subscription_id, "unlink-window -k").await;
    assert_drain_source_released(&handler, &surface_key, "unlink-window -k");
}

#[tokio::test]
async fn linked_respawn_window_k_drains_surface_sibling_and_preserves_retained_receiver() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "subscription-respawn-owner").await;
    let alias = create_session(&handler, "subscription-respawn-alias").await;
    split_window(&handler, &owner).await;
    link_window(
        &handler,
        WindowTarget::with_window(owner.clone(), 0),
        WindowTarget::with_window(alias.clone(), 1),
    )
    .await;

    let pane_ids = pane_ids_for_window(&handler, &owner, 0).await;
    assert_eq!(pane_ids.len(), 2, "respawn fixture must have two panes");
    let retained_pane_id = pane_ids[0];
    let removed_pane_id = pane_ids[1];
    let retained_target = pane_target_for_id(&handler, &owner, retained_pane_id)
        .await
        .expect("retained pane target exists before respawn");
    let removed_target = pane_target_for_id(&handler, &owner, removed_pane_id)
        .await
        .expect("sibling pane target exists before respawn");
    let removed_alias_target = pane_target_for_id(&handler, &alias, removed_pane_id)
        .await
        .expect("sibling pane is present in the linked alias before respawn");
    let (retained_subscription, _) = subscribe(&handler, &retained_target).await;
    let (removed_subscription, _) = subscribe(&handler, &removed_target).await;
    let (removed_surface_subscription, removed_surface_key) =
        subscribe_surface_stream(&handler, &removed_target).await;
    let tail_sequence = publish_pre_destroy_tail(&handler, &removed_target).await;
    let option_response = handler
        .handle(Request::PaneOptionSet(PaneOptionSetRequest {
            target: PaneTargetRef::by_id(alias.clone(), removed_pane_id),
            name: "@respawn-linked-sibling".to_owned(),
            value: Some("must-be-pruned".to_owned()),
            mode: SetOptionMode::Replace,
            unset: false,
        }))
        .await;
    assert!(
        matches!(option_response, Response::PaneOptionSet(_)),
        "linked sibling option fixture should succeed: {option_response:?}"
    );

    let response = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: WindowTarget::with_window(owner.clone(), 0),
            kill: true,
            environment: None,
            command: None,
            start_directory: None,
        })))
        .await;
    assert!(
        matches!(response, Response::RespawnWindow(_)),
        "{response:?}"
    );

    assert_eq!(
        pane_ids_for_window(&handler, &owner, 0).await,
        vec![retained_pane_id],
        "respawned owner window keeps only the stable first pane"
    );
    assert_eq!(
        pane_ids_for_window(&handler, &alias, 1).await,
        vec![retained_pane_id],
        "linked alias must receive the respawned one-pane model"
    );
    assert!(
        pane_target_for_id(&handler, &owner, removed_pane_id)
            .await
            .is_none(),
        "destroyed sibling must not remain reachable through the owner"
    );
    assert!(
        pane_target_for_id(&handler, &alias, removed_pane_id)
            .await
            .is_none(),
        "destroyed sibling must not remain reachable through the linked alias"
    );
    assert_surface_stream_drains_then_closes(
        &handler,
        removed_surface_subscription,
        tail_sequence,
        "respawn-window -k sibling",
    )
    .await;
    assert_subscription_drains_then_closes(
        &handler,
        removed_subscription,
        "respawn-window sibling",
    )
    .await;
    assert_drain_source_released(&handler, &removed_surface_key, "respawn-window -k sibling");
    let stale_alias_options = {
        let state = handler.state.lock().await;
        state
            .options
            .explicit_entries_for_scope(&OptionScopeSelector::Pane(removed_alias_target))
    };
    assert!(
        stale_alias_options
            .iter()
            .all(|(name, _)| name != "@respawn-linked-sibling"),
        "respawn must prune destroyed sibling options from every linked alias: {stale_alias_options:?}"
    );

    let retained_target = pane_target_for_id(&handler, &owner, retained_pane_id)
        .await
        .expect("retained pane remains reachable after respawn");
    let expected = b"respawned-output-channel-remains-live".to_vec();
    handler
        .send_pane_output_for_test(&retained_target, expected.clone())
        .await;
    let cursor = handler
        .handle_pane_output_cursor(
            CONNECTION_ID,
            PaneOutputCursorRequest {
                subscription_id: retained_subscription,
                max_events: Some(16),
            },
        )
        .await;
    let cursor = match cursor {
        Response::PaneOutputCursor(cursor) => cursor,
        Response::PaneOutputLag(lag) => {
            assert_eq!(
                lag.subscription_id, retained_subscription,
                "respawn lag must belong to the retained subscription"
            );
            assert!(
                lag.lag.missed_events > 0 && lag.lag.resume_sequence > lag.lag.expected_sequence,
                "respawn lag must describe a real cleared lifetime-boundary gap: {lag:?}"
            );
            assert_eq!(
                lag.lag.missed_events,
                lag.lag.resume_sequence - lag.lag.expected_sequence,
                "respawn lag count must cover the complete cleared sequence range"
            );
            assert_eq!(
                lag.cursor.next_sequence, lag.lag.resume_sequence,
                "respawn lag cursor must advance to the advertised resume sequence"
            );
            assert_eq!(
                lag.cursor.missed_events, lag.lag.missed_events,
                "respawn lag cursor must account for every missed event"
            );
            assert!(
                lag.lag
                    .recent
                    .bytes
                    .windows(expected.len())
                    .any(|bytes| bytes == expected.as_slice()),
                "respawn lag recovery must retain the new runtime output: {lag:?}"
            );

            let resumed = handler
                .handle_pane_output_cursor(
                    CONNECTION_ID,
                    PaneOutputCursorRequest {
                        subscription_id: retained_subscription,
                        max_events: Some(16),
                    },
                )
                .await;
            let Response::PaneOutputCursor(cursor) = resumed else {
                panic!("retained respawn subscription must resume after one lag: {resumed:?}");
            };
            cursor
        }
        response => {
            panic!("retained respawn subscription should remain readable: {response:?}")
        }
    };
    assert!(
        cursor.events.iter().any(|event| event.bytes == expected),
        "the pre-respawn receiver must observe output from the new runtime"
    );
}

async fn create_session(handler: &RequestHandler, name: &str) -> SessionName {
    let session_name = SessionName::new(name).expect("valid session name");
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&response);
    handler.wait_for_initial_panes_for_test().await;
    session_name
}

async fn create_window(handler: &RequestHandler, session: &SessionName, window_index: u32) {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: Some(window_index),
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    handler.wait_for_initial_panes_for_test().await;
}

async fn split_window(handler: &RequestHandler, session: &SessionName) {
    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::SplitWindow(_)), "{response:?}");
}

async fn link_window(handler: &RequestHandler, source: WindowTarget, target: WindowTarget) {
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source,
            target,
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
}

async fn subscribe(
    handler: &RequestHandler,
    target: &PaneTarget,
) -> (PaneOutputSubscriptionId, rmux_proto::PaneId) {
    let pane_id = pane_id_for_target(handler, target).await;
    let response = handler
        .handle_subscribe_pane_output_ref(
            CONNECTION_ID,
            SubscribePaneOutputRefRequest {
                target: PaneTargetRef::by_id(target.session_name().clone(), pane_id),
                start: PaneOutputSubscriptionStart::Now,
            },
        )
        .await;
    let Response::SubscribePaneOutput(response) = response else {
        panic!("pane-output subscription should succeed: {response:?}");
    };
    (response.subscription_id, pane_id)
}

async fn subscribe_raw_stream(
    handler: &RequestHandler,
    target: &PaneTarget,
) -> PaneOutputSubscriptionId {
    let response = handler
        .handle_subscribe_pane_stream(
            CONNECTION_ID,
            SubscribePaneStreamRequest {
                target: PaneTargetRef::slot(target.clone()),
                mode: PaneStreamMode::Raw,
                include_snapshot: false,
            },
        )
        .await;
    let Response::SubscribePaneStream(response) = response else {
        panic!("raw pane stream subscription should succeed: {response:?}");
    };
    response.subscription_id
}

async fn subscribe_surface_stream(
    handler: &RequestHandler,
    target: &PaneTarget,
) -> (PaneOutputSubscriptionId, PaneOutputSubscriptionKey) {
    let response = handler
        .handle_subscribe_pane_stream(
            CONNECTION_ID,
            SubscribePaneStreamRequest {
                target: PaneTargetRef::slot(target.clone()),
                mode: PaneStreamMode::Surface,
                include_snapshot: false,
            },
        )
        .await;
    let Response::SubscribePaneStream(response) = response else {
        panic!("surface pane stream subscription should succeed: {response:?}");
    };
    assert!(
        matches!(response.event, PaneStreamEvent::SurfaceReset(_)),
        "surface pane stream must begin with a reset: {response:?}"
    );
    let key = handler
        .pane_output_subscription_key_for_test(response.subscription_id)
        .expect("surface pane stream has a registry key");
    (response.subscription_id, key)
}

async fn publish_pre_destroy_tail(handler: &RequestHandler, target: &PaneTarget) -> u64 {
    let (output, transcript) = {
        let state = handler.state.lock().await;
        let output = state
            .pane_output_for_target(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            )
            .expect("test pane has an output channel");
        let transcript = state
            .transcript_handle(target)
            .expect("test pane has a transcript");
        (output, transcript)
    };
    transcript.lock().append_bytes(PRE_DESTROY_TAIL);
    output.send(PRE_DESTROY_TAIL.to_vec())
}

async fn raw_stream_events_until_end(
    handler: &RequestHandler,
    subscription_id: PaneOutputSubscriptionId,
) -> Vec<PaneStreamEvent> {
    let mut events = Vec::new();
    for _ in 0..64 {
        let response = handler
            .handle_pane_stream_cursor(
                CONNECTION_ID,
                PaneStreamCursorRequest {
                    subscription_id,
                    max_events: Some(32),
                },
            )
            .await;
        let Response::PaneStreamCursor(response) = response else {
            panic!("raw stream cursor should stay readable while draining: {response:?}");
        };
        let ended = response
            .events
            .iter()
            .any(|event| matches!(event, PaneStreamEvent::End(_)));
        events.extend(response.events);
        if ended {
            return events;
        }
        if !response.limited {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    panic!("raw stream must reach a terminal event while draining: {events:?}");
}

async fn assert_surface_stream_drains_then_closes(
    handler: &RequestHandler,
    subscription_id: PaneOutputSubscriptionId,
    tail_sequence: u64,
    label: &str,
) {
    let mut events = Vec::new();
    for _ in 0..64 {
        let response = handler
            .handle_pane_stream_cursor(
                CONNECTION_ID,
                PaneStreamCursorRequest {
                    subscription_id,
                    max_events: Some(1),
                },
            )
            .await;
        let Response::PaneStreamCursor(response) = response else {
            panic!("{label} surface stream must stay readable while draining: {response:?}");
        };
        let ended = response
            .events
            .iter()
            .any(|event| matches!(event, PaneStreamEvent::End(_)));
        events.extend(response.events);
        if ended {
            break;
        }
        if !response.limited {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    assert!(
        matches!(
            events.last(),
            Some(PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved))
        ),
        "{label} must terminate with PaneRemoved: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, PaneStreamEvent::End(_)))
            .count(),
        1,
        "{label} must publish exactly one terminal event: {events:?}"
    );
    assert!(
        events[..events.len() - 1].iter().all(|event| matches!(
            event,
            PaneStreamEvent::SurfacePatch(_)
                | PaneStreamEvent::SurfaceReset(_)
                | PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
        )),
        "{label} emitted an unexpected pre-terminal event: {events:?}"
    );
    assert!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
            ))
            .count()
            <= 1,
        "{label} may publish at most one lifecycle event for the removed process: {events:?}"
    );
    let surface_frames = events
        .iter()
        .filter_map(|event| match event {
            PaneStreamEvent::SurfacePatch(frame) | PaneStreamEvent::SurfaceReset(frame) => {
                Some(frame.as_ref())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let final_surface_frame = surface_frames
        .last()
        .expect("an ending Surface stream must publish its final authoritative frame");
    // Surface is an authoritative state projection, not a lossless transcript.
    // Platform teardown may scroll or erase the visible tail; the output
    // boundary proves that the final projection was captured after it.
    assert!(
        final_surface_frame.next_output_sequence > tail_sequence,
        "{label} final Surface frame must represent the pre-destroy output boundary before End: \
         tail sequence {tail_sequence}, events {events:?}"
    );

    let response = handler
        .handle_pane_stream_cursor(
            CONNECTION_ID,
            PaneStreamCursorRequest {
                subscription_id,
                max_events: Some(1),
            },
        )
        .await;
    assert!(
        matches!(
            response,
            Response::Error(ref error) if error.error.to_string().contains("subscription not found")
        ),
        "{label} must not retain or redeliver an ended Surface stream: {response:?}"
    );
}

/// A destroyed pane's already-published output must still reach its
/// subscriber, exactly as it does for `kill-pane` and natural pane exit, and
/// the subscription must be retired once the drain goes idle.
async fn assert_subscription_drains_then_closes(
    handler: &RequestHandler,
    subscription_id: PaneOutputSubscriptionId,
    label: &str,
) {
    let cursor = handler
        .handle_pane_output_cursor(
            CONNECTION_ID,
            PaneOutputCursorRequest {
                subscription_id,
                max_events: Some(64),
            },
        )
        .await;
    let Response::PaneOutputCursor(cursor) = cursor else {
        panic!("{label} must keep the destroyed pane's subscription readable: {cursor:?}");
    };
    assert!(
        cursor
            .events
            .iter()
            .any(|event| event.bytes == PRE_DESTROY_TAIL),
        "{label} must drain output published before the pane was destroyed: {cursor:?}"
    );

    tokio::time::timeout(DRAIN_CLOSE_TIMEOUT, async {
        while handler
            .pane_output_subscription_key_for_test(subscription_id)
            .is_some()
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label} must retire the dead pane's registry record once idle"));

    let cursor = handler
        .handle_pane_output_cursor(
            CONNECTION_ID,
            PaneOutputCursorRequest {
                subscription_id,
                max_events: Some(1),
            },
        )
        .await;
    assert!(
        matches!(
            cursor,
            Response::Error(ref error) if error.error.to_string().contains("subscription not found")
        ),
        "{label} must make the drained subscription unreadable: {cursor:?}"
    );
}

fn assert_drain_source_released(
    handler: &RequestHandler,
    key: &PaneOutputSubscriptionKey,
    label: &str,
) {
    assert!(
        handler
            .subscriptions
            .lock()
            .expect("subscription registry mutex")
            .draining_stream_source(key)
            .is_none(),
        "{label} must release the staged pane stream source after the final subscription"
    );
}

async fn pane_id_for_target(handler: &RequestHandler, target: &PaneTarget) -> rmux_proto::PaneId {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(rmux_core::Pane::id)
        .expect("pane target exists")
}

async fn pane_ids_for_window(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
) -> Vec<rmux_proto::PaneId> {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(session_name)
        .and_then(|session| session.window_at(window_index))
        .map(|window| window.panes().iter().map(rmux_core::Pane::id).collect())
        .expect("window exists")
}

async fn pane_target_for_id(
    handler: &RequestHandler,
    session_name: &SessionName,
    pane_id: rmux_proto::PaneId,
) -> Option<PaneTarget> {
    let state = handler.state.lock().await;
    let session = state.sessions.session(session_name)?;
    let result = session.windows().find_map(|(window_index, window)| {
        window
            .panes()
            .iter()
            .find(|pane| pane.id() == pane_id)
            .map(|pane| PaneTarget::with_window(session_name.clone(), window_index, pane.index()))
    });
    result
}
