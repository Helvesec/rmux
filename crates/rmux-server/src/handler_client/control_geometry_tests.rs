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
        let (_attach_id, _attach_events) = register_attached_client(
            &handler,
            92_200 + index as u32,
            &session,
            SOURCE_ATTACHED_SIZE,
        )
        .await;
        let requester_pid = std::process::id();
        let (_control_id, _events) =
            register_control_client_with_id(&handler, requester_pid, &session).await;

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
async fn older_control_resize_keeps_the_latest_client_order_like_tmux37() {
    // Frozen tmux 3.7b oracle, 2026-07-25: `latest` is client arrival
    // order. Resizing an older control client updates its reported geometry
    // without making it the newest window-size candidate.
    let handler = RequestHandler::new();
    let session = session_name("control-latest-resize-order");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    set_window_size_policy(&handler, &session, "latest").await;
    let older_pid = 92_280;
    let latest_pid = older_pid + 1;
    let (_older_id, _older_events) =
        register_control_client_with_id(&handler, older_pid, &session).await;
    let older_size = TerminalSize {
        cols: 100,
        rows: 40,
    };
    let response = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(older_pid, older_size),
        )))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );

    let (_latest_id, _latest_events) =
        register_control_client_with_id(&handler, latest_pid, &session).await;
    let latest_size = TerminalSize { cols: 60, rows: 20 };
    let response = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(latest_pid, latest_size),
        )))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(session_size(&handler, &session).await, latest_size);

    let resized_older = TerminalSize {
        cols: 101,
        rows: 41,
    };
    let response = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(older_pid, resized_older),
        )))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(
        control_client_size(&handler, older_pid).await,
        resized_older,
        "the older control still records its new geometry"
    );
    assert_eq!(
        session_size(&handler, &session).await,
        latest_size,
        "resizing an older control must not steal latest-client ordering"
    );
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
    let (_control_id, _events) =
        register_control_client_with_id(&handler, requester_pid, &source).await;
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
        let (_attach_id, _attach_events) = register_attached_client(
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

#[tokio::test]
async fn control_clients_share_largest_and_smallest_size_candidates_like_tmux37() {
    for (index, (policy, first_size, second_size, expected_size)) in [
        ("largest", CONTROL_SIZE, TARGET_SIZE, CONTROL_SIZE),
        ("smallest", TARGET_SIZE, CONTROL_SIZE, TARGET_SIZE),
    ]
    .into_iter()
    .enumerate()
    {
        let handler = RequestHandler::new();
        let session = session_name(&format!("control-multi-{policy}"));
        create_session(&handler, session.clone(), INITIAL_SIZE).await;
        set_window_size_policy(&handler, &session, policy).await;
        let first_pid = 92_400 + index as u32 * 2;
        let second_pid = first_pid + 1;
        let (_first_id, _first_events) =
            register_control_client_with_id(&handler, first_pid, &session).await;
        let (_second_id, _second_events) =
            register_control_client_with_id(&handler, second_pid, &session).await;

        for (pid, size) in [(first_pid, first_size), (second_pid, second_size)] {
            let response = handler
                .handle(Request::RefreshClient(Box::new(
                    refresh_client_size_request(pid, size),
                )))
                .await;
            assert!(
                matches!(response, Response::RefreshClient(_)),
                "{response:?}"
            );
        }

        assert_eq!(
            session_size(&handler, &session).await,
            expected_size,
            "{policy}"
        );
    }
}

#[tokio::test]
async fn switching_control_client_reconciles_the_source_session_geometry() {
    let handler = RequestHandler::new();
    let source = session_name("control-switch-reconcile-source");
    let target = session_name("control-switch-reconcile-target");
    create_session(&handler, source.clone(), INITIAL_SIZE).await;
    create_session(&handler, target.clone(), TARGET_SIZE).await;
    set_window_size_policy(&handler, &source, "largest").await;
    set_window_size_policy(&handler, &target, "largest").await;

    let switching_pid = std::process::id();
    let surviving_pid = switching_pid.saturating_add(1);
    let switching_size = TerminalSize {
        cols: 101,
        rows: 41,
    };
    let surviving_size = TerminalSize { cols: 60, rows: 20 };
    let (_switching_id, _switching_events) =
        register_control_client_with_id(&handler, switching_pid, &source).await;
    let (_surviving_id, _surviving_events) =
        register_control_client_with_id(&handler, surviving_pid, &source).await;

    for (pid, size) in [
        (switching_pid, switching_size),
        (surviving_pid, surviving_size),
    ] {
        let response = handler
            .handle(Request::RefreshClient(Box::new(
                refresh_client_size_request(pid, size),
            )))
            .await;
        assert!(
            matches!(response, Response::RefreshClient(_)),
            "{response:?}"
        );
    }
    assert_eq!(session_size(&handler, &source).await, switching_size);

    let response = handler
        .handle(Request::SwitchClient(SwitchClientRequest {
            target: target.clone(),
        }))
        .await;

    assert!(
        matches!(response, Response::SwitchClient(_)),
        "{response:?}"
    );
    assert_eq!(session_size(&handler, &source).await, surviving_size);
    assert_eq!(session_size(&handler, &target).await, switching_size);
}

#[tokio::test]
async fn attached_arrival_and_departure_keep_control_geometry_in_every_automatic_policy() {
    // Frozen tmux 3.7b oracle, 2026-07-25: a control client remains a
    // window-size candidate while an ordinary attach arrives and after it
    // departs. Latest follows arrival order; largest/smallest aggregate both.
    for (index, (policy, control_size, attach_size, attached_size)) in [
        ("latest", CONTROL_SIZE, TARGET_SIZE, TARGET_SIZE),
        ("largest", CONTROL_SIZE, TARGET_SIZE, CONTROL_SIZE),
        ("smallest", TARGET_SIZE, CONTROL_SIZE, TARGET_SIZE),
    ]
    .into_iter()
    .enumerate()
    {
        let handler = RequestHandler::new();
        let session = session_name(&format!("control-attach-{policy}"));
        create_session(&handler, session.clone(), INITIAL_SIZE).await;
        set_window_size_policy(&handler, &session, policy).await;
        let control_pid = 92_500 + index as u32 * 2;
        let attach_pid = control_pid + 1;
        let (_control_id, _control_events) =
            register_control_client_with_id(&handler, control_pid, &session).await;
        let refreshed = handler
            .handle(Request::RefreshClient(Box::new(
                refresh_client_size_request(control_pid, control_size),
            )))
            .await;
        assert!(
            matches!(refreshed, Response::RefreshClient(_)),
            "{refreshed:?}"
        );

        let (attach_id, _attach_events) =
            register_attached_client(&handler, attach_pid, &session, attach_size).await;
        handler
            .reconcile_attached_session_size_and_emit(&session)
            .await
            .expect("ordinary attach arrival reconciles mixed client geometry");
        assert_eq!(
            session_size(&handler, &session).await,
            attached_size,
            "{policy} after ordinary attach"
        );

        handler.finish_attach(attach_pid, attach_id).await;
        assert_eq!(
            session_size(&handler, &session).await,
            control_size,
            "{policy} after ordinary detach"
        );
    }
}

#[tokio::test]
async fn control_departure_reconciles_surviving_control_geometry() {
    // Frozen tmux 3.7b oracle, 2026-07-25: removing the winning control
    // candidate restores the surviving control for latest/largest/smallest.
    for (index, (policy, first_size, second_size, removed_first, expected_size)) in [
        ("latest", CONTROL_SIZE, TARGET_SIZE, false, CONTROL_SIZE),
        ("largest", CONTROL_SIZE, TARGET_SIZE, true, TARGET_SIZE),
        ("smallest", TARGET_SIZE, CONTROL_SIZE, true, CONTROL_SIZE),
    ]
    .into_iter()
    .enumerate()
    {
        let handler = RequestHandler::new();
        let session = session_name(&format!("control-depart-{policy}"));
        create_session(&handler, session.clone(), INITIAL_SIZE).await;
        set_window_size_policy(&handler, &session, policy).await;
        let first_pid = 92_600 + index as u32 * 2;
        let second_pid = first_pid + 1;
        let (first_id, _first_events) =
            register_control_client_with_id(&handler, first_pid, &session).await;
        let (second_id, _second_events) =
            register_control_client_with_id(&handler, second_pid, &session).await;
        for (pid, size) in [(first_pid, first_size), (second_pid, second_size)] {
            let response = handler
                .handle(Request::RefreshClient(Box::new(
                    refresh_client_size_request(pid, size),
                )))
                .await;
            assert!(
                matches!(response, Response::RefreshClient(_)),
                "{response:?}"
            );
        }

        let (removed_pid, removed_id) = if removed_first {
            (first_pid, first_id)
        } else {
            (second_pid, second_id)
        };
        handler.finish_control(removed_pid, removed_id).await;
        assert_eq!(
            session_size(&handler, &session).await,
            expected_size,
            "{policy} after control departure"
        );
    }
}

#[tokio::test]
async fn window_size_option_reconciliation_includes_control_candidates() {
    let handler = RequestHandler::new();
    let session = session_name("control-option-reconcile");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    let control_pid = 92_650;
    let (_control_id, _control_events) =
        register_control_client_with_id(&handler, control_pid, &session).await;
    let refreshed = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(control_pid, CONTROL_SIZE),
        )))
        .await;
    assert!(
        matches!(refreshed, Response::RefreshClient(_)),
        "{refreshed:?}"
    );
    let (_attach_id, _attach_events) =
        register_attached_client(&handler, 92_651, &session, TARGET_SIZE).await;

    for (policy, expected_size) in [
        ("largest", CONTROL_SIZE),
        ("smallest", TARGET_SIZE),
        ("latest", TARGET_SIZE),
    ] {
        set_window_size_policy(&handler, &session, policy).await;
        assert_eq!(
            session_size(&handler, &session).await,
            expected_size,
            "{policy} option reconciliation"
        );
    }
}

