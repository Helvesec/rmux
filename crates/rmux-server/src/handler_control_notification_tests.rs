use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::RequestHandler;
use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};
use rmux_core::LifecycleEvent;
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
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: name.map(str::to_owned),
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

async fn register_control_client(
    handler: &RequestHandler,
    requester_pid: u32,
    session_name: Option<SessionName>,
) -> mpsc::Receiver<ControlServerEvent> {
    let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let _control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
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

fn drain_control_notifications(rx: &mut mpsc::Receiver<ControlServerEvent>) -> Vec<String> {
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

fn collect_control_events(rx: &mut mpsc::Receiver<ControlServerEvent>) -> Vec<ControlServerEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn full_control_server_event_queue_closes_and_removes_client() {
    let handler = RequestHandler::new();
    let requester_pid = 4242;
    let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let _control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            event_tx,
            Arc::clone(&closing),
        )
        .await;

    for index in 0..CONTROL_SERVER_EVENT_CAPACITY {
        handler
            .send_control_notification_to(requester_pid, format!("%message queued-{index}"))
            .await;
    }

    assert_eq!(event_rx.len(), CONTROL_SERVER_EVENT_CAPACITY);
    assert_eq!(event_rx.max_capacity(), CONTROL_SERVER_EVENT_CAPACITY);
    assert!(!closing.load(Ordering::SeqCst));
    assert!(handler.is_control_client(requester_pid).await);

    handler
        .send_control_notification_to(requester_pid, "%message overflow".to_owned())
        .await;

    assert_eq!(event_rx.len(), CONTROL_SERVER_EVENT_CAPACITY);
    assert!(event_rx.is_closed());
    assert!(closing.load(Ordering::SeqCst));
    assert!(!handler.is_control_client(requester_pid).await);
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

async fn prepared_client_session_changed(
    handler: &RequestHandler,
    session_name: SessionName,
    session_id: rmux_proto::SessionId,
    client_name: &str,
) -> super::QueuedLifecycleEvent {
    let mut events = handler.subscribe_lifecycle_events();
    handler
        .emit_for_session_identity(
            LifecycleEvent::ClientSessionChanged {
                session_name: session_name.clone(),
                client_name: Some(client_name.to_owned()),
            },
            &session_name,
            session_id,
        )
        .await;
    events
        .recv()
        .await
        .expect("exact client-session-changed event queued")
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

    let renamed = handler
        .handle(Request::RenameWindow(RenameWindowRequest {
            target: target.clone(),
            name: "bad\n%output %1 injected".to_owned(),
        }))
        .await;
    assert!(matches!(renamed, Response::RenameWindow(_)));

    assert_eq!(
        drain_control_notifications(&mut alpha_rx),
        vec![format!(
            "%window-renamed @{window_id} bad\\012%output %1 injected"
        )]
    );
    assert_eq!(
        drain_control_notifications(&mut beta_rx),
        vec![format!(
            "%unlinked-window-renamed @{window_id} bad\\012%output %1 injected"
        )]
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
        .handle(Request::SetBuffer(Box::new(SetBufferRequest {
            name: Some("named".to_owned()),
            content: b"hello".to_vec(),
            append: false,
            set_clipboard: false,
            new_name: None,
            target_client: None,
        })))
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

    let set_response = handler
        .handle(Request::SetBuffer(Box::new(SetBufferRequest {
            name: Some("bad\nname".to_owned()),
            content: b"hello".to_vec(),
            append: false,
            set_clipboard: false,
            new_name: None,
            target_client: None,
        })))
        .await;
    assert!(matches!(set_response, Response::SetBuffer(_)));
    assert_eq!(
        drain_control_notifications(&mut control_rx),
        vec!["%paste-buffer-changed bad\\012name".to_owned()]
    );

    let delete_response = handler
        .handle(Request::DeleteBuffer(DeleteBufferRequest {
            name: Some("bad\nname".to_owned()),
        }))
        .await;
    assert!(matches!(delete_response, Response::DeleteBuffer(_)));
    assert_eq!(
        drain_control_notifications(&mut control_rx),
        vec!["%paste-buffer-deleted bad\\012name".to_owned()]
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

    let beta_window_id = window_id(&handler, &WindowTarget::new(beta.clone())).await;
    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: beta,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(response, Response::KillSession(_)));
    assert_eq!(
        drain_control_notifications(&mut attached_rx),
        vec![
            "%sessions-changed".to_owned(),
            format!("%unlinked-window-close @{beta_window_id}")
        ]
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
            empty_target_context: false,
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
async fn startup_config_errors_do_not_block_first_regular_command() {
    let handler = RequestHandler::new();
    handler
        .startup_config_errors
        .lock()
        .await
        .push(rmux_proto::RmuxError::Server(
            "startup config failed".to_owned(),
        ));

    let response = dispatch_as(
        &handler,
        711,
        Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await;

    assert!(matches!(response, Response::NewSession(_)));

    let mut control_rx = register_control_client(&handler, 711, None).await;
    assert_eq!(
        drain_control_notifications(&mut control_rx),
        vec!["%config-error startup config failed".to_owned()]
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
            include_inherited: true,
            quiet: false,
            include_hooks: false,
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

#[tokio::test]
async fn exact_client_attached_event_follows_rename_and_name_reuse_by_session_id() {
    let handler = RequestHandler::new();
    let original = session_name("client-attached-original");
    let renamed = session_name("client-attached-renamed");
    new_session(&handler, &original).await;
    let original_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&original)
            .expect("original session exists")
            .id()
    };

    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: original.clone(),
            new_name: renamed.clone(),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );
    new_session(&handler, &original).await;

    let mut events = handler.subscribe_lifecycle_events();
    handler
        .emit_client_attached_identity(9_901, original, original_id)
        .await;
    let queued = events
        .recv()
        .await
        .expect("exact client-attached event queued");
    assert_eq!(queued.control_session_identity, Some(original_id));
    assert!(matches!(
        queued.event,
        LifecycleEvent::ClientAttached { session_name, .. } if session_name == renamed
    ));
}

#[tokio::test]
async fn client_session_changed_notification_follows_rename_not_reused_name() {
    let handler = RequestHandler::new();
    let original = session_name("notify-session-original");
    let renamed = session_name("notify-session-renamed");
    let observer = session_name("notify-session-observer");
    new_session(&handler, &original).await;
    new_session(&handler, &observer).await;
    let original_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&original)
            .expect("session exists")
            .id()
    };
    let queued =
        prepared_client_session_changed(&handler, original.clone(), original_id, "9902").await;
    let mut observer_rx = register_control_client(&handler, 9_903, Some(observer)).await;
    let _ = drain_control_notifications(&mut observer_rx);

    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: original.clone(),
            new_name: renamed.clone(),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );
    new_session(&handler, &original).await;
    let _ = drain_control_notifications(&mut observer_rx);

    handler.dispatch_lifecycle_hook(queued).await;
    assert_eq!(
        drain_control_notifications(&mut observer_rx),
        vec![format!(
            "%client-session-changed 9902 ${} {renamed}",
            original_id.as_u32()
        )]
    );
}

#[tokio::test]
async fn hooks_disabled_client_session_changed_skips_deleted_reused_session() {
    let handler = RequestHandler::new();
    let replaced = session_name("notify-session-replaced");
    let observer = session_name("notify-session-disabled-observer");
    new_session(&handler, &replaced).await;
    new_session(&handler, &observer).await;
    let replaced_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&replaced)
            .expect("session exists")
            .id()
    };
    let queued =
        prepared_client_session_changed(&handler, replaced.clone(), replaced_id, "9904").await;
    let mut observer_rx = register_control_client(&handler, 9_905, Some(observer)).await;
    let _ = drain_control_notifications(&mut observer_rx);

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: replaced.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
    new_session(&handler, &replaced).await;
    let _ = drain_control_notifications(&mut observer_rx);

    crate::hook_runtime::with_hook_execution(Vec::new(), async {
        handler.emit_prepared(queued).await;
    })
    .await;
    assert!(drain_control_notifications(&mut observer_rx).is_empty());
}

