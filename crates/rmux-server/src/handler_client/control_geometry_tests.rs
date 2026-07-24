use super::*;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use rmux_proto::{
    ControlMode, NewSessionRequest, OptionName, Request, Response, ScopeSelector, SessionName,
    SetOptionMode, SetOptionRequest, SwitchClientRequest, TerminalSize, WindowTarget,
};
use tokio::sync::mpsc;

use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};

const INITIAL_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const TARGET_SIZE: TerminalSize = TerminalSize { cols: 60, rows: 15 };
const CONTROL_SIZE: TerminalSize = TerminalSize {
    cols: 100,
    rows: 40,
};
const SOURCE_ATTACHED_SIZE: TerminalSize = TerminalSize { cols: 70, rows: 20 };
const TARGET_ATTACHED_SIZE: TerminalSize = TerminalSize { cols: 90, rows: 30 };

#[tokio::test]
async fn refresh_client_control_size_respects_window_size_policy_like_tmux37() {
    // Frozen tmux 3.7b oracle, 2026-07-25: with a 70x20 attached client,
    // refreshing a control client to 100x40 selects the control geometry for
    // latest/largest, the attached geometry for smallest, and no size for
    // manual. The oracle's visible window rows are one less for the ordinary
    // client because its status line consumes a row.
    for (index, (policy, expected_size)) in [
        ("latest", CONTROL_SIZE),
        ("largest", CONTROL_SIZE),
        ("smallest", SOURCE_ATTACHED_SIZE),
        ("manual", INITIAL_SIZE),
    ]
    .into_iter()
    .enumerate()
    {
        let handler = RequestHandler::new();
        let session = session_name(&format!("control-refresh-{policy}"));
        create_session(&handler, session.clone(), INITIAL_SIZE).await;
        set_window_size_policy(&handler, &session, policy).await;
        let _attach_events = register_attached_client(
            &handler,
            92_200 + index as u32,
            &session,
            SOURCE_ATTACHED_SIZE,
        )
        .await;
        let requester_pid = std::process::id();
        let _events = register_control_client(&handler, requester_pid, &session).await;

        let response = handler
            .handle(Request::RefreshClient(Box::new(
                refresh_client_size_request(requester_pid, CONTROL_SIZE),
            )))
            .await;

        assert!(
            matches!(response, Response::RefreshClient(_)),
            "{response:?}"
        );
        assert_eq!(
            control_client_size(&handler, requester_pid).await,
            CONTROL_SIZE
        );
        assert_eq!(
            session_size(&handler, &session).await,
            expected_size,
            "{policy}"
        );
    }
}

#[tokio::test]
async fn switch_control_client_reapplies_reported_size_like_tmux37() {
    // Frozen tmux 3.7b oracle, 2026-07-25: after refresh-client -C 100x40,
    // switch-client chooses that geometry for latest/largest, the resident
    // 90x30 attached geometry for smallest, and no resize for manual.
    let handler = RequestHandler::new();
    let source = session_name("control-switch-source");
    create_session(&handler, source.clone(), INITIAL_SIZE).await;
    let requester_pid = std::process::id();
    let _events = register_control_client(&handler, requester_pid, &source).await;
    let refreshed = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(requester_pid, CONTROL_SIZE),
        )))
        .await;
    assert!(
        matches!(refreshed, Response::RefreshClient(_)),
        "{refreshed:?}"
    );

    for (index, (policy, expected_size)) in [
        ("latest", CONTROL_SIZE),
        ("largest", CONTROL_SIZE),
        ("smallest", TARGET_ATTACHED_SIZE),
        ("manual", TARGET_SIZE),
    ]
    .into_iter()
    .enumerate()
    {
        let target = session_name(&format!("control-switch-{policy}"));
        create_session(&handler, target.clone(), TARGET_SIZE).await;
        set_window_size_policy(&handler, &target, policy).await;
        let _attach_events = register_attached_client(
            &handler,
            92_300 + index as u32,
            &target,
            TARGET_ATTACHED_SIZE,
        )
        .await;

        let response = handler
            .handle(Request::SwitchClient(SwitchClientRequest {
                target: target.clone(),
            }))
            .await;

        assert!(
            matches!(response, Response::SwitchClient(_)),
            "{response:?}"
        );
        assert_eq!(
            session_size(&handler, &target).await,
            expected_size,
            "{policy}"
        );
    }
}

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_session(handler: &RequestHandler, session: SessionName, size: TerminalSize) {
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session,
            detached: true,
            size: Some(size),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
}

async fn set_window_size_policy(handler: &RequestHandler, session: &SessionName, policy: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
            option: OptionName::WindowSize,
            value: policy.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn register_control_client(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
) -> mpsc::Receiver<ControlServerEvent> {
    let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
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
    handler
        .set_control_session(requester_pid, Some(session.clone()))
        .await
        .expect("set control session");
    event_rx
}

async fn register_attached_client(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
    size: TerminalSize,
) -> mpsc::UnboundedReceiver<crate::pane_io::AttachControl> {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;
    let mut active_attach = handler.active_attach.lock().await;
    let size_sequence = active_attach.next_size_sequence;
    active_attach.next_size_sequence = size_sequence.saturating_add(1);
    let active = active_attach
        .by_pid
        .get_mut(&requester_pid)
        .expect("attached client remains registered");
    active.client_size = size;
    active.size_sequence = size_sequence;
    drop(active_attach);
    handler.bump_active_attach_epoch();
    control_rx
}

fn refresh_client_size_request(
    requester_pid: u32,
    size: TerminalSize,
) -> rmux_proto::request::RefreshClientRequest {
    rmux_proto::request::RefreshClientRequest {
        target_client: Some(requester_pid.to_string()),
        adjustment: None,
        clear_pan: false,
        pan_left: false,
        pan_right: false,
        pan_up: false,
        pan_down: false,
        status_only: false,
        clipboard_query: false,
        flags: None,
        flags_alias: None,
        subscriptions: Vec::new(),
        subscriptions_format: Vec::new(),
        control_size: Some(format!("{}x{}", size.cols, size.rows)),
        colour_report: None,
    }
}

async fn control_client_size(handler: &RequestHandler, requester_pid: u32) -> TerminalSize {
    let active_control = handler.active_control.lock().await;
    let active = active_control
        .by_pid
        .get(&requester_pid)
        .expect("control client remains registered");
    TerminalSize {
        cols: active.client_width,
        rows: active.client_height,
    }
}

async fn session_size(handler: &RequestHandler, session: &SessionName) -> TerminalSize {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(session)
        .expect("session remains present")
        .window()
        .size()
}