#[tokio::test]
async fn control_resize_racing_reconcile_cannot_apply_a_stale_geometry() {
    let handler = RequestHandler::new();
    let session = session_name("control-resize-selection-race");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    set_window_size_policy(&handler, &session, "largest").await;
    let (_attach_id, _attach_events) =
        register_attached_client(&handler, 92_700, &session, INITIAL_SIZE).await;
    let control_pid = 92_701;
    let (_control_id, _control_events) =
        register_control_client_with_id(&handler, control_pid, &session).await;
    let refreshed = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(control_pid, CONTROL_SIZE),
        )))
        .await;
    assert!(
        matches!(refreshed, Response::RefreshClient(_)),
        "{refreshed:?}"
    );

    let pause = handler.install_attached_size_selection_pause();
    let reconcile_handler = handler.clone();
    let reconcile_session = session.clone();
    let reconcile = tokio::spawn(async move {
        reconcile_handler
            .reconcile_attached_session_size(&reconcile_session)
            .await
    });
    pause.reached.notified().await;

    let newest_size = TerminalSize {
        cols: 120,
        rows: 45,
    };
    let refreshed = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(control_pid, newest_size),
        )))
        .await;
    assert!(
        matches!(refreshed, Response::RefreshClient(_)),
        "{refreshed:?}"
    );
    pause.release.notify_one();

    reconcile
        .await
        .expect("reconcile task joins")
        .expect("reconcile succeeds");
    assert_eq!(
        session_size(&handler, &session).await,
        newest_size,
        "a selection predating the control resize must be retried"
    );
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

async fn register_control_client_with_id(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
) -> (u64, mpsc::Receiver<ControlServerEvent>) {
    let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
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
        .set_control_session(requester_pid, Some(session.clone()))
        .await
        .expect("set control session");
    (control_id, event_rx)
}

async fn register_attached_client(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
    size: TerminalSize,
) -> (u64, mpsc::UnboundedReceiver<crate::pane_io::AttachControl>) {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let attach_id = handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;
    let mut active_attach = handler.active_attach.lock().await;
    let size_sequence = handler.next_client_size_sequence();
    let active = active_attach
        .by_pid
        .get_mut(&requester_pid)
        .expect("attached client remains registered");
    active.client_size = size;
    active.size_sequence = size_sequence;
    drop(active_attach);
    handler.bump_active_attach_epoch();
    (attach_id, control_rx)
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