#[tokio::test]
async fn control_notification_delivery_cannot_jump_to_reused_pid_registration() {
    let handler = RequestHandler::new();
    let requester_pid = 9_906;
    let (old_tx, mut old_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let old_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            old_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    let queued = {
        let mut state = handler.state.lock().await;
        super::prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::PasteBufferChanged {
                buffer_name: "recipient-aba".to_owned(),
            },
        )
    };
    let pause = handler.install_control_notification_delivery_pause();
    let dispatch_handler = handler.clone();
    let dispatch = tokio::spawn(async move {
        dispatch_handler
            .dispatch_control_notifications(&queued)
            .await;
    });
    pause.reached.notified().await;

    let replacement_handler = handler.clone();
    let replacement = tokio::spawn(async move {
        let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        let control_id = replacement_handler
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
        (control_id, event_rx)
    });
    tokio::task::yield_now().await;
    assert!(
        !replacement.is_finished(),
        "replacement registration waits for the identity-locked delivery"
    );

    pause.release.notify_one();
    dispatch.await.expect("notification dispatch completes");
    let (replacement_id, mut replacement_rx) = replacement
        .await
        .expect("replacement registration completes");
    assert_ne!(replacement_id, old_id);
    assert!(collect_control_events(&mut old_rx).iter().any(|event| {
        matches!(
            event,
            ControlServerEvent::Notification(line)
                if line == "%paste-buffer-changed recipient-aba"
        )
    }));
    assert!(drain_control_notifications(&mut replacement_rx).is_empty());
}
