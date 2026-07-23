use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use super::pane_group_transfer_tests::create_grouped_session;
use super::{QueuedLifecycleEvent, RequestHandler};
use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};
use crate::pane_io::{AttachControl, PaneExitEvent};
use rmux_core::{command_parser::CommandParser, LifecycleEvent};
use rmux_proto::{
    ClientTerminalContext, ControlMode, DetachClientExtRequest, JoinPaneRequest, KillPaneRequest,
    KillSessionRequest, KillWindowRequest, LinkWindowRequest, MovePaneRequest, MoveWindowRequest,
    MoveWindowTarget, NewSessionRequest, NewWindowRequest, OptionName, PaneKillRequest, PaneTarget,
    PaneTargetRef, RenameSessionRequest, Request, Response, ScopeSelector, SessionName,
    SetOptionMode, SplitDirection, TerminalSize, WaitForMode, WaitForRequest, WindowTarget,
};
use tokio::sync::mpsc;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn new_session(handler: &RequestHandler, session_name: &SessionName) {
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)));
}

async fn new_window(handler: &RequestHandler, session_name: &SessionName) -> WindowTarget {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;

    let Response::NewWindow(response) = response else {
        panic!("expected new-window response");
    };
    response.target
}

async fn register_control_session(
    handler: &RequestHandler,
    requester_pid: u32,
    session_name: SessionName,
) -> mpsc::Receiver<ControlServerEvent> {
    let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let _control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default()
                    .with_client_terminal(&ClientTerminalContext {
                        terminal_features: Vec::new(),
                        utf8: true,
                    }),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(requester_pid, Some(session_name))
        .await
        .expect("control session set succeeds");
    event_rx
}

#[tokio::test]
async fn same_pid_control_replacement_emits_one_client_detached_for_old_identity() {
    const CONTROL_PID: u32 = 42_460;
    const KEEPALIVE_PID: u32 = 42_461;

    let handler = RequestHandler::new();
    let session = session_name("same-pid-control-replacement");
    new_session(&handler, &session).await;
    let mut keepalive = register_control_session(&handler, KEEPALIVE_PID, session.clone()).await;
    let _ = drain_control_events(&mut keepalive);
    let mut old_events = register_control_session(&handler, CONTROL_PID, session.clone()).await;
    let _ = drain_control_events(&mut old_events);
    let (old_control_id, session_id) = {
        let active_control = handler.active_control.lock().await;
        let old = active_control
            .by_pid
            .get(&CONTROL_PID)
            .expect("old control registration exists");
        (
            old.id,
            old.session_id
                .expect("old control is attached to a session"),
        )
    };
    let mut lifecycle = handler.subscribe_lifecycle_events();

    let (replacement_tx, _replacement_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let replacement_id = handler
        .register_control_with_closing(
            CONTROL_PID,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            replacement_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    assert_ne!(replacement_id, old_control_id);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), old_events.recv()).await,
        Ok(Some(ControlServerEvent::Exit(None)))
    ));

    let detached = tokio::time::timeout(Duration::from_secs(1), lifecycle.recv())
        .await
        .expect("replacement publishes client-detached")
        .expect("lifecycle channel remains open");
    assert_eq!(detached.control_session_identity, Some(session_id));
    assert!(matches!(
        detached.event,
        LifecycleEvent::ClientDetached {
            session_name,
            client_name: Some(client_name),
        } if session_name == session && client_name == CONTROL_PID.to_string()
    ));
    let active_control = handler.active_control.lock().await;
    let replacement = active_control
        .by_pid
        .get(&CONTROL_PID)
        .expect("replacement control remains registered");
    assert_eq!(replacement.id, replacement_id);
    assert_eq!(
        (replacement.session_name.as_ref(), replacement.session_id),
        (None, None)
    );
    drop(active_control);

    handler.finish_control(CONTROL_PID, old_control_id).await;
    assert!(matches!(
        lifecycle.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    handler.finish_control(CONTROL_PID, replacement_id).await;
}

#[tokio::test]
async fn same_pid_control_replacement_destroys_the_old_unattached_session() {
    const CONTROL_PID: u32 = 42_464;

    let handler = RequestHandler::new();
    let session = session_name("same-pid-replacement-destroy-unattached");
    new_session(&handler, &session).await;
    let session_id = {
        let mut state = handler.state.lock().await;
        let session_id = state
            .sessions
            .session(&session)
            .expect("session exists")
            .id();
        state
            .options
            .set(
                ScopeSelector::Session(session.clone()),
                OptionName::DestroyUnattached,
                "on".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("destroy-unattached option is valid");
        session_id
    };
    let _old_events = register_control_session(&handler, CONTROL_PID, session.clone()).await;
    let old_control_id = handler.active_control.lock().await.by_pid[&CONTROL_PID].id;
    let mut lifecycle = handler.subscribe_lifecycle_events();

    let (replacement_tx, _replacement_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let replacement_id = handler
        .register_control_with_closing(
            CONTROL_PID,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            replacement_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;

    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&session)
        .is_none());
    let active_control = handler.active_control.lock().await;
    let replacement = active_control
        .by_pid
        .get(&CONTROL_PID)
        .expect("replacement control remains registered");
    assert_eq!(replacement.id, replacement_id);
    assert_eq!(
        (replacement.session_name.as_ref(), replacement.session_id),
        (None, None)
    );
    drop(active_control);

    let events = tokio::time::timeout(Duration::from_secs(1), async {
        let mut events = Vec::new();
        loop {
            let event = lifecycle
                .recv()
                .await
                .expect("lifecycle channel remains open");
            let session_closed = matches!(
                &event.event,
                LifecycleEvent::SessionClosed {
                    session_name: closed_session,
                    session_id: Some(closed_id),
                } if closed_session == &session && *closed_id == session_id.as_u32()
            );
            events.push(event);
            if session_closed {
                break events;
            }
        }
    })
    .await
    .expect("replacement publishes detach and session-close events");
    let detached_positions = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            matches!(
                &event.event,
                LifecycleEvent::ClientDetached {
                    session_name: detached_session,
                    client_name: Some(client_name),
                } if detached_session == &session && client_name == &CONTROL_PID.to_string()
            )
            .then_some(index)
        })
        .collect::<Vec<_>>();
    assert_eq!(detached_positions.len(), 1);
    assert_eq!(
        events[detached_positions[0]].control_session_identity,
        Some(session_id)
    );
    let closed_position = events
        .iter()
        .position(|event| matches!(&event.event, LifecycleEvent::SessionClosed { .. }))
        .expect("session-close event is present");
    assert!(detached_positions[0] < closed_position);

    handler.finish_control(CONTROL_PID, old_control_id).await;
    handler.finish_control(CONTROL_PID, replacement_id).await;
}

