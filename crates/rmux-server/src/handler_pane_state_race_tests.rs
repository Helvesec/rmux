use std::sync::Arc;
use std::time::Duration;

use super::RequestHandler;
use crate::pane_state_journal::{PaneStateChange, PANE_STATE_JOURNAL_CAPACITY};
use rmux_core::PaneId;
use rmux_proto::{
    LinkWindowRequest, MoveWindowRequest, MoveWindowTarget, NewSessionRequest, NewWindowRequest,
    PaneKillRequest, PaneOptionSetRequest, PaneStateClosedReason, PaneStateCursorRequest,
    PaneStateEventDto, PaneStateSubscriptionId, PaneTarget, PaneTargetRef, Request, Response,
    SessionName, SetOptionMode, SubscribePaneStateRequest, TerminalSize, UnlinkWindowRequest,
    WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_session(handler: &RequestHandler, value: &str) -> SessionName {
    let session = session_name(value);
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    session
}

async fn create_window(handler: &RequestHandler, session: &SessionName, index: u32) {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: Some(index),
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
}

async fn subscribe(
    handler: &RequestHandler,
    connection_id: u64,
    target: PaneTarget,
    include_options: bool,
) -> (PaneStateSubscriptionId, u64, PaneId) {
    match handler
        .handle_subscribe_pane_state(
            connection_id,
            SubscribePaneStateRequest {
                target: PaneTargetRef::slot(target),
                include_title: true,
                include_options,
                include_foreground: false,
            },
        )
        .await
    {
        Response::SubscribePaneState(response) => (
            response.subscription_id,
            response.snapshot.revision,
            response.pane_id,
        ),
        response => panic!("subscribe-pane-state failed: {response:?}"),
    }
}

async fn read_cursor(
    handler: &RequestHandler,
    connection_id: u64,
    subscription_id: PaneStateSubscriptionId,
    after_revision: u64,
) -> Response {
    handler
        .handle_pane_state_cursor(
            connection_id,
            PaneStateCursorRequest {
                subscription_id,
                after_revision,
                wait: false,
                max_events: Some(16),
            },
        )
        .await
}

fn assert_killed_closed(response: Response, pane_id: PaneId) {
    match response {
        Response::PaneStateCursor(response) => assert!(matches!(
            response.events.as_slice(),
            [PaneStateEventDto::Closed {
                pane_id: event_pane_id,
                reason: PaneStateClosedReason::Killed,
                ..
            }] if *event_pane_id == pane_id
        )),
        response => panic!("expected terminal Closed, got {response:?}"),
    }
}

fn move_request(source: WindowTarget, target: WindowTarget) -> Request {
    Request::MoveWindow(MoveWindowRequest {
        source: Some(source),
        target: MoveWindowTarget::Window(target),
        renumber: false,
        kill_destination: true,
        detached: true,
        after: false,
        before: false,
    })
}

#[tokio::test]
async fn move_window_k_journals_the_destination_removed_at_commit_for_two_connections() {
    let handler = Arc::new(RequestHandler::new());
    let source = create_session(&handler, "a01-move-source").await;
    let destination = create_session(&handler, "a01-move-destination").await;
    let interloper = create_session(&handler, "a01-move-interloper").await;
    handler.wait_for_initial_panes_for_test().await;
    let destination_target = PaneTarget::with_window(destination.clone(), 0, 0);
    let (old_subscription, old_revision, old_pane_id) =
        subscribe(&handler, 1001, destination_target.clone(), false).await;

    let pause = handler.install_window_lifecycle_mutation_pause();
    let move_handler = Arc::clone(&handler);
    let move_source = source.clone();
    let move_destination = destination.clone();
    let moving = tokio::spawn(async move {
        move_handler
            .handle(move_request(
                WindowTarget::with_window(move_source, 0),
                WindowTarget::with_window(move_destination, 0),
            ))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("move-window reaches pre-mutation pause");

    let interposed = handler
        .handle(move_request(
            WindowTarget::with_window(interloper, 0),
            WindowTarget::with_window(destination.clone(), 0),
        ))
        .await;
    assert!(
        matches!(interposed, Response::MoveWindow(_)),
        "{interposed:?}"
    );
    assert_killed_closed(
        read_cursor(&handler, 1001, old_subscription, old_revision).await,
        old_pane_id,
    );

    let (current_subscription, current_revision, current_pane_id) =
        subscribe(&handler, 1002, destination_target, false).await;
    assert_ne!(current_pane_id, old_pane_id);
    pause.release.notify_one();
    let moved = moving.await.expect("move-window task joins");
    assert!(matches!(moved, Response::MoveWindow(_)), "{moved:?}");
    assert_killed_closed(
        read_cursor(&handler, 1002, current_subscription, current_revision).await,
        current_pane_id,
    );
}

#[tokio::test]
async fn link_window_journals_the_destination_removed_at_commit_for_two_connections() {
    let handler = Arc::new(RequestHandler::new());
    let source = create_session(&handler, "a01-link-source").await;
    let destination = create_session(&handler, "a01-link-destination").await;
    let interloper = create_session(&handler, "a01-link-interloper").await;
    handler.wait_for_initial_panes_for_test().await;
    let destination_target = PaneTarget::with_window(destination.clone(), 0, 0);
    let (old_subscription, old_revision, old_pane_id) =
        subscribe(&handler, 1011, destination_target.clone(), false).await;

    let pause = handler.install_window_lifecycle_mutation_pause();
    let link_handler = Arc::clone(&handler);
    let link_source = source.clone();
    let link_destination = destination.clone();
    let linking = tokio::spawn(async move {
        link_handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(link_source, 0),
                target: WindowTarget::with_window(link_destination, 0),
                after: false,
                before: false,
                kill_destination: true,
                detached: true,
            }))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("link-window reaches pre-mutation pause");

    let interposed = handler
        .handle(move_request(
            WindowTarget::with_window(interloper, 0),
            WindowTarget::with_window(destination.clone(), 0),
        ))
        .await;
    assert!(
        matches!(interposed, Response::MoveWindow(_)),
        "{interposed:?}"
    );
    assert_killed_closed(
        read_cursor(&handler, 1011, old_subscription, old_revision).await,
        old_pane_id,
    );

    let (current_subscription, current_revision, current_pane_id) =
        subscribe(&handler, 1012, destination_target, false).await;
    pause.release.notify_one();
    let linked = linking.await.expect("link-window task joins");
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    assert_killed_closed(
        read_cursor(&handler, 1012, current_subscription, current_revision).await,
        current_pane_id,
    );
}

#[tokio::test]
async fn unlink_window_journals_last_link_decided_at_commit_for_two_connections() {
    let handler = Arc::new(RequestHandler::new());
    let owner = create_session(&handler, "a01-unlink-owner").await;
    let peer = create_session(&handler, "a01-unlink-peer").await;
    create_window(&handler, &owner, 1).await;
    create_window(&handler, &peer, 1).await;
    handler.wait_for_initial_panes_for_test().await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(peer.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");

    let watched_target = PaneTarget::with_window(owner.clone(), 0, 0);
    let (first_subscription, first_revision, shared_pane_id) =
        subscribe(&handler, 1021, watched_target.clone(), false).await;
    let (second_subscription, second_revision, second_pane_id) =
        subscribe(&handler, 1022, watched_target, false).await;
    assert_eq!(second_pane_id, shared_pane_id);

    let pause = handler.install_window_lifecycle_mutation_pause();
    let unlink_handler = Arc::clone(&handler);
    let unlink_owner = owner.clone();
    let unlinking = tokio::spawn(async move {
        unlink_handler
            .handle(Request::UnlinkWindow(UnlinkWindowRequest {
                target: WindowTarget::with_window(unlink_owner, 0),
                kill_if_last: true,
            }))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("unlink-window reaches pre-mutation pause");

    let peer_unlinked = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: WindowTarget::with_window(peer, 0),
            kill_if_last: false,
        }))
        .await;
    assert!(
        matches!(peer_unlinked, Response::UnlinkWindow(_)),
        "{peer_unlinked:?}"
    );
    pause.release.notify_one();
    let owner_unlinked = unlinking.await.expect("unlink-window task joins");
    assert!(
        matches!(owner_unlinked, Response::UnlinkWindow(_)),
        "{owner_unlinked:?}"
    );

    assert_killed_closed(
        read_cursor(&handler, 1021, first_subscription, first_revision).await,
        shared_pane_id,
    );
    assert_killed_closed(
        read_cursor(&handler, 1022, second_subscription, second_revision).await,
        shared_pane_id,
    );
}

#[tokio::test]
async fn kill_during_lag_rebase_returns_current_snapshot_then_terminal_closed() {
    let handler = Arc::new(RequestHandler::new());
    let session = create_session(&handler, "a03-kill-during-lag").await;
    handler.wait_for_initial_panes_for_test().await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let (lag_subscription, lag_revision, watched_pane_id) =
        subscribe(&handler, 1031, target.clone(), false).await;
    for index in 0..=PANE_STATE_JOURNAL_CAPACITY {
        handler.record_pane_state_change(
            watched_pane_id,
            Some(1),
            PaneStateChange::TitleChanged {
                old: index.to_string(),
                new: (index + 1).to_string(),
            },
        );
    }
    let (second_subscription, second_revision, _) =
        subscribe(&handler, 1032, target.clone(), false).await;

    let pause = handler.install_pane_state_lag_rebase_pause();
    let cursor_handler = Arc::clone(&handler);
    let lag_cursor = tokio::spawn(async move {
        read_cursor(&cursor_handler, 1031, lag_subscription, lag_revision).await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("cursor reaches lag snapshot pause");

    let killed = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(session, watched_pane_id),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)), "{killed:?}");
    let closed_revision = handler
        .pane_state_journal
        .lock()
        .expect("pane-state journal lock")
        .current_revision();
    assert_killed_closed(
        read_cursor(&handler, 1032, second_subscription, second_revision).await,
        watched_pane_id,
    );

    pause.release.notify_one();
    let lag = lag_cursor.await.expect("lag cursor task joins");
    let snapshot_revision = match lag {
        Response::PaneStateLag(response) => {
            assert_eq!(response.snapshot.revision, closed_revision);
            assert!(response.snapshot.revision >= response.resume_revision);
            response.snapshot.revision
        }
        response => panic!("kill during lag must not return pane-not-found: {response:?}"),
    };
    assert_killed_closed(
        read_cursor(&handler, 1031, lag_subscription, snapshot_revision).await,
        watched_pane_id,
    );
}

#[tokio::test]
async fn pane_option_mutation_and_journal_order_are_linearized() {
    let handler = Arc::new(RequestHandler::new());
    let session = create_session(&handler, "pane-option-linearized").await;
    handler.wait_for_initial_panes_for_test().await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let (subscription, revision, pane_id) = subscribe(&handler, 1041, target.clone(), true).await;

    let pause = handler.install_pane_option_journal_pause();
    let first_handler = Arc::clone(&handler);
    let first_target = target.clone();
    let first = tokio::spawn(async move {
        first_handler
            .handle(Request::PaneOptionSet(PaneOptionSetRequest {
                target: PaneTargetRef::slot(first_target),
                name: "@linearized".to_owned(),
                value: Some("first".to_owned()),
                mode: SetOptionMode::Replace,
                unset: false,
            }))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("first option mutation reaches journal pause");

    let second_handler = Arc::clone(&handler);
    let mut second = tokio::spawn(async move {
        second_handler
            .handle(Request::PaneOptionSet(PaneOptionSetRequest {
                target: PaneTargetRef::by_id(session, pane_id),
                name: "@linearized".to_owned(),
                value: Some("second".to_owned()),
                mode: SetOptionMode::Replace,
                unset: false,
            }))
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut second)
            .await
            .is_err(),
        "second mutation must remain blocked while the first owns state+journal ordering"
    );
    pause.release.notify_one();
    assert!(matches!(
        first.await.expect("first mutation joins"),
        Response::PaneOptionSet(_)
    ));
    assert!(matches!(
        second.await.expect("second mutation joins"),
        Response::PaneOptionSet(_)
    ));

    match read_cursor(&handler, 1041, subscription, revision).await {
        Response::PaneStateCursor(response) => assert!(matches!(
            response.events.as_slice(),
            [
                PaneStateEventDto::OptionSet {
                    old_value: None,
                    new_value: first,
                    ..
                },
                PaneStateEventDto::OptionSet {
                    old_value: Some(old),
                    new_value: second,
                    ..
                },
            ] if first == "first" && old == "first" && second == "second"
        )),
        response => panic!("pane option cursor failed: {response:?}"),
    }
}
