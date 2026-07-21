use std::sync::Arc;
use std::time::{Duration, Instant};

use rmux_core::events::{
    OutputCursor, OutputCursorItem, OutputRing, PaneOutputSubscriptionKey, SubscriptionLimits,
};
use rmux_proto::{
    NewSessionRequest, PaneId, PaneOutputCursor, PaneOutputCursorRequest, PaneOutputKeyframe,
    PaneOutputRecoveryResponse, PaneOutputSubscriptionStart, PaneSnapshotCursor,
    PaneSnapshotResponse, PaneTarget, PaneTargetRef, RenameSessionRequest, Request, Response,
    RmuxError, SessionId, SessionName, SubscribePaneOutputRefRequest, SubscribePaneOutputRequest,
    DEFAULT_MAX_DETACHED_FRAME_LENGTH,
};

use crate::daemon::ShutdownHandle;
use crate::handler::exited_output_support::RetainedExitedPaneIdentities;
use crate::pane_io::pane_output_channel_with_limits;

use super::{
    lag_dto, validate_recovery_response, OutputSubscriptionState, RequestHandler,
    MAX_LAG_RECENT_BYTES,
};
use crate::handler::PendingShutdownReason;

#[path = "handler_subscriptions_tests/destructive_mutations.rs"]
mod destructive_mutations;
#[path = "handler_subscriptions_tests/transfers.rs"]
mod transfers;

#[derive(Debug, Default)]
pub(super) struct LiveSubscriptionCommitPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

static LIVE_SUBSCRIPTION_COMMIT_PAUSE: std::sync::Mutex<
    Option<(PaneOutputSubscriptionKey, Arc<LiveSubscriptionCommitPause>)>,
> = std::sync::Mutex::new(None);

fn retained_identities() -> RetainedExitedPaneIdentities {
    RetainedExitedPaneIdentities::new(SessionId::new(1), SessionId::new(1))
}

fn install_live_subscription_commit_pause(
    pane: PaneOutputSubscriptionKey,
) -> Arc<LiveSubscriptionCommitPause> {
    let pause = Arc::new(LiveSubscriptionCommitPause::default());
    *LIVE_SUBSCRIPTION_COMMIT_PAUSE
        .lock()
        .expect("live subscription commit pause lock") = Some((pane, pause.clone()));
    pause
}

pub(super) async fn pause_before_live_subscription_commit(pane: &PaneOutputSubscriptionKey) {
    let pause = LIVE_SUBSCRIPTION_COMMIT_PAUSE
        .lock()
        .expect("live subscription commit pause lock")
        .as_ref()
        .filter(|(paused_pane, _)| paused_pane == pane)
        .map(|(_, pause)| pause.clone());
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
    let mut installed = LIVE_SUBSCRIPTION_COMMIT_PAUSE
        .lock()
        .expect("live subscription commit pause lock");
    if installed
        .as_ref()
        .is_some_and(|(_, current)| Arc::ptr_eq(current, &pause))
    {
        installed.take();
    }
}

#[test]
fn lag_dto_carries_recent_output_without_replaying_missed_bytes() {
    let mut ring = OutputRing::new(1, 8);
    let mut cursor = OutputCursor::new(0);
    ring.push(b"abcdef".to_vec());
    ring.push(b"ghijk".to_vec());

    let Some(OutputCursorItem::Gap(gap)) = ring.poll_cursor(&mut cursor) else {
        panic!("cursor should lag after output ring rotation");
    };
    let notice = lag_dto(&gap);

    assert_eq!(notice.expected_sequence, 0);
    assert_eq!(notice.resume_sequence, 1);
    assert_eq!(notice.missed_events, 1);
    assert_eq!(notice.newest_sequence, 1);
    assert_eq!(notice.recent.bytes, b"defghijk");
    assert_eq!(notice.recent.oldest_sequence, Some(0));
    assert_eq!(notice.recent.newest_sequence, Some(1));

    let Some(OutputCursorItem::Event(event)) = ring.poll_cursor(&mut cursor) else {
        panic!("cursor should resume at the oldest retained output event");
    };
    assert_eq!(event.sequence(), notice.resume_sequence);
    assert_eq!(event.bytes(), b"ghijk");
    assert_ne!(event.bytes(), notice.recent.bytes.as_slice());
}