#[tokio::test]
async fn finished_control_identity_is_not_lost_before_same_pid_replacement() {
    const CONTROL_PID: u32 = 42_465;
    const KEEPALIVE_PID: u32 = 42_466;

    let handler = RequestHandler::new();
    let session = session_name("finished-control-before-same-pid-replacement");
    new_session(&handler, &session).await;
    let mut keepalive = register_control_session(&handler, KEEPALIVE_PID, session.clone()).await;
    let _ = drain_control_events(&mut keepalive);
    let _old_events = register_control_session(&handler, CONTROL_PID, session.clone()).await;
    let (old_control_id, old_session_id) = {
        let active_control = handler.active_control.lock().await;
        let old = &active_control.by_pid[&CONTROL_PID];
        (
            old.id,
            old.session_id
                .expect("old control is attached to a session"),
        )
    };
    let mut lifecycle = handler.subscribe_lifecycle_events();

    handler.finish_control(CONTROL_PID, old_control_id).await;
    let (replacement_tx, _replacement_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let replacement_id = handler
        .register_control_with_closing(
            CONTROL_PID,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            replacement_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;

    let detached = lifecycle
        .try_recv()
        .expect("finishing the old identity publishes client-detached");
    assert_eq!(detached.control_session_identity, Some(old_session_id));
    assert!(matches!(
        detached.event,
        LifecycleEvent::ClientDetached {
            session_name,
            client_name: Some(client_name),
        } if session_name == session && client_name == CONTROL_PID.to_string()
    ));
    assert!(matches!(
        lifecycle.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    handler.finish_control(CONTROL_PID, replacement_id).await;
}

#[tokio::test]
async fn finish_control_does_not_duplicate_an_explicit_client_detached_event() {
    const CONTROL_PID: u32 = 42_467;
    const KEEPALIVE_PID: u32 = 42_468;

    let handler = RequestHandler::new();
    let session = session_name("control-explicit-detach-once");
    new_session(&handler, &session).await;
    let mut keepalive = register_control_session(&handler, KEEPALIVE_PID, session.clone()).await;
    let _ = drain_control_events(&mut keepalive);
    let mut control_events = register_control_session(&handler, CONTROL_PID, session.clone()).await;
    let _ = drain_control_events(&mut control_events);
    let control_id = handler.active_control.lock().await.by_pid[&CONTROL_PID].id;
    for index in 0..CONTROL_SERVER_EVENT_CAPACITY {
        handler
            .send_control_notification_to(CONTROL_PID, format!("%message queued-{index}"))
            .await;
    }
    assert_eq!(control_events.len(), CONTROL_SERVER_EVENT_CAPACITY);
    let mut lifecycle = handler.subscribe_lifecycle_events();

    let outcome = handler
        .exit_control_client_for_identity(CONTROL_PID, control_id, None)
        .await
        .expect("explicit control detach succeeds");
    {
        let active_control = handler.active_control.lock().await;
        let active = active_control
            .by_pid
            .get(&CONTROL_PID)
            .expect("failed Exit delivery stays registered until transport finish");
        assert_eq!(active.id, control_id);
        assert!(active.closing.load(std::sync::atomic::Ordering::SeqCst));
    }
    handler
        .emit_prepared(
            outcome
                .lifecycle_event
                .expect("explicit detach prepares client-detached"),
        )
        .await;
    handler.finish_control(CONTROL_PID, control_id).await;
    assert!(control_events.is_closed());

    assert!(matches!(
        lifecycle.try_recv(),
        Ok(event)
            if matches!(
                event.event,
                LifecycleEvent::ClientDetached {
                    session_name: ref detached_session,
                    client_name: Some(ref client_name),
                } if detached_session == &session && client_name == &CONTROL_PID.to_string()
            )
    ));
    assert!(matches!(
        lifecycle.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn explicit_control_detach_claim_prevents_same_pid_replacement_duplicate() {
    const CONTROL_PID: u32 = 42_462;
    const KEEPALIVE_PID: u32 = 42_463;

    let handler = RequestHandler::new();
    let session = session_name("explicit-detach-before-control-replacement");
    new_session(&handler, &session).await;
    let mut keepalive = register_control_session(&handler, KEEPALIVE_PID, session.clone()).await;
    let _ = drain_control_events(&mut keepalive);
    let _old_events = register_control_session(&handler, CONTROL_PID, session.clone()).await;
    let (old_control_id, old_session_id) = {
        let active_control = handler.active_control.lock().await;
        let old = &active_control.by_pid[&CONTROL_PID];
        (
            old.id,
            old.session_id
                .expect("old control is attached to a session"),
        )
    };
    let mut lifecycle = handler.subscribe_lifecycle_events();

    let detached_event = handler
        .exit_control_client_for_identity(CONTROL_PID, old_control_id, None)
        .await
        .expect("explicit control detach succeeds")
        .lifecycle_event
        .expect("attached control prepares client-detached");
    assert_eq!(
        detached_event.control_session_identity,
        Some(old_session_id)
    );

    let (replacement_tx, _replacement_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let replacement_id = handler
        .register_control_with_closing(
            CONTROL_PID,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            replacement_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    assert!(matches!(
        lifecycle.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));

    handler.emit_prepared(detached_event).await;
    assert!(matches!(
        lifecycle.try_recv(),
        Ok(event)
            if event.control_session_identity == Some(old_session_id) && matches!(
                event.event,
                LifecycleEvent::ClientDetached {
                    session_name: ref detached_session,
                    client_name: Some(ref client_name),
                } if detached_session == &session && client_name == &CONTROL_PID.to_string()
            )
    ));
    assert!(matches!(
        lifecycle.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));

    handler.finish_control(CONTROL_PID, old_control_id).await;
    handler.finish_control(CONTROL_PID, replacement_id).await;
}

#[tokio::test]
async fn target_session_detach_does_not_duplicate_a_claimed_control_event() {
    const CONTROL_PID: u32 = 42_469;

    let handler = RequestHandler::new();
    let session = session_name("target-session-claimed-control-detach");
    new_session(&handler, &session).await;
    let _control_events = register_control_session(&handler, CONTROL_PID, session.clone()).await;
    let (control_id, session_id) = {
        let active_control = handler.active_control.lock().await;
        let active = &active_control.by_pid[&CONTROL_PID];
        (
            active.id,
            active
                .session_id
                .expect("control is attached to the target session"),
        )
    };
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let explicit_event = handler
        .exit_control_client_for_identity(CONTROL_PID, control_id, None)
        .await
        .expect("explicit control detach succeeds")
        .lifecycle_event
        .expect("explicit detach claims client-detached");

    let response = handler
        .handle(Request::DetachClientExt(DetachClientExtRequest {
            target_client: None,
            all_other_clients: false,
            target_session: Some(session.clone()),
            kill_on_detach: false,
            exec_command: None,
        }))
        .await;
    assert!(
        matches!(response, Response::DetachClient(_)),
        "{response:?}"
    );
    handler.emit_prepared(explicit_event).await;

    let detached = std::iter::from_fn(|| lifecycle.try_recv().ok())
        .filter(|event| {
            matches!(
                &event.event,
                LifecycleEvent::ClientDetached {
                    session_name: detached_session,
                    client_name: Some(client_name),
                } if detached_session == &session && client_name == &CONTROL_PID.to_string()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(detached.len(), 1);
    assert_eq!(detached[0].control_session_identity, Some(session_id));
}

#[tokio::test]
async fn target_session_detach_pins_each_control_session_identity() {
    const FIRST_PID: u32 = 42_470;
    const SECOND_PID: u32 = 42_471;

    let handler = RequestHandler::new();
    let session = session_name("target-session-stable-control-detach");
    new_session(&handler, &session).await;
    let _first_events = register_control_session(&handler, FIRST_PID, session.clone()).await;
    let _second_events = register_control_session(&handler, SECOND_PID, session.clone()).await;
    let session_id = handler.active_control.lock().await.by_pid[&FIRST_PID]
        .session_id
        .expect("first control is attached to the target session");
    let mut lifecycle = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::DetachClientExt(DetachClientExtRequest {
            target_client: None,
            all_other_clients: false,
            target_session: Some(session.clone()),
            kill_on_detach: false,
            exec_command: None,
        }))
        .await;
    assert!(
        matches!(response, Response::DetachClient(_)),
        "{response:?}"
    );

    let detached = std::iter::from_fn(|| lifecycle.try_recv().ok())
        .filter_map(|event| match event.event {
            LifecycleEvent::ClientDetached {
                session_name: detached_session,
                client_name: Some(client_name),
            } if detached_session == session => {
                assert_eq!(event.control_session_identity, Some(session_id));
                Some(client_name)
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        detached,
        BTreeSet::from([FIRST_PID.to_string(), SECOND_PID.to_string()])
    );
}

#[tokio::test]
async fn rename_session_commits_control_identity_before_a_concurrent_kill() {
    let handler = RequestHandler::new();
    let alpha = session_name("rename-control-atomic-alpha");
    let beta = session_name("rename-control-atomic-beta");
    new_session(&handler, &alpha).await;
    let requester_pid = 42_456;
    let mut events = register_control_session(&handler, requester_pid, alpha.clone()).await;
    assert!(matches!(
        events.try_recv(),
        Ok(ControlServerEvent::SessionChanged(Some(ref session_name)))
            | Ok(ControlServerEvent::SessionChangedAt {
                ref session_name,
                ..
            })
            if session_name == &alpha
    ));
    let pause = handler.install_rename_session_control_commit_pause(alpha.clone());

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

    pause.reached.notified().await;
    let kill_handler = handler.clone();
    let kill_beta = beta.clone();
    let kill = tokio::spawn(async move {
        kill_handler
            .handle(Request::KillSession(KillSessionRequest {
                target: kill_beta,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await
    });
    tokio::task::yield_now().await;
    pause.release.notify_one();

    assert!(matches!(
        rename.await.expect("rename task joins"),
        Response::RenameSession(_)
    ));
    assert!(matches!(
        kill.await.expect("kill task joins"),
        Response::KillSession(_)
    ));

    let mut renamed = false;
    let mut exited = false;
    while let Ok(event) = events.try_recv() {
        renamed |= matches!(
            event,
            ControlServerEvent::SessionChanged(Some(ref session_name)) if session_name == &beta
        );
        exited |= matches!(event, ControlServerEvent::Exit(_));
    }
    assert!(renamed, "control must observe the committed rename");
    assert!(exited, "concurrent kill must exit the renamed control");
    let active_control = handler.active_control.lock().await;
    let active = active_control
        .by_pid
        .get(&requester_pid)
        .expect("closing control remains until its transport finishes");
    assert_eq!(active.session_name.as_ref(), Some(&beta));
    assert!(active.closing.load(std::sync::atomic::Ordering::SeqCst));
}

async fn dispatch_as(handler: &RequestHandler, requester_pid: u32, request: Request) -> Response {
    let mut lifecycle_events = handler.subscribe_lifecycle_events();
    let outcome = handler.dispatch(requester_pid, request).await;

    loop {
        match lifecycle_events.try_recv() {
            Ok(event) => handler.dispatch_lifecycle_hook(event).await,
            Err(
                tokio::sync::broadcast::error::TryRecvError::Empty
                | tokio::sync::broadcast::error::TryRecvError::Closed,
            ) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                panic!("lifecycle events lagged during test: {skipped}");
            }
        }
    }

    outcome.response
}

fn drain_control_events(rx: &mut mpsc::Receiver<ControlServerEvent>) -> Vec<ControlServerEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

fn drain_destroy_lifecycle_order(
    events: &mut tokio::sync::broadcast::Receiver<QueuedLifecycleEvent>,
) -> Vec<&'static str> {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|queued| match queued.event {
            LifecycleEvent::WindowLinked { .. } => Some("window-linked"),
            LifecycleEvent::WindowUnlinked { .. } => Some("window-unlinked"),
            LifecycleEvent::SessionClosed { .. } => Some("session-closed"),
            LifecycleEvent::ClientSessionChanged { .. } => Some("client-session-changed"),
            _ => None,
        })
        .collect()
}

fn assert_has_exit(events: &[ControlServerEvent]) {
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ControlServerEvent::Exit(None))),
        "control client must receive %exit after target deletion, got {events:?}"
    );
}

fn assert_has_no_exit(events: &[ControlServerEvent]) {
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ControlServerEvent::Exit(_))),
        "control client must stay open, got {events:?}"
    );
}

fn assert_control_switched_to(events: &[ControlServerEvent], expected: &SessionName) {
    assert_has_no_exit(events);
    assert!(
        events.iter().any(|event| matches!(
            event,
            ControlServerEvent::SessionChanged(Some(session_name))
                | ControlServerEvent::SessionChangedAt { session_name, .. }
                if session_name == expected
        )),
        "control client must switch to {expected}, got {events:?}"
    );
}

fn assert_attach_switched_to(
    events: &mut mpsc::UnboundedReceiver<AttachControl>,
    expected: &SessionName,
) {
    let switched = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
        AttachControl::Switch(target) => Some(target.into_target().session_name),
        _ => None,
    });
    assert_eq!(switched.as_ref(), Some(expected));
}

fn assert_teardown_precedes_control_changes(
    order: &[&'static str],
    expected_teardown_events: usize,
    expected_control_changes: usize,
) {
    let first_control = order
        .iter()
        .position(|event| *event == "client-session-changed")
        .expect("at least one control rehome event is published");
    assert_eq!(first_control, expected_teardown_events, "{order:?}");
    assert_eq!(
        order[first_control..]
            .iter()
            .filter(|event| **event == "client-session-changed")
            .count(),
        expected_control_changes,
        "{order:?}"
    );
    assert!(
        order[first_control..]
            .iter()
            .all(|event| *event == "client-session-changed"),
        "no teardown event may be published after a control rehome: {order:?}"
    );
}

async fn set_detach_on_destroy(handler: &RequestHandler, session_name: &SessionName, value: &str) {
    handler
        .state
        .lock()
        .await
        .options
        .set(
            ScopeSelector::Session(session_name.clone()),
            OptionName::DetachOnDestroy,
            value.to_owned(),
            SetOptionMode::Replace,
        )
        .expect("detach-on-destroy value is valid");
}

#[tokio::test]
async fn kill_session_publishes_teardown_before_destroy_rehome_without_deadlock() {
    let handler = RequestHandler::new();
    let survivor = session_name("kill-session-order-survivor");
    let destroyed = session_name("kill-session-order-destroyed");
    new_session(&handler, &survivor).await;
    new_session(&handler, &destroyed).await;
    set_detach_on_destroy(&handler, &destroyed, "off").await;
    let (attach_tx, _attach_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(43_060, destroyed.clone(), attach_tx)
        .await;
    let mut lifecycle_events = handler.subscribe_lifecycle_events();

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        handler.handle(Request::KillSession(KillSessionRequest {
            target: destroyed,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        })),
    )
    .await
    .expect("kill-session with destroy rehome must not wait on its own later ticket");
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
    assert_eq!(
        drain_destroy_lifecycle_order(&mut lifecycle_events),
        vec![
            "session-closed",
            "window-unlinked",
            "client-session-changed",
        ]
    );
}

#[tokio::test]
async fn kill_session_all_except_rehomes_every_removed_session_after_teardown() {
    let handler = RequestHandler::new();
    let survivor = session_name("kill-all-except-survivor");
    let alpha = session_name("kill-all-except-alpha");
    let beta = session_name("kill-all-except-beta");
    for session in [&survivor, &alpha, &beta] {
        new_session(&handler, session).await;
    }
    set_detach_on_destroy(&handler, &alpha, "off").await;
    set_detach_on_destroy(&handler, &beta, "off").await;
    let mut alpha_control = register_control_session(&handler, 43_063, alpha.clone()).await;
    let mut beta_control = register_control_session(&handler, 43_064, beta.clone()).await;
    let _ = drain_control_events(&mut alpha_control);
    let _ = drain_control_events(&mut beta_control);
    let (alpha_attach_tx, mut alpha_attach) = mpsc::unbounded_channel();
    let (beta_attach_tx, mut beta_attach) = mpsc::unbounded_channel();
    handler
        .register_attach(43_065, alpha.clone(), alpha_attach_tx)
        .await;
    handler
        .register_attach(43_066, beta.clone(), beta_attach_tx)
        .await;
    let mut lifecycle_events = handler.subscribe_lifecycle_events();

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        handler.handle(Request::KillSession(KillSessionRequest {
            target: survivor.clone(),
            kill_all_except_target: true,
            clear_alerts: false,
            kill_group: false,
        })),
    )
    .await
    .expect("kill-session -a rehomes all removed clients without deadlock");
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");

    let order = drain_destroy_lifecycle_order(&mut lifecycle_events);
    assert_teardown_precedes_control_changes(&order, 4, 4);
    assert_control_switched_to(&drain_control_events(&mut alpha_control), &survivor);
    assert_control_switched_to(&drain_control_events(&mut beta_control), &survivor);
    assert_attach_switched_to(&mut alpha_attach, &survivor);
    assert_attach_switched_to(&mut beta_attach, &survivor);
}

#[tokio::test]
async fn kill_window_publishes_teardown_before_destroy_rehome_without_deadlock() {
    let handler = RequestHandler::new();
    let survivor = session_name("kill-window-order-survivor");
    let destroyed = session_name("kill-window-order-destroyed");
    new_session(&handler, &survivor).await;
    new_session(&handler, &destroyed).await;
    set_detach_on_destroy(&handler, &destroyed, "off").await;
    let (attach_tx, _attach_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(43_061, destroyed.clone(), attach_tx)
        .await;
    let mut lifecycle_events = handler.subscribe_lifecycle_events();

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        handler.handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(destroyed, 0),
            kill_all_others: false,
        })),
    )
    .await
    .expect("kill-window with destroy rehome must not wait on its own later ticket");
    assert!(matches!(response, Response::KillWindow(_)), "{response:?}");
    assert_eq!(
        drain_destroy_lifecycle_order(&mut lifecycle_events),
        vec![
            "window-unlinked",
            "session-closed",
            "client-session-changed",
        ]
    );
}

#[tokio::test]
async fn kill_window_all_others_rehomes_clients_from_destroyed_linked_alias() {
    let handler = RequestHandler::new();
    let survivor = session_name("kill-window-linked-survivor");
    let alias = session_name("kill-window-linked-alias");
    new_session(&handler, &survivor).await;
    let linked_window = new_window(&handler, &survivor).await;
    new_session(&handler, &alias).await;
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: linked_window,
            target: WindowTarget::with_window(alias.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(alias.clone(), 0),
            kill_all_others: false,
        }))
        .await;
    assert!(matches!(response, Response::KillWindow(_)), "{response:?}");
    set_detach_on_destroy(&handler, &alias, "off").await;

    let mut control_events = register_control_session(&handler, 43_067, alias.clone()).await;
    let _ = drain_control_events(&mut control_events);
    let (attach_tx, mut attach_events) = mpsc::unbounded_channel();
    handler
        .register_attach(43_068, alias.clone(), attach_tx)
        .await;

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(survivor.clone(), 0),
            kill_all_others: true,
        }))
        .await;
    assert!(matches!(response, Response::KillWindow(_)), "{response:?}");
    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&alias)
        .is_none());
    assert_control_switched_to(&drain_control_events(&mut control_events), &survivor);
    assert_attach_switched_to(&mut attach_events, &survivor);
}

