use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::RequestHandler;
use crate::control::{ControlModeUpgrade, ControlServerEvent};
use rmux_proto::{
    ControlMode, DeleteBufferRequest, DetachClientRequest, DisplayMessageRequest, HookLifecycle,
    HookName, KillSessionRequest, KillWindowRequest, NewSessionRequest, NewWindowRequest,
    RenameSessionRequest, RenameWindowRequest, Request, Response, ScopeSelector,
    SelectWindowRequest, SessionName, SetBufferRequest, SetHookRequest, ShowOptionsRequest,
    SwitchClientRequest, Target, TerminalSize, WindowTarget,
};
use tokio::sync::mpsc;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn new_session(handler: &RequestHandler, session_name: &SessionName) {
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
}

async fn new_window(
    handler: &RequestHandler,
    session_name: &SessionName,
    name: Option<&str>,
) -> WindowTarget {
    let response = handler
        .handle(Request::NewWindow(NewWindowRequest {
            target: session_name.clone(),
            name: name.map(str::to_owned),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            target_window_index: None,
            insert_at_target: false,
        }))
        .await;

    let Response::NewWindow(response) = response else {
        panic!("expected new-window response");
    };

    response.target
}

async fn register_control_client(
    handler: &RequestHandler,
    requester_pid: u32,
    session_name: Option<SessionName>,
) -> mpsc::UnboundedReceiver<ControlServerEvent> {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let _control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default()
                    .with_client_terminal(&rmux_proto::ClientTerminalContext {
                        terminal_features: Vec::new(),
                        utf8: true,
                    }),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    if let Some(session_name) = session_name {
        handler
            .set_control_session(requester_pid, Some(session_name))
            .await
            .expect("control session set succeeds");
    }
    event_rx
}

fn drain_control_notifications(
    rx: &mut mpsc::UnboundedReceiver<ControlServerEvent>,
) -> Vec<String> {
    let mut lines = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(ControlServerEvent::Notification(line)) => lines.push(line),
            Ok(ControlServerEvent::SessionChanged(_) | ControlServerEvent::Refresh) => {}
            Ok(ControlServerEvent::Exit(reason)) => {
                panic!("unexpected control exit: {reason:?}");
            }
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                break;
            }
        }
    }
    lines
}

fn collect_control_events(
    rx: &mut mpsc::UnboundedReceiver<ControlServerEvent>,
) -> Vec<ControlServerEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

async fn session_id(handler: &RequestHandler, session_name: &SessionName) -> u32 {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(session_name)
        .expect("session exists")
        .id()
        .as_u32()
}

async fn window_id(handler: &RequestHandler, target: &WindowTarget) -> u32 {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .expect("window exists")
        .id()
        .as_u32()
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

#[tokio::test]
async fn control_switch_client_sends_self_and_other_session_notifications() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    new_session(&handler, &alpha).await;
    new_session(&handler, &beta).await;

    let mut self_rx = register_control_client(&handler, 101, Some(alpha.clone())).await;
    let mut other_rx = register_control_client(&handler, 202, Some(alpha.clone())).await;
    let mut detached_rx = register_control_client(&handler, 303, None).await;
    let _ = drain_control_notifications(&mut self_rx);
    let _ = drain_control_notifications(&mut other_rx);
    let _ = drain_control_notifications(&mut detached_rx);

    let response = dispatch_as(
        &handler,
        101,
        Request::SwitchClient(SwitchClientRequest {
            target: beta.clone(),
        }),
    )
    .await;

    assert_eq!(
        response,
        Response::SwitchClient(rmux_proto::SwitchClientResponse {
            session_name: beta.clone(),
        })
    );

    let beta_id = session_id(&handler, &beta).await;
    assert_eq!(
        drain_control_notifications(&mut self_rx),
        vec![format!("%session-changed ${beta_id} {beta}")]
    );
    assert_eq!(
        drain_control_notifications(&mut other_rx),
        vec![format!("%client-session-changed 101 ${beta_id} {beta}")]
    );
    assert!(drain_control_notifications(&mut detached_rx).is_empty());
}