#[test]
fn lag_dto_trims_recent_hint_under_detached_frame_limit() {
    let mut ring = OutputRing::new(1, MAX_LAG_RECENT_BYTES + 16);
    let mut cursor = OutputCursor::new(0);
    ring.push(vec![b'a'; 16]);
    ring.push(vec![b'b'; MAX_LAG_RECENT_BYTES + 16]);

    let Some(OutputCursorItem::Gap(gap)) = ring.poll_cursor(&mut cursor) else {
        panic!("cursor should lag after output ring rotation");
    };
    let notice = lag_dto(&gap);

    assert_eq!(notice.recent.bytes.len(), MAX_LAG_RECENT_BYTES);
    assert!(notice.recent.bytes.iter().all(|byte| *byte == b'b'));
    assert_eq!(notice.recent.oldest_sequence, None);
    assert_eq!(notice.recent.newest_sequence, Some(1));
    assert_eq!(notice.resume_sequence, 1);
}

#[test]
fn oversized_recovery_response_rolls_back_opaque_subscription() {
    let handler = RequestHandler::new();
    let connection_id = 76;
    let session = SessionName::new("oversized-recovery").expect("valid session");
    let pane_id = PaneId::new(7);
    let sender = pane_output_channel_with_limits(1, 8);
    let receiver = sender.subscribe();
    let subscription_id = {
        let mut subscriptions = handler
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        let record = subscriptions
            .registry
            .subscribe(
                connection_id,
                PaneOutputSubscriptionKey::new(session.clone(), pane_id),
                Instant::now(),
            )
            .expect("subscription is within limits");
        let subscription_id = record.id();
        subscriptions.receivers.insert(subscription_id, receiver);
        subscription_id
    };
    let response = Response::PaneOutputRecovery(Box::new(PaneOutputRecoveryResponse {
        subscription_id,
        target: PaneTarget::new(session, 0),
        pane_id,
        cursor: PaneOutputCursor {
            next_sequence: 0,
            missed_events: 0,
        },
        snapshot: PaneSnapshotResponse {
            cols: 0,
            rows: 0,
            cells: Vec::new(),
            cursor: PaneSnapshotCursor {
                row: 0,
                col: 0,
                visible: true,
                style: 0,
            },
            revision: 1,
        },
        keyframe: PaneOutputKeyframe {
            cols: 0,
            rows: 0,
            bytes: vec![0; DEFAULT_MAX_DETACHED_FRAME_LENGTH],
            alternate: false,
            next_sequence: 0,
        },
    }));

    assert!(matches!(
        validate_recovery_response(&handler.subscriptions, subscription_id, response),
        Response::Error(rmux_proto::ErrorResponse {
            error: RmuxError::FrameTooLarge { .. },
        })
    ));
    let subscriptions = handler
        .subscriptions
        .lock()
        .expect("subscription registry mutex must not be poisoned");
    assert!(subscriptions.registry.get(subscription_id).is_none());
    assert!(!subscriptions.receivers.contains_key(&subscription_id));
}