#[tokio::test]
async fn move_window_publishes_prepared_order_before_destroy_rehome_without_deadlock() {
    let handler = RequestHandler::new();
    let destination = session_name("move-window-order-destination");
    let source = session_name("move-window-order-source");
    new_session(&handler, &destination).await;
    new_session(&handler, &source).await;
    set_detach_on_destroy(&handler, &source, "off").await;
    let (attach_tx, _attach_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(43_062, source.clone(), attach_tx)
        .await;
    let mut lifecycle_events = handler.subscribe_lifecycle_events();

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        handler.handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(source, 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(destination, 1)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        })),
    )
    .await
    .expect("move-window with destroy rehome must not wait on a later lifecycle ticket");
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");
    assert_eq!(
        drain_destroy_lifecycle_order(&mut lifecycle_events),
        vec![
            "window-linked",
            "window-unlinked",
            "session-closed",
            "client-session-changed",
        ]
    );
}

#[tokio::test]
async fn destroy_control_switch_queues_committed_session_change_after_target_disappears() {
    let handler = RequestHandler::new();
    let source = session_name("destroy-control-event-source");
    let target = session_name("destroy-control-event-target");
    new_session(&handler, &source).await;
    new_session(&handler, &target).await;
    let requester_pid = 43_041;
    let mut control_events =
        register_control_session(&handler, requester_pid, source.clone()).await;
    let _ = drain_control_events(&mut control_events);
    let (control_id, source_id, target_id) = {
        let active_control = handler.active_control.lock().await;
        let active = active_control
            .by_pid
            .get(&requester_pid)
            .expect("control client is registered");
        let control_id = active.id;
        let source_id = active.session_id.expect("control has a source session");
        drop(active_control);
        let state = handler.state.lock().await;
        let target_id = state
            .sessions
            .session(&target)
            .expect("target session exists")
            .id();
        (control_id, source_id, target_id)
    };
    let mut lifecycle_events = handler.subscribe_lifecycle_events();
    let pause = handler.install_control_switch_post_commit_pause(requester_pid);
    let switching_handler = handler.clone();
    let switch = tokio::spawn(async move {
        switching_handler
            .switch_control_session_after_destroy(requester_pid, control_id, source_id, target_id)
            .await
    });

    tokio::time::timeout(Duration::from_secs(5), pause.reached.notified())
        .await
        .expect("destroy switch reaches the post-commit pause");
    handler
        .state
        .lock()
        .await
        .sessions
        .remove_session(&target)
        .expect("committed target can disappear before event delivery");
    pause.release.notify_one();

    assert_eq!(
        switch.await.expect("destroy switch task joins"),
        Some(target.clone())
    );
    let queued = tokio::time::timeout(Duration::from_secs(5), lifecycle_events.recv())
        .await
        .expect("committed event is emitted")
        .expect("lifecycle receiver remains open");
    assert_eq!(queued.control_session_identity, Some(target_id));
    assert!(matches!(
        queued.event,
        LifecycleEvent::ClientSessionChanged {
            session_name,
            client_name: Some(client_name),
        } if session_name == target && client_name == requester_pid.to_string()
    ));
}

