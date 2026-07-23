use std::sync::atomic::Ordering;

use rmux_proto::{PaneTargetRef, Response, SubscribePaneStateRequest, UnsubscribePaneStateRequest};

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_reset_cannot_lose_an_interleaved_subscription() {
    let handler = Arc::new(RequestHandler::new());
    let (_session, target, pane_id) =
        create_session_with_pane(&handler, "foreground-watch-idle-handoff").await;
    let first_connection_id = 10_001;
    let first_subscription_id = match handler
        .handle_subscribe_pane_state(
            first_connection_id,
            SubscribePaneStateRequest {
                target: PaneTargetRef::slot(target.clone()),
                include_title: false,
                include_options: false,
                include_foreground: true,
            },
        )
        .await
    {
        Response::SubscribePaneState(response) => response.subscription_id,
        response => panic!("initial foreground subscription failed: {response:?}"),
    };
    assert!(
        handler.foreground_watch_started.load(Ordering::Acquire),
        "the initial subscription starts the foreground watcher"
    );

    // Remove the synchronous subscription baseline and require the registered
    // watcher to rebuild it. In debug/test builds its collection phase asserts
    // that the task-local lifecycle mutation depth is zero throughout every
    // OS process probe.
    handler
        .foreground_state_cache
        .lock()
        .expect("foreground state cache lock")
        .clear();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if handler
                .foreground_state_cache
                .lock()
                .expect("foreground state cache lock")
                .contains_key(&pane_id)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("registered watcher probes and rebuilds the foreground cache");

    let idle_pause = handler.install_foreground_watch_idle_pause();
    assert!(matches!(
        handler
            .handle_unsubscribe_pane_state(
                first_connection_id,
                UnsubscribePaneStateRequest {
                    subscription_id: first_subscription_id,
                },
            )
            .await,
        Response::UnsubscribePaneState(_)
    ));
    tokio::time::timeout(Duration::from_secs(3), idle_pause.reached.notified())
        .await
        .expect("watcher observes zero subscriptions before reset");

    // The watcher has decided to retire but still owns the journal lock. A
    // replacement can reserve lifecycle ownership and resolve state, but it
    // cannot publish its subscription until `started = false` is visible.
    let replacement_handler = Arc::clone(&handler);
    let replacement = tokio::spawn(async move {
        replacement_handler
            .handle_subscribe_pane_state(
                10_002,
                SubscribePaneStateRequest {
                    target: PaneTargetRef::slot(target),
                    include_title: false,
                    include_options: false,
                    include_foreground: true,
                },
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !replacement.is_finished(),
        "the replacement must not publish between the zero decision and watcher reset"
    );

    idle_pause.release();
    let replacement_response = tokio::time::timeout(Duration::from_secs(3), replacement)
        .await
        .expect("replacement subscription completes after idle reset")
        .expect("replacement subscription task joins");
    assert!(
        matches!(replacement_response, Response::SubscribePaneState(_)),
        "replacement foreground subscription failed: {replacement_response:?}"
    );
    assert!(
        handler.foreground_watch_started.load(Ordering::Acquire),
        "the replacement observes the retired watcher and starts a new one"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handoff_is_pre_admitted_across_lane_close() {
    let handler = Arc::new(RequestHandler::new());
    let (_session, target, _pane_id) =
        create_session_with_pane(&handler, "foreground-watch-shutdown-handoff").await;
    let spawn_pause =
        handler.install_pre_admitted_producer_spawn_pause("rmux-pane-foreground-watch");

    let subscribe_handler = Arc::clone(&handler);
    let post_close_target = target.clone();
    let subscribing = tokio::spawn(async move {
        subscribe_handler
            .handle_subscribe_pane_state(
                10_003,
                SubscribePaneStateRequest {
                    target: PaneTargetRef::slot(target),
                    include_title: false,
                    include_options: false,
                    include_foreground: true,
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), spawn_pause.reached.notified())
        .await
        .expect("subscription reaches the pre-admitted watcher handoff");
    assert_eq!(
        handler
            .pane_state_journal
            .lock()
            .expect("pane-state journal lock")
            .foreground_subscription_count(),
        1,
        "the subscription is observable before the watcher spawn handoff"
    );

    let close_handler = Arc::clone(&handler);
    let closing = tokio::spawn(async move {
        close_handler
            .close_normal_and_drain_lifecycle_producers()
            .await;
    });
    handler
        .wait_until_normal_lifecycle_producers_closing_for_test()
        .await;
    tokio::task::yield_now().await;
    assert!(
        !closing.is_finished(),
        "lane close must wait for the pre-admitted watcher handoff"
    );

    spawn_pause.release();
    let response = tokio::time::timeout(Duration::from_secs(3), subscribing)
        .await
        .expect("foreground subscription returns after watcher spawn")
        .expect("foreground subscription task joins");
    assert!(matches!(response, Response::SubscribePaneState(_)));
    tokio::time::timeout(Duration::from_secs(3), closing)
        .await
        .expect("normal producer lane drains after watcher cleanup")
        .expect("normal producer close task joins");
    assert!(
        !handler.foreground_watch_started.load(Ordering::Acquire),
        "cancelled watcher cleanup resets the ownership flag"
    );
    assert!(
        handler
            .foreground_state_cache
            .lock()
            .expect("foreground state cache lock")
            .is_empty(),
        "cancelled watcher cleanup removes the pre-probed cache baseline"
    );

    let rejected = handler
        .handle_subscribe_pane_state(
            10_004,
            SubscribePaneStateRequest {
                target: PaneTargetRef::slot(post_close_target),
                include_title: false,
                include_options: false,
                include_foreground: true,
            },
        )
        .await;
    assert!(
        matches!(rejected, Response::Error(_)),
        "a foreground watcher cannot be admitted after lane close: {rejected:?}"
    );
    assert_eq!(
        handler
            .pane_state_journal
            .lock()
            .expect("pane-state journal lock")
            .foreground_subscription_count(),
        1,
        "failed watcher admission must not publish another subscription"
    );
}