#[tokio::test]
async fn cursor_handler_trims_lag_recent_hint_for_subscription_response() {
    let handler = RequestHandler::new();
    let connection_id = 77;
    let sender = pane_output_channel_with_limits(1, MAX_LAG_RECENT_BYTES + 32);
    let receiver = sender.subscribe();
    sender.send(vec![b'a'; 32]);
    sender.send(vec![b'b'; MAX_LAG_RECENT_BYTES + 32]);

    let subscription_id = {
        let mut subscriptions = handler
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        let record = subscriptions
            .registry
            .subscribe(
                connection_id,
                PaneOutputSubscriptionKey::new(
                    SessionName::new("runtime").expect("valid session name"),
                    PaneId::new(1),
                ),
                Instant::now(),
            )
            .expect("subscription is within limits");
        let subscription_id = record.id();
        subscriptions.receivers.insert(subscription_id, receiver);
        subscription_id
    };

    let response = handler
        .handle_pane_output_cursor(
            connection_id,
            PaneOutputCursorRequest {
                subscription_id,
                max_events: Some(8),
            },
        )
        .await;
    let Response::PaneOutputLag(lag) = response else {
        panic!("lagged subscription should produce a lag response");
    };

    assert_eq!(lag.subscription_id, subscription_id);
    assert_eq!(lag.cursor.next_sequence, 1);
    assert_eq!(lag.cursor.missed_events, 1);
    assert_eq!(lag.lag.expected_sequence, 0);
    assert_eq!(lag.lag.resume_sequence, 1);
    assert_eq!(lag.lag.missed_events, 1);
    assert_eq!(lag.lag.recent.bytes.len(), MAX_LAG_RECENT_BYTES);
    assert!(lag.lag.recent.bytes.iter().all(|byte| *byte == b'b'));
    assert_eq!(lag.lag.recent.oldest_sequence, None);
    assert_eq!(lag.lag.recent.newest_sequence, Some(1));
}