#[tokio::test]
async fn natural_last_pane_exit_rehomes_control_and_attach_after_session_teardown() {
    let handler = RequestHandler::new();
    let survivor = session_name("natural-rehome-survivor");
    let source = session_name("natural-rehome-source");
    new_session(&handler, &survivor).await;
    new_session(&handler, &source).await;
    handler.wait_for_initial_panes_for_test().await;
    set_detach_on_destroy(&handler, &source, "off").await;
    let mut control_events = register_control_session(&handler, 43_067, source.clone()).await;
    let _ = drain_control_events(&mut control_events);
    let (attach_tx, mut attach_events) = mpsc::unbounded_channel();
    handler
        .register_attach(43_068, source.clone(), attach_tx)
        .await;
    let target = PaneTarget::with_window(source.clone(), 0, 0);
    let pane_id = {
        let mut state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&source)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .expect("source pane exists")
            .id();
        state
            .mark_pane_dead_without_exit_details(&target)
            .expect("mark source pane naturally exited");
        pane_id
    };
    let mut lifecycle_events = handler.subscribe_lifecycle_events();

    tokio::time::timeout(
        Duration::from_secs(5),
        handler.handle_pane_exit_event(PaneExitEvent::eof_published(source.clone(), pane_id, None)),
    )
    .await
    .expect("natural last-pane exit rehomes clients without deadlock");

    assert_eq!(
        drain_destroy_lifecycle_order(&mut lifecycle_events),
        vec![
            "window-unlinked",
            "session-closed",
            "client-session-changed",
            "client-session-changed",
        ]
    );
    assert_control_switched_to(&drain_control_events(&mut control_events), &survivor);
    assert_attach_switched_to(&mut attach_events, &survivor);
}

