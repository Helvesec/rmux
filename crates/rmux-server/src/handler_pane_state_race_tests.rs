use std::sync::Arc;
use std::time::Duration;

use super::RequestHandler;
use crate::pane_io::PaneExitEvent;
use crate::pane_state_journal::{PaneStateChange, PANE_STATE_JOURNAL_CAPACITY};
use rmux_core::{LifecycleEvent, PaneId};
use rmux_proto::{
    BreakPaneRequest, HookLifecycle, HookName, KillWindowRequest, LinkWindowRequest,
    MoveWindowRequest, MoveWindowTarget, NewSessionRequest, NewWindowRequest, PaneKillRequest,
    PaneOptionSetRequest, PaneStateClosedReason, PaneStateCursorRequest, PaneStateEventDto,
    PaneStateSubscriptionId, PaneTarget, PaneTargetRef, Request, RespawnPaneRequest, Response,
    ScopeSelector, SessionName, SetHookRequest, SetOptionMode, SplitDirection, SplitWindowRequest,
    SplitWindowTarget, SubscribePaneStateRequest, TerminalSize, UnlinkWindowRequest, WindowTarget,
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

async fn install_destructive_window_hook(handler: &RequestHandler, hook: HookName) {
    let response = handler
        .handle(Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Global,
            hook,
            command: "kill-window".to_owned(),
            lifecycle: HookLifecycle::Persistent,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

async fn create_window_direct(
    handler: &RequestHandler,
    session: &SessionName,
    index: u32,
) -> Response {
    handler
        .handle_new_window(
            std::process::id(),
            NewWindowRequest {
                target: session.clone(),
                name: None,
                detached: true,
                environment: None,
                command: None,
                start_directory: None,
                target_window_index: Some(index),
                insert_at_target: false,
                process_command: None,
            },
        )
        .await
}

async fn receive_window_unlinked(
    events: &mut tokio::sync::broadcast::Receiver<super::QueuedLifecycleEvent>,
    session_name: &SessionName,
) -> super::QueuedLifecycleEvent {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("window-unlinked event arrives")
            .expect("lifecycle sender remains open");
        if matches!(
            &event.event,
            LifecycleEvent::WindowUnlinked { session_name: event_session, .. }
                if event_session == session_name
        ) {
            return event;
        }
    }
}

async fn receive_window_linked(
    events: &mut tokio::sync::broadcast::Receiver<super::QueuedLifecycleEvent>,
    session_name: &SessionName,
) -> super::QueuedLifecycleEvent {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("window-linked event arrives")
            .expect("lifecycle sender remains open");
        if matches!(
            &event.event,
            LifecycleEvent::WindowLinked { session_name: event_session, .. }
                if event_session == session_name
        ) {
            return event;
        }
    }
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
async fn natural_exit_journals_closed_before_the_removed_pane_becomes_observable() {
    let handler = Arc::new(RequestHandler::new());
    let session = create_session(&handler, "a01-natural-exit").await;
    handler.wait_for_initial_panes_for_test().await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let (subscription_id, revision, pane_id) =
        subscribe(&handler, 1000, target.clone(), false).await;
    {
        let mut state = handler.state.lock().await;
        state
            .mark_pane_dead_without_exit_details(&target)
            .expect("mark pane naturally exited");
    }
    let pause = handler.install_pane_exit_commit_pause();
    let exit_handler = Arc::clone(&handler);
    let exiting = tokio::spawn(async move {
        exit_handler
            .handle_pane_exit_event(PaneExitEvent::eof_published(session, pane_id, None))
            .await;
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("natural exit reaches post-commit pause");

    match read_cursor(&handler, 1000, subscription_id, revision).await {
        Response::PaneStateCursor(response) => assert!(matches!(
            response.events.as_slice(),
            [PaneStateEventDto::Closed {
                pane_id: event_pane_id,
                reason: PaneStateClosedReason::Exited,
                ..
            }] if *event_pane_id == pane_id
        )),
        response => panic!("natural exit must expose Closed at removal commit: {response:?}"),
    }

    pause.release.notify_one();
    exiting.await.expect("natural exit task joins");
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
async fn kill_window_prepares_lifecycle_identity_before_same_slot_reuse() {
    let handler = Arc::new(RequestHandler::new());
    let session = create_session(&handler, "lifecycle-kill-slot-reuse").await;
    create_window(&handler, &session, 1).await;
    install_destructive_window_hook(&handler, HookName::WindowUnlinked).await;
    let mut events = handler.subscribe_lifecycle_events();
    let pause = handler.install_window_lifecycle_emit_pause();

    let kill_handler = Arc::clone(&handler);
    let kill_session = session.clone();
    let killing = tokio::spawn(async move {
        kill_handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(kill_session, 0),
                kill_all_others: false,
            }))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("kill-window reaches post-commit lifecycle pause");

    create_window(&handler, &session, 0).await;
    let replacement_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&session)
        .and_then(|session| session.window_at(0))
        .expect("replacement window exists")
        .id();
    pause.release.notify_one();
    let response = killing.await.expect("kill-window task joins");
    assert!(matches!(response, Response::KillWindow(_)), "{response:?}");

    let event = receive_window_unlinked(&mut events, &session).await;
    handler.dispatch_lifecycle_hook(event).await;
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .expect("replacement slot survives the destructive hook")
            .id(),
        replacement_id
    );
}

#[tokio::test]
async fn unlink_window_prepares_lifecycle_identity_before_same_slot_reuse() {
    let handler = Arc::new(RequestHandler::new());
    let owner = create_session(&handler, "lifecycle-unlink-slot-reuse").await;
    let external = create_session(&handler, "lifecycle-unlink-external").await;
    create_window(&handler, &owner, 1).await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(external, 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    install_destructive_window_hook(&handler, HookName::WindowUnlinked).await;
    let mut events = handler.subscribe_lifecycle_events();
    let pause = handler.install_window_lifecycle_emit_pause();

    let unlink_handler = Arc::clone(&handler);
    let unlink_owner = owner.clone();
    let unlinking = tokio::spawn(async move {
        unlink_handler
            .handle(Request::UnlinkWindow(UnlinkWindowRequest {
                target: WindowTarget::with_window(unlink_owner, 0),
                kill_if_last: false,
            }))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("unlink-window reaches post-commit lifecycle pause");

    create_window(&handler, &owner, 0).await;
    let replacement_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&owner)
        .and_then(|session| session.window_at(0))
        .expect("replacement window exists")
        .id();
    pause.release.notify_one();
    let response = unlinking.await.expect("unlink-window task joins");
    assert!(
        matches!(response, Response::UnlinkWindow(_)),
        "{response:?}"
    );

    let event = receive_window_unlinked(&mut events, &owner).await;
    handler.dispatch_lifecycle_hook(event).await;
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&owner)
            .and_then(|session| session.window_at(0))
            .expect("replacement slot survives the destructive hook")
            .id(),
        replacement_id
    );
}

#[tokio::test]
async fn new_window_prepares_lifecycle_identity_before_same_slot_reuse() {
    let handler = Arc::new(RequestHandler::new());
    let session = create_session(&handler, "lifecycle-new-slot-reuse").await;
    install_destructive_window_hook(&handler, HookName::WindowLinked).await;
    let mut events = handler.subscribe_lifecycle_events();
    let pause = handler.install_window_lifecycle_emit_pause();

    let create_handler = Arc::clone(&handler);
    let create_session = session.clone();
    let creating =
        tokio::spawn(
            async move { create_window_direct(&create_handler, &create_session, 1).await },
        );
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("new-window reaches post-commit lifecycle pause");

    let removed = handler
        .handle_kill_window(KillWindowRequest {
            target: WindowTarget::with_window(session.clone(), 1),
            kill_all_others: false,
        })
        .await;
    assert!(matches!(removed, Response::KillWindow(_)), "{removed:?}");
    let replacement = create_window_direct(&handler, &session, 1).await;
    assert!(
        matches!(replacement, Response::NewWindow(_)),
        "{replacement:?}"
    );
    let replacement_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&session)
        .and_then(|session| session.window_at(1))
        .expect("replacement window exists")
        .id();
    while events.try_recv().is_ok() {}
    pause.release.notify_one();
    let response = creating.await.expect("new-window task joins");
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");

    let event = receive_window_linked(&mut events, &session).await;
    handler.dispatch_lifecycle_hook(event).await;
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(1))
            .expect("replacement slot survives the destructive hook")
            .id(),
        replacement_id
    );
}