#[tokio::test]
async fn live_subscription_commit_serializes_with_session_rename_rekey() {
    let handler = RequestHandler::new();
    let alpha = SessionName::new("subscription-rename-alpha").expect("valid session name");
    let beta = SessionName::new("subscription-rename-beta").expect("valid session name");
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    handler.wait_for_initial_panes_for_test().await;

    let previous_key = {
        let state = handler.state.lock().await;
        state
            .pane_output_subscription_key_for_target(&target)
            .expect("live pane has a subscription key")
    };
    let pause = install_live_subscription_commit_pause(previous_key.clone());
    let subscribe_handler = handler.clone();
    let subscribe_target = target.clone();
    let subscribe = tokio::spawn(async move {
        subscribe_handler
            .handle_subscribe_pane_output(
                71,
                SubscribePaneOutputRequest {
                    target: subscribe_target,
                    start: PaneOutputSubscriptionStart::Now,
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), pause.reached.notified())
        .await
        .expect("subscription reaches its state-locked commit point");

    let rename_handler = handler.clone();
    let rename_alpha = alpha.clone();
    let rename_beta = beta.clone();
    let rename = tokio::spawn(async move {
        rename_handler
            .handle(Request::RenameSession(RenameSessionRequest {
                target: rename_alpha,
                new_name: rename_beta,
            }))
            .await
    });
    tokio::task::yield_now().await;
    assert!(
        !rename.is_finished(),
        "rename must wait while a live subscription holds the state-to-registry transaction"
    );

    pause.release.notify_one();
    let subscribed = tokio::time::timeout(Duration::from_secs(2), subscribe)
        .await
        .expect("subscription commit must not deadlock")
        .expect("subscription task joins");
    let Response::SubscribePaneOutput(subscribed) = subscribed else {
        panic!("live subscription should succeed: {subscribed:?}");
    };
    let renamed = tokio::time::timeout(Duration::from_secs(2), rename)
        .await
        .expect("rename rekey must not deadlock")
        .expect("rename task joins");
    assert!(matches!(renamed, Response::RenameSession(_)), "{renamed:?}");

    let current_key = handler
        .pane_output_subscription_key_for_test(subscribed.subscription_id)
        .expect("subscription survives rename");
    assert_eq!(
        current_key,
        PaneOutputSubscriptionKey::new(beta, previous_key.pane_id()),
        "rename must rekey the subscription committed immediately before it"
    );
}

#[tokio::test]
async fn exited_pane_subscription_stays_alive_after_eof_until_cleanup() {
    let handler = RequestHandler::new();
    let connection_id = 88;
    let pane = PaneOutputSubscriptionKey::new(
        SessionName::new("runtime").expect("valid session name"),
        PaneId::new(7),
    );
    let sender = pane_output_channel_with_limits(8, 1024);
    let receiver = sender.subscribe_from_oldest();

    let subscription_id = {
        let mut subscriptions = handler
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        let record = subscriptions
            .registry
            .subscribe(connection_id, pane.clone(), Instant::now())
            .expect("subscription is within limits");
        let subscription_id = record.id();
        subscriptions.receivers.insert(subscription_id, receiver);
        subscription_id
    };

    handler
        .drain_exited_pane_output_subscriptions(pane.clone())
        .await;

    let empty_before_eof = handler
        .handle_pane_output_cursor(
            connection_id,
            PaneOutputCursorRequest {
                subscription_id,
                max_events: Some(8),
            },
        )
        .await;
    let Response::PaneOutputCursor(empty_before_eof) = empty_before_eof else {
        panic!("draining subscription must stay alive before EOF");
    };
    assert!(empty_before_eof.events.is_empty());

    sender.send(b"final burst".to_vec());
    sender.send(Vec::new());

    let drained = handler
        .handle_pane_output_cursor(
            connection_id,
            PaneOutputCursorRequest {
                subscription_id,
                max_events: Some(8),
            },
        )
        .await;
    let Response::PaneOutputCursor(drained) = drained else {
        panic!("draining subscription should deliver retained bytes and EOF");
    };
    assert_eq!(drained.events.len(), 2);
    assert_eq!(drained.events[0].bytes, b"final burst");
    assert!(drained.events[1].bytes.is_empty());

    let idle_after_eof = handler
        .handle_pane_output_cursor(
            connection_id,
            PaneOutputCursorRequest {
                subscription_id,
                max_events: Some(8),
            },
        )
        .await;
    let Response::PaneOutputCursor(idle_after_eof) = idle_after_eof else {
        panic!("subscription should stay alive after EOF until explicit cleanup");
    };
    assert!(idle_after_eof.events.is_empty());

    handler.cleanup_connection_subscriptions_sync(connection_id);
    let closed = handler
        .handle_pane_output_cursor(
            connection_id,
            PaneOutputCursorRequest {
                subscription_id,
                max_events: Some(8),
            },
        )
        .await;
    let Response::Error(error) = closed else {
        panic!("subscription should close after connection cleanup");
    };
    assert_eq!(
        error.error,
        RmuxError::Server("subscription not found".to_owned())
    );
}

#[tokio::test]
async fn empty_server_shutdown_waits_for_exited_pane_subscription_drain() {
    let handler = RequestHandler::new();
    let (shutdown_handle, mut shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);
    let connection_id = 188;
    let pane = PaneOutputSubscriptionKey::new(
        SessionName::new("runtime").expect("valid session name"),
        PaneId::new(17),
    );
    let sender = pane_output_channel_with_limits(8, 1024);
    let receiver = sender.subscribe_from_oldest();

    let subscription_id = {
        let mut subscriptions = handler
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        let record = subscriptions
            .registry
            .subscribe(connection_id, pane.clone(), Instant::now())
            .expect("subscription is within limits");
        let subscription_id = record.id();
        subscriptions.receivers.insert(subscription_id, receiver);
        subscription_id
    };

    handler
        .drain_exited_pane_output_subscriptions(pane.clone())
        .await;
    assert!(
        !handler.request_shutdown_if_server_empty().await,
        "active output drains must keep an otherwise empty server alive"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut shutdown_rx)
            .await
            .is_err(),
        "shutdown must not fire before the SDK can drain retained output"
    );

    sender.send(b"final burst".to_vec());
    sender.send(Vec::new());
    let drained = handler
        .handle_pane_output_cursor(
            connection_id,
            PaneOutputCursorRequest {
                subscription_id,
                max_events: Some(8),
            },
        )
        .await;
    let Response::PaneOutputCursor(drained) = drained else {
        panic!("draining subscription should deliver retained bytes and EOF");
    };
    assert_eq!(drained.events.len(), 2);
    assert_eq!(drained.events[0].bytes, b"final burst");
    assert!(drained.events[1].bytes.is_empty());

    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut shutdown_rx)
            .await
            .is_err(),
        "the SDK connection must remain alive after EOF so callers can reconcile exit state"
    );

    handler.cleanup_connection_subscriptions_sync(connection_id);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), shutdown_rx)
            .await
            .expect("shutdown should fire after the SDK connection closes")
            .is_ok(),
        "shutdown receiver should complete cleanly"
    );
}