#[tokio::test]
async fn natural_grouped_last_pane_exit_rehomes_each_client_after_family_teardown() {
    let handler = RequestHandler::new();
    let survivor = session_name("natural-group-rehome-survivor");
    let owner = session_name("natural-group-rehome-owner");
    new_session(&handler, &survivor).await;
    new_session(&handler, &owner).await;
    let peer = create_grouped_session(&handler, "natural-group-rehome-peer", &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    set_detach_on_destroy(&handler, &owner, "off").await;
    set_detach_on_destroy(&handler, &peer, "off").await;
    let mut owner_control = register_control_session(&handler, 43_069, owner.clone()).await;
    let mut peer_control = register_control_session(&handler, 43_070, peer.clone()).await;
    let _ = drain_control_events(&mut owner_control);
    let _ = drain_control_events(&mut peer_control);
    let (attach_tx, mut attach_events) = mpsc::unbounded_channel();
    handler
        .register_attach(43_071, owner.clone(), attach_tx)
        .await;
    let target = PaneTarget::with_window(owner.clone(), 0, 0);
    let pane_id = {
        let mut state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&owner)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .expect("grouped source pane exists")
            .id();
        state
            .mark_pane_dead_without_exit_details(&target)
            .expect("mark grouped pane naturally exited");
        pane_id
    };
    let mut lifecycle_events = handler.subscribe_lifecycle_events();

    tokio::time::timeout(
        Duration::from_secs(5),
        handler.handle_pane_exit_event(PaneExitEvent::eof_published(owner.clone(), pane_id, None)),
    )
    .await
    .expect("natural grouped last-pane exit rehomes clients without deadlock");

    let order = drain_destroy_lifecycle_order(&mut lifecycle_events);
    assert_teardown_precedes_control_changes(&order, 4, 3);
    assert_control_switched_to(&drain_control_events(&mut owner_control), &survivor);
    assert_control_switched_to(&drain_control_events(&mut peer_control), &survivor);
    assert_attach_switched_to(&mut attach_events, &survivor);
}

#[tokio::test]
async fn control_destroy_switch_honors_each_detach_on_destroy_policy() {
    for (case_index, policy, occupied, expected) in [
        (0_u32, "off", None, "z"),
        (1, "no-detached", Some("z"), "a"),
        (2, "previous", None, "a"),
        (3, "next", None, "z"),
    ] {
        let handler = RequestHandler::new();
        let alpha = session_name(&format!("a-{case_index}"));
        let middle = session_name(&format!("m-{case_index}"));
        let zulu = session_name(&format!("z-{case_index}"));
        for session_name in [&alpha, &zulu, &middle] {
            new_session(&handler, session_name).await;
        }
        set_detach_on_destroy(&handler, &middle, policy).await;
        let subject_pid = 43_000 + case_index * 2;
        let mut subject_events =
            register_control_session(&handler, subject_pid, middle.clone()).await;
        let _ = drain_control_events(&mut subject_events);
        let mut occupied_events = match occupied {
            Some("z") => {
                Some(register_control_session(&handler, subject_pid + 1, zulu.clone()).await)
            }
            Some(other) => panic!("unexpected occupied-session marker {other}"),
            None => None,
        };
        if let Some(events) = occupied_events.as_mut() {
            let _ = drain_control_events(events);
        }

        let response = dispatch_as(
            &handler,
            subject_pid,
            Request::KillSession(KillSessionRequest {
                target: middle,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }),
        )
        .await;
        assert!(matches!(response, Response::KillSession(_)), "{response:?}");

        let expected = match expected {
            "a" => &alpha,
            "z" => &zulu,
            other => panic!("unexpected target marker {other}"),
        };
        assert_control_switched_to(&drain_control_events(&mut subject_events), expected);
        let active_control = handler.active_control.lock().await;
        let active = active_control
            .by_pid
            .get(&subject_pid)
            .expect("destroy switch preserves control client");
        assert_eq!(active.session_name.as_ref(), Some(expected));
        assert!(!active.closing.load(std::sync::atomic::Ordering::SeqCst));
    }
}

