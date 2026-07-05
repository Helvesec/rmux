use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::RequestHandler;
use crate::control::{ControlModeUpgrade, ControlServerEvent};
use rmux_proto::{
    ClientTerminalContext, ControlMode, KillSessionRequest, KillWindowRequest, NewSessionRequest,
    NewWindowRequest, Request, Response, SessionName, TerminalSize, WindowTarget,
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
) -> mpsc::UnboundedReceiver<ControlServerEvent> {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let _control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
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

fn drain_control_events(
    rx: &mut mpsc::UnboundedReceiver<ControlServerEvent>,
) -> Vec<ControlServerEvent> {
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