#[test]
fn exited_pane_drain_idle_tracks_subscription_touch() {
    let mut subscriptions = OutputSubscriptionState::new(SubscriptionLimits::default());
    let pane = PaneOutputSubscriptionKey::new(
        SessionName::new("runtime").expect("valid session name"),
        PaneId::new(42),
    );
    let created = Instant::now();
    let record = subscriptions
        .registry
        .subscribe(5, pane.clone(), created)
        .expect("subscription is within limits");

    assert!(subscriptions.begin_pane_drain(pane.clone()));
    assert_eq!(
        subscriptions.pane_drain_idle_for(&pane, created),
        Some(Duration::ZERO)
    );

    let touched = created + Duration::from_secs(5);
    subscriptions
        .registry
        .touch(record.id(), touched)
        .expect("subscription should still be live");
    assert_eq!(
        subscriptions.pane_drain_idle_for(&pane, touched + Duration::from_millis(25)),
        Some(Duration::from_millis(25))
    );
}

#[tokio::test]
async fn exited_pane_subscription_auto_cleans_after_drain_timeout() {
    let handler = RequestHandler::new();
    let connection_id = 99;
    let pane = PaneOutputSubscriptionKey::new(
        SessionName::new("runtime").expect("valid session name"),
        PaneId::new(9),
    );
    let sender = pane_output_channel_with_limits(8, 1024);
    let receiver = sender.subscribe_from_oldest();
    let subscription_id = {
        let mut subscriptions = handler
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        let record = subscriptions
            .registry
            .subscribe(connection_id, pane.clone(), Instant::now())
            .expect("subscription is within limits");
        let subscription_id = record.id();
        subscriptions.receivers.insert(subscription_id, receiver);
        subscription_id
    };

    handler
        .drain_exited_pane_output_subscriptions(pane.clone())
        .await;
    sender.send(b"tail".to_vec());
    sender.send(Vec::new());

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let removed = handler
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned")
                .registry
                .get(subscription_id)
                .is_none();
            if removed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("undrained subscription should auto-clean after the drain timeout");
}

#[tokio::test]
async fn exit_empty_shutdown_does_not_wait_for_unsubscribed_retained_output() {
    let handler = RequestHandler::new();
    let (shutdown_handle, mut shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);
    let session_name = SessionName::new("gone").expect("valid session name");
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    let pane = PaneOutputSubscriptionKey::new(session_name, PaneId::new(44));
    let sender = pane_output_channel_with_limits(8, 1024);
    sender.send(b"retained".to_vec());
    sender.send(Vec::new());
    handler
        .retain_exited_pane_output(target, pane.clone(), retained_identities(), sender)
        .await;

    handler.queue_shutdown_request(PendingShutdownReason::ExitEmpty);
    assert!(
        handler.request_shutdown_if_pending(),
        "exit-empty shutdown should not wait for retained output with no subscribers"
    );
    tokio::time::timeout(Duration::from_millis(50), &mut shutdown_rx)
        .await
        .expect("shutdown should be requested immediately")
        .expect("shutdown receiver should complete cleanly");
    assert!(
        handler
            .retained_exited_pane_output_by_pane(&pane, Instant::now())
            .is_none(),
        "retained output should be discarded once exit-empty is committed"
    );
}