#[tokio::test]
async fn no_detached_uses_one_destroy_snapshot_for_control_and_attach() {
    let handler = RequestHandler::new();
    let source = session_name("dual-destroy-source");
    let beta = session_name("dual-destroy-beta");
    let gamma = session_name("dual-destroy-gamma");
    for session_name in [&source, &beta, &gamma] {
        new_session(&handler, session_name).await;
    }
    set_detach_on_destroy(&handler, &source, "no-detached").await;

    let control_pid = 43_050;
    let mut control_events = register_control_session(&handler, control_pid, source.clone()).await;
    let _ = drain_control_events(&mut control_events);
    let attach_pid = 43_051;
    let (attach_tx, mut attach_events) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, source.clone(), attach_tx)
        .await;

    let response = dispatch_as(
        &handler,
        control_pid,
        Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(source, 0, 0),
            kill_all_except: false,
        }),
    )
    .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    assert_control_switched_to(&drain_control_events(&mut control_events), &gamma);
    let attach_target = std::iter::from_fn(|| attach_events.try_recv().ok()).find_map(|event| {
        if let AttachControl::Switch(target) = event {
            Some(target.into_target())
        } else {
            None
        }
    });
    assert_eq!(
        attach_target.as_ref().map(|target| &target.session_name),
        Some(&gamma),
        "control and interactive clients must use the same pre-destroy detached target"
    );
    let active_attach = handler.active_attach.lock().await;
    assert_eq!(
        active_attach
            .by_pid
            .get(&attach_pid)
            .map(|active| &active.session_name),
        Some(&gamma)
    );
}

#[tokio::test]
async fn pane_kill_entry_paths_rehome_control_before_session_closed() {
    for by_id in [false, true] {
        let handler = RequestHandler::new();
        let suffix = if by_id { "by-id" } else { "target" };
        let survivor = session_name(&format!("pane-destroy-survivor-{suffix}"));
        let destroyed = session_name(&format!("pane-destroy-source-{suffix}"));
        new_session(&handler, &survivor).await;
        new_session(&handler, &destroyed).await;
        set_detach_on_destroy(&handler, &destroyed, "off").await;
        let requester_pid = if by_id { 43_101 } else { 43_100 };
        let mut events = register_control_session(&handler, requester_pid, destroyed.clone()).await;
        let _ = drain_control_events(&mut events);
        let pane_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&destroyed)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .expect("initial pane exists")
            .id();
        let request = if by_id {
            Request::PaneKill(PaneKillRequest {
                target: PaneTargetRef::by_id(destroyed, pane_id),
                kill_all_except: false,
            })
        } else {
            Request::KillPane(KillPaneRequest {
                target: PaneTarget::with_window(destroyed, 0, 0),
                kill_all_except: false,
            })
        };

        let response = dispatch_as(&handler, requester_pid, request).await;
        assert!(matches!(response, Response::KillPane(_)), "{response:?}");
        assert_control_switched_to(&drain_control_events(&mut events), &survivor);
    }
}

#[tokio::test]
async fn pane_transfer_rehomes_control_before_source_session_closed() {
    let handler = RequestHandler::new();
    let destination = session_name("control-join-destination");
    let source = session_name("control-join-source");
    new_session(&handler, &destination).await;
    new_session(&handler, &source).await;
    set_detach_on_destroy(&handler, &source, "off").await;
    let requester_pid = 43_200;
    let mut events = register_control_session(&handler, requester_pid, source.clone()).await;
    let _ = drain_control_events(&mut events);

    let response = dispatch_as(
        &handler,
        requester_pid,
        Request::JoinPane(JoinPaneRequest {
            source: PaneTarget::with_window(source.clone(), 0, 0),
            target: PaneTarget::with_window(destination.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }),
    )
    .await;
    assert!(matches!(response, Response::JoinPane(_)), "{response:?}");
    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&source)
        .is_none());
    assert_control_switched_to(&drain_control_events(&mut events), &destination);
}

#[tokio::test]
async fn move_pane_rehomes_source_control_after_session_teardown() {
    let handler = RequestHandler::new();
    let destination = session_name("control-move-pane-destination");
    let source = session_name("control-move-pane-source");
    new_session(&handler, &destination).await;
    new_session(&handler, &source).await;
    set_detach_on_destroy(&handler, &source, "off").await;
    let requester_pid = 43_202;
    let mut events = register_control_session(&handler, requester_pid, source.clone()).await;
    let _ = drain_control_events(&mut events);
    let mut lifecycle_events = handler.subscribe_lifecycle_events();

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        dispatch_as(
            &handler,
            requester_pid,
            Request::MovePane(MovePaneRequest {
                source: PaneTarget::with_window(source.clone(), 0, 0),
                target: PaneTarget::with_window(destination.clone(), 0, 0),
                direction: SplitDirection::Vertical,
                detached: true,
                before: false,
                full_size: false,
                size: None,
            }),
        ),
    )
    .await
    .expect("move-pane source teardown and rehome must not deadlock");
    assert!(matches!(response, Response::MovePane(_)), "{response:?}");
    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&source)
        .is_none());
    assert_eq!(
        drain_destroy_lifecycle_order(&mut lifecycle_events),
        vec![
            "window-unlinked",
            "session-closed",
            "client-session-changed",
        ]
    );
    assert_control_switched_to(&drain_control_events(&mut events), &destination);
}

#[tokio::test]
async fn move_window_rehomes_control_before_source_session_closed() {
    let handler = RequestHandler::new();
    let destination = session_name("control-move-window-destination");
    let source = session_name("control-move-window-source");
    new_session(&handler, &destination).await;
    new_session(&handler, &source).await;
    set_detach_on_destroy(&handler, &source, "off").await;
    let requester_pid = 43_201;
    let mut events = register_control_session(&handler, requester_pid, source.clone()).await;
    let _ = drain_control_events(&mut events);

    let response = dispatch_as(
        &handler,
        requester_pid,
        Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(source.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(destination.clone(), 1)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }),
    )
    .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");
    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&source)
        .is_none());
    assert_control_switched_to(&drain_control_events(&mut events), &destination);
}