#[tokio::test]
async fn link_window_prepares_lifecycle_identity_before_same_slot_reuse() {
    let handler = Arc::new(RequestHandler::new());
    let source = create_session(&handler, "lifecycle-link-source").await;
    let destination = create_session(&handler, "lifecycle-link-destination").await;
    handler.wait_for_initial_panes_for_test().await;
    install_destructive_window_hook(&handler, HookName::WindowLinked).await;
    let mut events = handler.subscribe_lifecycle_events();
    let pause = handler.install_window_lifecycle_emit_pause();

    let link_handler = Arc::clone(&handler);
    let link_source = source.clone();
    let link_destination = destination.clone();
    let linking = tokio::spawn(async move {
        link_handler
            .handle_link_window(LinkWindowRequest {
                source: WindowTarget::with_window(link_source, 0),
                target: WindowTarget::with_window(link_destination, 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("link-window reaches post-commit lifecycle pause");

    let unlinked = handler
        .handle_unlink_window(UnlinkWindowRequest {
            target: WindowTarget::with_window(destination.clone(), 1),
            kill_if_last: false,
        })
        .await;
    assert!(
        matches!(unlinked, Response::UnlinkWindow(_)),
        "{unlinked:?}"
    );
    let replacement = create_window_direct(&handler, &destination, 1).await;
    assert!(
        matches!(replacement, Response::NewWindow(_)),
        "{replacement:?}"
    );
    let replacement_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&destination)
        .and_then(|session| session.window_at(1))
        .expect("replacement window exists")
        .id();
    while events.try_recv().is_ok() {}
    pause.release.notify_one();
    let response = linking.await.expect("link-window task joins");
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let event = receive_window_linked(&mut events, &destination).await;
    handler.dispatch_lifecycle_hook(event).await;
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&destination)
            .and_then(|session| session.window_at(1))
            .expect("replacement slot survives the destructive hook")
            .id(),
        replacement_id
    );
}

#[tokio::test]
async fn move_window_prepares_linked_identity_before_same_slot_reuse() {
    let handler = Arc::new(RequestHandler::new());
    let source = create_session(&handler, "lifecycle-move-source").await;
    let destination = create_session(&handler, "lifecycle-move-destination").await;
    handler.wait_for_initial_panes_for_test().await;
    install_destructive_window_hook(&handler, HookName::WindowLinked).await;
    let mut events = handler.subscribe_lifecycle_events();
    let pause = handler.install_window_lifecycle_emit_pause();

    let move_handler = Arc::clone(&handler);
    let move_source = source.clone();
    let move_destination = destination.clone();
    let moving = tokio::spawn(async move {
        move_handler
            .handle_move_window(MoveWindowRequest {
                source: Some(WindowTarget::with_window(move_source, 0)),
                target: MoveWindowTarget::Window(WindowTarget::with_window(move_destination, 1)),
                renumber: false,
                kill_destination: false,
                detached: true,
                after: false,
                before: false,
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("move-window reaches post-commit lifecycle pause");

    let removed = handler
        .handle_kill_window(KillWindowRequest {
            target: WindowTarget::with_window(destination.clone(), 1),
            kill_all_others: false,
        })
        .await;
    assert!(matches!(removed, Response::KillWindow(_)), "{removed:?}");
    let replacement = create_window_direct(&handler, &destination, 1).await;
    assert!(
        matches!(replacement, Response::NewWindow(_)),
        "{replacement:?}"
    );
    let replacement_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&destination)
        .and_then(|session| session.window_at(1))
        .expect("replacement window exists")
        .id();
    while events.try_recv().is_ok() {}
    pause.release.notify_one();
    let response = moving.await.expect("move-window task joins");
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    let event = receive_window_linked(&mut events, &destination).await;
    handler.dispatch_lifecycle_hook(event).await;
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&destination)
            .and_then(|session| session.window_at(1))
            .expect("replacement slot survives the destructive hook")
            .id(),
        replacement_id
    );
}

#[tokio::test]
async fn break_pane_prepares_linked_identity_before_same_slot_reuse() {
    let handler = Arc::new(RequestHandler::new());
    let session = create_session(&handler, "lifecycle-break-slot-reuse").await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    install_destructive_window_hook(&handler, HookName::WindowLinked).await;
    let mut events = handler.subscribe_lifecycle_events();
    let pause = handler.install_window_lifecycle_emit_pause();

    let break_handler = Arc::clone(&handler);
    let break_session = session.clone();
    let breaking = tokio::spawn(async move {
        break_handler
            .handle_break_pane(BreakPaneRequest {
                source: PaneTarget::with_window(break_session.clone(), 0, 1),
                target: Some(WindowTarget::with_window(break_session, 1)),
                name: None,
                detached: true,
                after: false,
                before: false,
                print_target: false,
                format: None,
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("break-pane reaches post-commit lifecycle pause");

    let removed = handler
        .handle_kill_window(KillWindowRequest {
            target: WindowTarget::with_window(session.clone(), 1),
            kill_all_others: false,
        })
        .await;
    assert!(matches!(removed, Response::KillWindow(_)), "{removed:?}");
    let replacement = create_window_direct(&handler, &session, 1).await;
    assert!(
        matches!(replacement, Response::NewWindow(_)),
        "{replacement:?}"
    );
    let replacement_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&session)
        .and_then(|session| session.window_at(1))
        .expect("replacement window exists")
        .id();
    while events.try_recv().is_ok() {}
    pause.release.notify_one();
    let response = breaking.await.expect("break-pane task joins");
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");

    let event = receive_window_linked(&mut events, &session).await;
    handler.dispatch_lifecycle_hook(event).await;
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(1))
            .expect("replacement slot survives the destructive hook")
            .id(),
        replacement_id
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
            assert_eq!(
                response.snapshot.revision,
                closed_revision.saturating_sub(1),
                "a closed subscription must rebase before its terminal event"
            );
            assert!(response.snapshot.revision < closed_revision);
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
async fn respawn_during_lag_rebase_does_not_snapshot_the_new_generation() {
    let handler = Arc::new(RequestHandler::new());
    let session = create_session(&handler, "a04-respawn-during-lag").await;
    handler.wait_for_initial_panes_for_test().await;
    let target = PaneTarget::with_window(session, 0, 0);
    let (subscription, initial_revision, pane_id) =
        subscribe(&handler, 1033, target.clone(), true).await;

    for index in 0..=PANE_STATE_JOURNAL_CAPACITY {
        handler.record_pane_state_change(
            pane_id,
            Some(1),
            PaneStateChange::TitleChanged {
                old: index.to_string(),
                new: (index + 1).to_string(),
            },
        );
    }
    let respawned = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        })))
        .await;
    assert!(
        matches!(respawned, Response::RespawnPane(_)),
        "{respawned:?}"
    );
    let closed_revision = handler
        .pane_state_journal
        .lock()
        .expect("pane-state journal lock")
        .current_revision();

    let changed = handler
        .handle(Request::PaneOptionSet(PaneOptionSetRequest {
            target: PaneTargetRef::slot(target),
            name: "@new-generation".to_owned(),
            value: Some("must-not-leak".to_owned()),
            mode: SetOptionMode::Replace,
            unset: false,
        }))
        .await;
    assert!(matches!(changed, Response::PaneOptionSet(_)), "{changed:?}");

    let snapshot_revision = match read_cursor(&handler, 1033, subscription, initial_revision).await
    {
        Response::PaneStateLag(response) => {
            assert_eq!(
                response.snapshot.revision,
                closed_revision.saturating_sub(1)
            );
            assert!(
                response.snapshot.options.is_empty(),
                "lag snapshot must not expose options from the respawned generation"
            );
            response.snapshot.revision
        }
        response => panic!("expected lag rebase, got {response:?}"),
    };
    assert_killed_closed(
        read_cursor(&handler, 1033, subscription, snapshot_revision).await,
        pane_id,
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