#[tokio::test]
async fn oldest_subscription_can_attach_to_retained_exited_pane_output() {
    let handler = RequestHandler::new();
    let connection_id = 55;
    let session_name = SessionName::new("gone").expect("valid session name");
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    let pane = PaneOutputSubscriptionKey::new(session_name, PaneId::new(33));
    let sender = pane_output_channel_with_limits(8, 1024);
    sender.send(b"retained start".to_vec());
    sender.send(b"retained tail".to_vec());
    sender.send(Vec::new());

    handler
        .retain_exited_pane_output(target.clone(), pane, retained_identities(), sender)
        .await;

    let response = handler
        .handle_subscribe_pane_output(
            connection_id,
            SubscribePaneOutputRequest {
                target,
                start: PaneOutputSubscriptionStart::Oldest,
            },
        )
        .await;
    let Response::SubscribePaneOutput(subscribe) = response else {
        panic!("retained exited output should accept an Oldest subscription");
    };

    let response = handler
        .handle_pane_output_cursor(
            connection_id,
            PaneOutputCursorRequest {
                subscription_id: subscribe.subscription_id,
                max_events: Some(8),
            },
        )
        .await;
    let Response::PaneOutputCursor(cursor) = response else {
        panic!("retained subscription should return a cursor response");
    };
    assert_eq!(cursor.events.len(), 3);
    assert_eq!(cursor.events[0].bytes, b"retained start");
    assert_eq!(cursor.events[1].bytes, b"retained tail");
    assert!(cursor.events[2].bytes.is_empty());
}

#[tokio::test]
async fn oldest_subscription_by_id_can_attach_to_retained_exited_pane_output() {
    let handler = RequestHandler::new();
    let connection_id = 56;
    let session_name = SessionName::new("gone-by-id").expect("valid session name");
    let pane_id = PaneId::new(34);
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    let pane = PaneOutputSubscriptionKey::new(session_name.clone(), pane_id);
    let sender = pane_output_channel_with_limits(8, 1024);
    sender.send(b"retained id start".to_vec());
    sender.send(b"retained id tail".to_vec());
    sender.send(Vec::new());

    handler
        .retain_exited_pane_output(target.clone(), pane, retained_identities(), sender)
        .await;

    let response = handler
        .handle_subscribe_pane_output_ref(
            connection_id,
            SubscribePaneOutputRefRequest {
                target: PaneTargetRef::by_id(session_name, pane_id),
                start: PaneOutputSubscriptionStart::Oldest,
            },
        )
        .await;
    let Response::SubscribePaneOutput(subscribe) = response else {
        panic!("retained exited output should accept an Oldest by-id subscription");
    };
    assert_eq!(subscribe.target, target);
    assert_eq!(subscribe.pane_id, pane_id);

    let response = handler
        .handle_pane_output_cursor(
            connection_id,
            PaneOutputCursorRequest {
                subscription_id: subscribe.subscription_id,
                max_events: Some(8),
            },
        )
        .await;
    let Response::PaneOutputCursor(cursor) = response else {
        panic!("retained by-id subscription should return a cursor response");
    };
    assert_eq!(cursor.events.len(), 3);
    assert_eq!(cursor.events[0].bytes, b"retained id start");
    assert_eq!(cursor.events[1].bytes, b"retained id tail");
    assert!(cursor.events[2].bytes.is_empty());
}

#[tokio::test]
async fn retained_exited_output_by_id_does_not_follow_reused_slot() {
    let handler = RequestHandler::new();
    let session_name = SessionName::new("reused-slot").expect("valid session name");
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    let old_pane_id = PaneId::new(34);
    let new_pane_id = PaneId::new(35);
    let old_pane = PaneOutputSubscriptionKey::new(session_name.clone(), old_pane_id);
    let new_pane = PaneOutputSubscriptionKey::new(session_name.clone(), new_pane_id);
    let old_sender = pane_output_channel_with_limits(8, 1024);
    let new_sender = pane_output_channel_with_limits(8, 1024);
    old_sender.send(b"old retained output".to_vec());
    old_sender.send(Vec::new());
    new_sender.send(b"new retained output".to_vec());
    new_sender.send(Vec::new());

    handler
        .retain_exited_pane_output(target.clone(), old_pane, retained_identities(), old_sender)
        .await;
    handler
        .retain_exited_pane_output(target.clone(), new_pane, retained_identities(), new_sender)
        .await;

    assert_retained_output_by_id(
        &handler,
        57,
        session_name.clone(),
        old_pane_id,
        b"old retained output",
    )
    .await;
    assert_retained_output_by_id(
        &handler,
        58,
        session_name.clone(),
        new_pane_id,
        b"new retained output",
    )
    .await;

    let response = handler
        .handle_subscribe_pane_output(
            59,
            SubscribePaneOutputRequest {
                target,
                start: PaneOutputSubscriptionStart::Oldest,
            },
        )
        .await;
    let Response::SubscribePaneOutput(subscribe) = response else {
        panic!("retained slot subscription should resolve to newest slot occupant");
    };
    assert_eq!(subscribe.pane_id, new_pane_id);
}