#[tokio::test]
async fn kill_group_rehomes_controls_from_every_destroyed_alias() {
    let handler = RequestHandler::new();
    let survivor = session_name("control-group-survivor");
    let owner = session_name("control-group-owner");
    new_session(&handler, &survivor).await;
    new_session(&handler, &owner).await;
    let peer = create_grouped_session(&handler, "control-group-peer", &owner).await;
    set_detach_on_destroy(&handler, &owner, "off").await;
    set_detach_on_destroy(&handler, &peer, "off").await;
    let mut owner_events = register_control_session(&handler, 43_300, owner.clone()).await;
    let mut peer_events = register_control_session(&handler, 43_301, peer.clone()).await;
    let _ = drain_control_events(&mut owner_events);
    let _ = drain_control_events(&mut peer_events);

    let response = dispatch_as(
        &handler,
        43_300,
        Request::KillSession(KillSessionRequest {
            target: owner.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: true,
        }),
    )
    .await;
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
    let state = handler.state.lock().await;
    assert!(state.sessions.session(&owner).is_none());
    assert!(state.sessions.session(&peer).is_none());
    drop(state);
    assert_control_switched_to(&drain_control_events(&mut owner_events), &survivor);
    assert_control_switched_to(&drain_control_events(&mut peer_events), &survivor);
}

#[tokio::test]
async fn control_client_exits_when_its_target_session_is_killed() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = 4242;
    new_session(&handler, &alpha).await;
    let mut rx = register_control_session(&handler, requester_pid, alpha.clone()).await;
    let _ = drain_control_events(&mut rx);

    let response = dispatch_as(
        &handler,
        requester_pid,
        Request::KillSession(KillSessionRequest {
            target: alpha,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }),
    )
    .await;
    assert!(matches!(response, Response::KillSession(_)));

    assert_has_exit(&drain_control_events(&mut rx));
}

#[tokio::test]
async fn hook_execution_kill_session_still_exits_control_without_requeueing_hooks() {
    let handler = RequestHandler::new();
    let alpha = session_name("hook-control-close-alpha");
    let requester_pid = 42_457;
    new_session(&handler, &alpha).await;
    let mut rx = register_control_session(&handler, requester_pid, alpha.clone()).await;
    let _ = drain_control_events(&mut rx);
    let mut lifecycle_events = handler.subscribe_lifecycle_events();

    let outcome = crate::hook_runtime::with_hook_execution(
        crate::hook_runtime::HookExecutionContext::lifecycle(rmux_proto::HookName::SessionClosed),
        Vec::new(),
        async {
            handler
                .dispatch(
                    requester_pid,
                    Request::KillSession(KillSessionRequest {
                        target: alpha,
                        kill_all_except_target: false,
                        clear_alerts: false,
                        kill_group: false,
                    }),
                )
                .await
        },
    )
    .await;

    assert!(matches!(outcome.response, Response::KillSession(_)));
    assert!(matches!(
        lifecycle_events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_has_exit(&drain_control_events(&mut rx));
}

#[tokio::test]
async fn closing_control_queue_rejects_follow_on_mutation_after_session_name_reuse() {
    let handler = RequestHandler::new();
    let alpha = session_name("closing-control-queue-alpha");
    let requester_pid = 42_458;
    let wait_channel = "closing-control-queue-wait";
    new_session(&handler, &alpha).await;
    let original_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("original session exists")
        .id();
    let mut rx = register_control_session(&handler, requester_pid, alpha.clone()).await;
    let _ = drain_control_events(&mut rx);
    let command = format!("wait-for {wait_channel}; set-environment CONTROL_AFTER_CLOSE mutated");
    let commands = CommandParser::new()
        .parse(&command)
        .expect("control commands parse");
    let queued_handler = handler.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_control_commands(requester_pid, commands)
            .await
    });
    wait_until_wait_for_count(&handler, wait_channel, 1).await;

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    assert_has_exit(&drain_control_events(&mut rx));
    new_session(&handler, &alpha).await;
    let replacement_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("replacement session exists")
        .id();
    assert_ne!(replacement_session_id, original_session_id);

    let signaled = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: wait_channel.to_owned(),
            mode: WaitForMode::Signal,
        }))
        .await;
    assert!(matches!(signaled, Response::WaitFor(_)), "{signaled:?}");
    let result = queued.await.expect("control queue task joins");
    assert!(
        result.error.is_some(),
        "the closing control queue must reject its follow-on command"
    );
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .environment
            .session_value(&alpha, "CONTROL_AFTER_CLOSE"),
        None,
        "the follow-on command must not mutate the replacement session"
    );
}

#[tokio::test]
async fn nonclosing_control_queue_revalidates_session_id_before_implicit_mutation() {
    let handler = RequestHandler::new();
    let alpha = session_name("stale-control-candidate-alpha");
    let requester_pid = 42_459;
    new_session(&handler, &alpha).await;
    let _rx = register_control_session(&handler, requester_pid, alpha.clone()).await;
    let original_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("original session exists")
        .id();
    let replacement_session_id = {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .remove_session(&alpha)
            .expect("original session removal succeeds");
        state
            .sessions
            .create_session(alpha.clone(), TerminalSize { cols: 80, rows: 24 })
            .expect("replacement session creation succeeds");
        state
            .sessions
            .session(&alpha)
            .expect("replacement session exists")
            .id()
    };
    assert_ne!(replacement_session_id, original_session_id);
    assert_eq!(handler.current_session_candidate(requester_pid).await, None);

    let commands = CommandParser::new()
        .parse("set-environment CONTROL_STALE_ID mutated")
        .expect("control command parses");
    let result = handler
        .execute_control_commands(requester_pid, commands)
        .await;

    assert!(matches!(
        result.error,
        Some(rmux_proto::RmuxError::SessionNotFound(_))
    ));
    let state = handler.state.lock().await;
    assert_eq!(
        state.environment.session_value(&alpha, "CONTROL_STALE_ID"),
        None
    );
}

async fn wait_until_wait_for_count(handler: &RequestHandler, channel: &str, expected: usize) {
    for _ in 0..200 {
        if handler.wait_for_counts(channel).0 == expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(handler.wait_for_counts(channel).0, expected);
}

#[tokio::test]
async fn control_client_exits_when_its_last_window_destroys_the_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = 4243;
    new_session(&handler, &alpha).await;
    let mut rx = register_control_session(&handler, requester_pid, alpha.clone()).await;
    let _ = drain_control_events(&mut rx);

    let response = dispatch_as(
        &handler,
        requester_pid,
        Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            kill_all_others: false,
        }),
    )
    .await;
    assert!(matches!(response, Response::KillWindow(_)));

    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .is_none());

    assert_has_exit(&drain_control_events(&mut rx));
}

#[tokio::test]
async fn control_client_stays_open_when_another_session_is_killed() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let requester_pid = 4244;
    new_session(&handler, &alpha).await;
    new_session(&handler, &beta).await;
    let mut rx = register_control_session(&handler, requester_pid, alpha).await;
    let _ = drain_control_events(&mut rx);

    let response = dispatch_as(
        &handler,
        requester_pid,
        Request::KillSession(KillSessionRequest {
            target: beta,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }),
    )
    .await;
    assert!(matches!(response, Response::KillSession(_)));

    assert_has_no_exit(&drain_control_events(&mut rx));
}