#[tokio::test]
async fn control_window_notifications_follow_each_clients_session_visibility() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    new_session(&handler, &alpha).await;
    new_session(&handler, &beta).await;

    let mut alpha_rx = register_control_client(&handler, 410, Some(alpha.clone())).await;
    let mut beta_rx = register_control_client(&handler, 420, Some(beta.clone())).await;
    let _ = drain_control_notifications(&mut alpha_rx);
    let _ = drain_control_notifications(&mut beta_rx);

    let target = new_window(&handler, &alpha, Some("logs")).await;
    let window_id = window_id(&handler, &target).await;

    assert_eq!(
        drain_control_notifications(&mut alpha_rx),
        vec![format!("%window-add @{window_id}")]
    );
    assert_eq!(
        drain_control_notifications(&mut beta_rx),
        vec![format!("%unlinked-window-add @{window_id}")]
    );

    let renamed = handler
        .handle(Request::RenameWindow(RenameWindowRequest {
            target: target.clone(),
            name: "build".to_owned(),
        }))
        .await;
    assert!(matches!(renamed, Response::RenameWindow(_)));

    assert_eq!(
        drain_control_notifications(&mut alpha_rx),
        vec![format!("%window-renamed @{window_id} build")]
    );
    assert_eq!(
        drain_control_notifications(&mut beta_rx),
        vec![format!("%unlinked-window-renamed @{window_id} build")]
    );
}

#[tokio::test]
async fn window_close_notifications_follow_each_clients_session_visibility() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    new_session(&handler, &alpha).await;
    new_session(&handler, &beta).await;

    let mut alpha_rx = register_control_client(&handler, 430, Some(alpha.clone())).await;
    let mut beta_rx = register_control_client(&handler, 440, Some(beta)).await;
    let _ = drain_control_notifications(&mut alpha_rx);
    let _ = drain_control_notifications(&mut beta_rx);

    let target = new_window(&handler, &alpha, Some("logs")).await;
    let window_id = window_id(&handler, &target).await;
    let _ = drain_control_notifications(&mut alpha_rx);
    let _ = drain_control_notifications(&mut beta_rx);

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target,
            kill_all_others: false,
        }))
        .await;
    assert!(matches!(response, Response::KillWindow(_)));

    assert_eq!(
        drain_control_notifications(&mut alpha_rx),
        vec![format!("%unlinked-window-close @{window_id}")]
    );
    assert_eq!(
        drain_control_notifications(&mut beta_rx),
        vec![format!("%unlinked-window-close @{window_id}")]
    );
}

#[tokio::test]
async fn killing_the_only_window_is_rejected_without_notifications() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_session(&handler, &alpha).await;

    let mut control_rx = register_control_client(&handler, 450, Some(alpha.clone())).await;
    let _ = drain_control_notifications(&mut control_rx);

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(alpha, 0),
            kill_all_others: false,
        }))
        .await;
    assert!(matches!(response, Response::Error(_)));
    assert!(drain_control_notifications(&mut control_rx).is_empty());
}

#[tokio::test]
async fn paste_buffer_notifications_use_the_buffer_name() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_session(&handler, &alpha).await;

    let mut control_rx = register_control_client(&handler, 510, Some(alpha)).await;
    let _ = drain_control_notifications(&mut control_rx);

    let set_response = handler
        .handle(Request::SetBuffer(SetBufferRequest {
            name: Some("named".to_owned()),
            content: b"hello".to_vec(),
            append: false,
            set_clipboard: false,
            new_name: None,
        }))
        .await;
    assert!(matches!(set_response, Response::SetBuffer(_)));
    assert_eq!(
        drain_control_notifications(&mut control_rx),
        vec!["%paste-buffer-changed named".to_owned()]
    );

    let delete_response = handler
        .handle(Request::DeleteBuffer(DeleteBufferRequest {
            name: Some("named".to_owned()),
        }))
        .await;
    assert!(matches!(delete_response, Response::DeleteBuffer(_)));
    assert_eq!(
        drain_control_notifications(&mut control_rx),
        vec!["%paste-buffer-deleted named".to_owned()]
    );
}