#[tokio::test]
async fn kill_server_does_not_wait_for_retained_exited_pane_output() {
    let handler = RequestHandler::new();
    let (shutdown_handle, mut shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);
    let session_name = SessionName::new("gone").expect("valid session name");
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    let pane = PaneOutputSubscriptionKey::new(session_name, PaneId::new(44));
    let sender = pane_output_channel_with_limits(8, 1024);
    sender.send(b"retained".to_vec());
    sender.send(Vec::new());
    handler
        .retain_exited_pane_output(target, pane, retained_identities(), sender)
        .await;

    let Response::KillServer(_) = handler.handle_kill_server().await else {
        panic!("kill-server should acknowledge shutdown");
    };
    assert!(
        handler.request_shutdown_if_pending(),
        "explicit kill-server must not wait for retained SDK output"
    );
    tokio::time::timeout(Duration::from_millis(50), &mut shutdown_rx)
        .await
        .expect("shutdown should be requested immediately")
        .expect("shutdown receiver should complete cleanly");
}

#[tokio::test]
async fn kill_server_does_not_wait_for_an_active_output_subscription() {
    let handler = RequestHandler::new();
    let (shutdown_handle, mut shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);
    let connection_id = 60;
    let pane = PaneOutputSubscriptionKey::new(
        SessionName::new("active").expect("valid session name"),
        PaneId::new(45),
    );
    let sender = pane_output_channel_with_limits(8, 1024);
    let receiver = sender.subscribe();
    {
        let mut subscriptions = handler
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        let subscription_id = subscriptions
            .registry
            .subscribe(connection_id, pane, Instant::now())
            .expect("subscription is within limits")
            .id();
        subscriptions.receivers.insert(subscription_id, receiver);
    }

    let Response::KillServer(_) = handler.handle_kill_server().await else {
        panic!("kill-server should acknowledge shutdown");
    };
    assert!(
        handler.request_shutdown_if_pending(),
        "explicit kill-server must bypass active SDK subscriptions"
    );
    tokio::time::timeout(Duration::from_millis(50), &mut shutdown_rx)
        .await
        .expect("shutdown should be requested immediately")
        .expect("shutdown receiver should complete cleanly");
    drop(sender);
}

async fn assert_retained_output_by_id(
    handler: &RequestHandler,
    connection_id: u64,
    session_name: SessionName,
    pane_id: PaneId,
    expected: &[u8],
) {
    let response = handler
        .handle_subscribe_pane_output_ref(
            connection_id,
            SubscribePaneOutputRefRequest {
                target: PaneTargetRef::by_id(session_name, pane_id),
                start: PaneOutputSubscriptionStart::Oldest,
            },
        )
        .await;
    let Response::SubscribePaneOutput(subscribe) = response else {
        panic!("retained by-id subscription should resolve");
    };

    let response = handler
        .handle_pane_output_cursor(
            connection_id,
            PaneOutputCursorRequest {
                subscription_id: subscribe.subscription_id,
                max_events: Some(8),
            },
        )
        .await;
    let Response::PaneOutputCursor(cursor) = response else {
        panic!("retained by-id subscription should return a cursor response");
    };
    assert_eq!(cursor.events[0].bytes, expected);
}