#[tokio::test]
async fn control_client_stays_open_when_non_last_window_is_killed() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = 4245;
    new_session(&handler, &alpha).await;
    let target = new_window(&handler, &alpha).await;
    let mut rx = register_control_session(&handler, requester_pid, alpha).await;
    let _ = drain_control_events(&mut rx);

    let response = dispatch_as(
        &handler,
        requester_pid,
        Request::KillWindow(KillWindowRequest {
            target,
            kill_all_others: false,
        }),
    )
    .await;
    assert!(matches!(response, Response::KillWindow(_)));

    let events = drain_control_events(&mut rx);
    assert_has_no_exit(&events);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ControlServerEvent::Refresh)),
        "window deletion should refresh an attached control client, got {events:?}"
    );
}

#[tokio::test]
async fn stale_control_session_identity_cannot_bind_to_recreated_name() {
    let handler = RequestHandler::new();
    let alpha = session_name("control-set-identity-alpha");
    let requester_pid = 42_451;
    new_session(&handler, &alpha).await;
    let old_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("old alpha exists")
        .id();
    let (event_tx, _event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    new_session(&handler, &alpha).await;

    assert_eq!(
        handler
            .set_control_session_identity(requester_pid, alpha.clone(), old_session_id)
            .await,
        Err(rmux_proto::RmuxError::SessionNotFound(alpha.to_string()))
    );
    let active_control = handler.active_control.lock().await;
    let active = active_control
        .by_pid
        .get(&requester_pid)
        .expect("control survives failed assignment");
    assert_eq!(
        (active.session_name.as_ref(), active.session_id),
        (None, None)
    );
}

#[tokio::test]
async fn stale_session_closed_cleanup_preserves_control_for_recreated_identity() {
    let handler = RequestHandler::new();
    let alpha = session_name("control-close-identity-alpha");
    new_session(&handler, &alpha).await;
    let old_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("old alpha exists")
        .id();
    let mut old_rx = register_control_session(&handler, 42_452, alpha.clone()).await;
    let _ = drain_control_events(&mut old_rx);

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    new_session(&handler, &alpha).await;
    let new_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("new alpha exists")
        .id();
    assert_ne!(new_session_id, old_session_id);
    let mut new_rx = register_control_session(&handler, 42_453, alpha.clone()).await;
    let _ = drain_control_events(&mut new_rx);

    handler
        .refresh_control_sessions_for_event(&LifecycleEvent::SessionClosed {
            session_name: alpha.clone(),
            session_id: Some(old_session_id.as_u32()),
        })
        .await;

    assert_has_exit(&drain_control_events(&mut old_rx));
    assert_has_no_exit(&drain_control_events(&mut new_rx));
    let active_control = handler.active_control.lock().await;
    assert_eq!(
        active_control
            .by_pid
            .get(&42_453)
            .and_then(|active| active.session_id),
        Some(new_session_id)
    );
}

#[tokio::test]
async fn late_control_rename_preserves_control_for_recreated_source_name() {
    let handler = RequestHandler::new();
    let alpha = session_name("control-rename-identity-alpha");
    let beta = session_name("control-rename-identity-beta");
    new_session(&handler, &alpha).await;
    let old_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("old alpha exists")
        .id();
    let mut old_rx = register_control_session(&handler, 42_454, alpha.clone()).await;
    let _ = drain_control_events(&mut old_rx);

    {
        let mut state = handler.state.lock().await;
        state
            .rename_session(&alpha, &beta)
            .expect("model rename succeeds");
    }
    new_session(&handler, &alpha).await;
    let new_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("new alpha exists")
        .id();
    let mut new_rx = register_control_session(&handler, 42_455, alpha.clone()).await;
    let _ = drain_control_events(&mut new_rx);

    handler
        .rename_control_session(&alpha, old_session_id, &beta)
        .await;

    assert!(drain_control_events(&mut old_rx).iter().any(|event| {
        matches!(event, ControlServerEvent::SessionChanged(Some(session)) if session == &beta)
    }));
    assert!(drain_control_events(&mut new_rx).is_empty());
    let active_control = handler.active_control.lock().await;
    let old_control = active_control
        .by_pid
        .get(&42_454)
        .expect("old control exists");
    assert_eq!(
        (old_control.session_name.as_ref(), old_control.session_id),
        (Some(&beta), Some(old_session_id))
    );
    let new_control = active_control
        .by_pid
        .get(&42_455)
        .expect("new control exists");
    assert_eq!(
        (new_control.session_name.as_ref(), new_control.session_id),
        (Some(&alpha), Some(new_session_id))
    );
}

#[tokio::test]
async fn failed_session_exit_finishes_stale_identity_without_destroying_recreated_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("control-finish-identity-alpha");
    let requester_pid = 42_456;
    new_session(&handler, &alpha).await;
    let old_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("old alpha exists")
        .id();
    let (event_tx, mut event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session_identity(requester_pid, alpha.clone(), old_session_id)
        .await
        .expect("old control attaches");
    let _ = drain_control_events(&mut event_rx);
    drop(event_rx);

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    new_session(&handler, &alpha).await;
    let new_session_id = {
        let mut state = handler.state.lock().await;
        let session_id = state
            .sessions
            .session(&alpha)
            .expect("new alpha exists")
            .id();
        state
            .options
            .set(
                ScopeSelector::Session(alpha.clone()),
                OptionName::DestroyUnattached,
                "on".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("destroy-unattached option is valid");
        session_id
    };
    assert_ne!(new_session_id, old_session_id);
    {
        let active_control = handler.active_control.lock().await;
        let active = active_control
            .by_pid
            .get(&requester_pid)
            .expect("failed session Exit keeps the stale control until transport finish");
        assert_eq!(active.id, control_id);
        assert_eq!(active.session_name.as_ref(), Some(&alpha));
        assert_eq!(active.session_id, Some(old_session_id));
        assert!(active.closing.load(std::sync::atomic::Ordering::SeqCst));
    }
    let mut lifecycle = handler.subscribe_lifecycle_events();

    handler.finish_control(requester_pid, control_id).await;

    let detached = tokio::time::timeout(Duration::from_secs(1), lifecycle.recv())
        .await
        .expect("transport finish publishes the stale client-detached identity")
        .expect("lifecycle channel remains open");
    assert_eq!(detached.control_session_identity, Some(old_session_id));
    assert!(matches!(
        detached.event,
        LifecycleEvent::ClientDetached {
            session_name,
            client_name: Some(client_name),
        } if session_name == alpha && client_name == requester_pid.to_string()
    ));
    let state = handler.state.lock().await;
    assert_eq!(
        state.sessions.session(&alpha).map(rmux_core::Session::id),
        Some(new_session_id)
    );
}