#[tokio::test]
async fn sessions_changed_notifications_reach_control_clients_with_and_without_sessions() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    new_session(&handler, &alpha).await;

    let mut attached_rx = register_control_client(&handler, 520, Some(alpha.clone())).await;
    let mut detached_rx = register_control_client(&handler, 530, None).await;
    let _ = drain_control_notifications(&mut attached_rx);
    let _ = drain_control_notifications(&mut detached_rx);

    new_session(&handler, &beta).await;
    assert_eq!(
        drain_control_notifications(&mut attached_rx),
        vec!["%sessions-changed".to_owned()]
    );
    assert_eq!(
        drain_control_notifications(&mut detached_rx),
        vec!["%sessions-changed".to_owned()]
    );

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: beta,
            kill_all_except_target: false,
            clear_alerts: false,
        }))
        .await;
    assert!(matches!(response, Response::KillSession(_)));
    assert_eq!(
        drain_control_notifications(&mut attached_rx),
        vec!["%sessions-changed".to_owned()]
    );
    assert_eq!(
        drain_control_notifications(&mut detached_rx),
        vec!["%sessions-changed".to_owned()]
    );
}

#[tokio::test]
async fn session_renamed_notifications_include_session_id_and_new_name() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    new_session(&handler, &alpha).await;

    let mut attached_rx = register_control_client(&handler, 540, Some(alpha.clone())).await;
    let mut detached_rx = register_control_client(&handler, 550, None).await;
    let _ = drain_control_notifications(&mut attached_rx);
    let _ = drain_control_notifications(&mut detached_rx);

    let alpha_id = session_id(&handler, &alpha).await;
    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: alpha,
            new_name: beta.clone(),
        }))
        .await;
    assert!(matches!(response, Response::RenameSession(_)));

    let expected = vec![format!("%session-renamed ${alpha_id} {beta}")];
    assert_eq!(drain_control_notifications(&mut attached_rx), expected);
    assert_eq!(
        drain_control_notifications(&mut detached_rx),
        vec![format!("%session-renamed ${alpha_id} {beta}")]
    );
}

#[tokio::test]
async fn session_window_changed_notifications_are_broadcast_to_all_control_clients() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_session(&handler, &alpha).await;

    let target = new_window(&handler, &alpha, Some("logs")).await;
    let window_id = window_id(&handler, &target).await;
    let session_id = session_id(&handler, &alpha).await;

    let mut attached_rx = register_control_client(&handler, 560, Some(alpha.clone())).await;
    let mut detached_rx = register_control_client(&handler, 570, None).await;
    let _ = drain_control_notifications(&mut attached_rx);
    let _ = drain_control_notifications(&mut detached_rx);

    let response = handler
        .handle(Request::SelectWindow(SelectWindowRequest { target }))
        .await;
    assert!(matches!(response, Response::SelectWindow(_)));

    let expected = vec![format!(
        "%session-window-changed ${session_id} @{window_id}"
    )];
    assert_eq!(drain_control_notifications(&mut attached_rx), expected);
    assert_eq!(
        drain_control_notifications(&mut detached_rx),
        vec![format!(
            "%session-window-changed ${session_id} @{window_id}"
        )]
    );
}

