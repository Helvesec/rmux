use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::RequestHandler;
use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};
use rmux_core::{command_parser::CommandParser, LifecycleEvent};
use rmux_proto::{
    ClientTerminalContext, ControlMode, KillSessionRequest, KillWindowRequest, NewSessionRequest,
    NewWindowRequest, OptionName, RenameSessionRequest, Request, Response, ScopeSelector,
    SessionName, SetOptionMode, TerminalSize, WaitForMode, WaitForRequest, WindowTarget,
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

    let outcome = crate::hook_runtime::with_hook_execution(Vec::new(), async {
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
    })
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
async fn control_client_stays_open_when_last_window_kill_is_rejected() {
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
            target: WindowTarget::with_window(alpha, 0),
            kill_all_others: false,
        }),
    )
    .await;
    assert!(matches!(response, Response::Error(_)));

    assert_has_no_exit(&drain_control_events(&mut rx));
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
async fn finishing_stale_control_does_not_destroy_recreated_unattached_session() {
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
    let (event_tx, _event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
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

    handler.finish_control(requester_pid, control_id).await;

    let state = handler.state.lock().await;
    assert_eq!(
        state.sessions.session(&alpha).map(rmux_core::Session::id),
        Some(new_session_id)
    );
}