#[tokio::test]
async fn detached_control_clients_skip_session_scoped_window_notifications() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_session(&handler, &alpha).await;

    let mut attached_rx = register_control_client(&handler, 580, Some(alpha.clone())).await;
    let mut detached_rx = register_control_client(&handler, 590, None).await;
    let _ = drain_control_notifications(&mut attached_rx);
    let _ = drain_control_notifications(&mut detached_rx);

    let target = new_window(&handler, &alpha, Some("logs")).await;
    let window_id = window_id(&handler, &target).await;

    assert_eq!(
        drain_control_notifications(&mut attached_rx),
        vec![format!("%window-add @{window_id}")]
    );
    assert!(drain_control_notifications(&mut detached_rx).is_empty());
}

#[tokio::test]
async fn display_message_for_control_client_uses_message_notification() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_session(&handler, &alpha).await;

    let mut control_rx = register_control_client(&handler, 610, Some(alpha.clone())).await;
    let _ = drain_control_notifications(&mut control_rx);

    let response = dispatch_as(
        &handler,
        610,
        Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Session(alpha)),
            print: false,
            message: Some("hello\t#{session_name}".to_owned()),
        }),
    )
    .await;

    assert_eq!(
        response,
        Response::DisplayMessage(rmux_proto::DisplayMessageResponse::no_output())
    );
    assert_eq!(
        drain_control_notifications(&mut control_rx),
        vec!["%message hello\\talpha".to_owned()]
    );
}

#[tokio::test]
async fn startup_config_errors_are_queued_as_percent_config_error_notifications() {
    let handler = RequestHandler::new();
    handler
        .startup_config_errors
        .lock()
        .await
        .push(rmux_proto::RmuxError::Server(
            "first startup error\nsecond startup error".to_owned(),
        ));

    let mut control_rx = register_control_client(&handler, 710, None).await;

    assert_eq!(
        drain_control_notifications(&mut control_rx),
        vec![
            "%config-error first startup error".to_owned(),
            "%config-error second startup error".to_owned(),
        ]
    );
}

#[tokio::test]
async fn control_detach_notifies_the_same_control_client_before_exit() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_session(&handler, &alpha).await;

    let mut self_rx = register_control_client(&handler, 810, Some(alpha.clone())).await;
    let mut other_rx = register_control_client(&handler, 820, Some(alpha)).await;
    let _ = drain_control_notifications(&mut self_rx);
    let _ = drain_control_notifications(&mut other_rx);

    let response = dispatch_as(&handler, 810, Request::DetachClient(DetachClientRequest)).await;
    assert_eq!(
        response,
        Response::DetachClient(rmux_proto::DetachClientResponse)
    );

    let self_events = collect_control_events(&mut self_rx);
    assert!(self_events.iter().any(|event| matches!(
        event,
        ControlServerEvent::Notification(line) if line == "%client-detached 810"
    )));
    assert!(self_events
        .iter()
        .any(|event| matches!(event, ControlServerEvent::Exit(None))));
    assert_eq!(
        drain_control_notifications(&mut other_rx),
        vec!["%client-detached 810".to_owned()]
    );
}

#[tokio::test]
async fn hook_commands_do_not_emit_nested_control_notifications() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_session(&handler, &alpha).await;

    let mut control_rx = register_control_client(&handler, 910, Some(alpha)).await;
    let _ = drain_control_notifications(&mut control_rx);

    let set_hook = handler
        .handle(Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Global,
            hook: HookName::AfterShowOptions,
            command: "new-session -d -s beta".to_owned(),
            lifecycle: HookLifecycle::OneShot,
        }))
        .await;
    assert!(matches!(set_hook, Response::SetHook(_)));

    let response = handler
        .handle(Request::ShowOptions(ShowOptionsRequest {
            scope: rmux_proto::OptionScopeSelector::SessionGlobal,
            name: None,
            value_only: false,
        }))
        .await;
    assert!(matches!(response, Response::ShowOptions(_)));
    assert!(drain_control_notifications(&mut control_rx).is_empty());

    let has_beta = handler
        .handle(Request::HasSession(rmux_proto::HasSessionRequest {
            target: session_name("beta"),
        }))
        .await;
    assert_eq!(
        has_beta,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );
}
