use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use rmux_proto::{
    ControlMode, LinkWindowRequest, NewSessionRequest, NewWindowRequest, PaneKillRequest,
    PaneTargetRef, Request, Response, SessionName, TerminalSize, WindowTarget,
};
use tokio::sync::mpsc;

use super::RequestHandler;
use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn new_session(handler: &RequestHandler, name: &SessionName) {
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
}

async fn new_window(
    handler: &RequestHandler,
    session: &SessionName,
    detached: bool,
) -> WindowTarget {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let Response::NewWindow(response) = response else {
        panic!("new-window failed: {response:?}");
    };
    response.target
}

async fn link_window(
    handler: &RequestHandler,
    source: WindowTarget,
    target: WindowTarget,
    detached: bool,
) -> WindowTarget {
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source,
            target,
            after: false,
            before: false,
            kill_destination: false,
            detached,
        }))
        .await;
    let Response::LinkWindow(response) = response else {
        panic!("link-window failed: {response:?}");
    };
    response.target
}

async fn register_control(
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
        .expect("control attaches to test session");
    (control_id, event_rx)
}

async fn run_control(handler: &RequestHandler, requester_pid: u32, control_id: u64, command: &str) {
    let commands = handler
        .parse_control_commands(command)
        .await
        .expect("control command parses");
    let result = handler
        .execute_control_commands_identity(requester_pid, control_id, commands)
        .await;
    assert!(result.error.is_none(), "{command}: {:?}", result.error);
}

fn relevant_notifications(rx: &mut mpsc::Receiver<ControlServerEvent>) -> Vec<String> {
    let mut lines = Vec::new();
    while let Ok(event) = rx.try_recv() {
        let ControlServerEvent::Notification(line) = event else {
            continue;
        };
        if line.starts_with("%layout-change ") {
            lines.push(
                line.split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        } else if [
            "%session-window-changed ",
            "%window-pane-changed ",
            "%window-add ",
            "%window-close ",
            "%unlinked-window-add ",
            "%unlinked-window-close ",
            "%sessions-changed",
        ]
        .iter()
        .any(|prefix| line.starts_with(prefix))
        {
            lines.push(line);
        }
    }
    lines
}

async fn active_id_strings(
    handler: &RequestHandler,
    session_name: &SessionName,
) -> (String, String, String) {
    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(session_name)
        .expect("session exists");
    let window = session.window();
    let pane = window.active_pane().expect("active pane exists");
    (
        session.id().to_string(),
        window.id().to_string(),
        pane.id().to_string(),
    )
}

async fn window_id(handler: &RequestHandler, target: &WindowTarget) -> String {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .expect("window exists")
        .id()
        .to_string()
}

#[tokio::test]
async fn new_window_and_explicit_select_publish_only_stable_id_changes() {
    // Frozen tmux 3.7b, measured 2026-07-26: non-detached new-window
    // publishes session-window-changed before window-add; -d publishes only
    // window-add. Selecting the current stable window is silent.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_session(&handler, &alpha).await;
    let (control_id, mut rx) = register_control(&handler, 31_001, &alpha).await;
    let _ = relevant_notifications(&mut rx);

    run_control(&handler, 31_001, control_id, "new-window -t alpha").await;
    let (session_id, selected_window_id, _) = active_id_strings(&handler, &alpha).await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![
            format!("%session-window-changed {session_id} {selected_window_id}"),
            format!("%window-add {selected_window_id}"),
        ]
    );

    run_control(&handler, 31_001, control_id, "new-window -d -t alpha").await;
    let detached_window_id =
        window_id(&handler, &WindowTarget::with_window(alpha.clone(), 2)).await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![format!("%window-add {detached_window_id}")]
    );

    run_control(&handler, 31_001, control_id, "select-window -t alpha:1").await;
    assert!(relevant_notifications(&mut rx).is_empty());
    run_control(&handler, 31_001, control_id, "select-window -t alpha:0").await;
    let (_, initial_window_id, _) = active_id_strings(&handler, &alpha).await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![format!(
            "%session-window-changed {session_id} {initial_window_id}"
        )]
    );

    let source_path =
        std::env::temp_dir().join(format!("rmux-selection-source-{}.conf", std::process::id()));
    std::fs::write(&source_path, "new-window -t alpha\n").expect("write source-file fixture");
    run_control(
        &handler,
        31_001,
        control_id,
        &format!("source-file {}", source_path.display()),
    )
    .await;
    std::fs::remove_file(&source_path).expect("remove source-file fixture");
    let (_, sourced_window_id, _) = active_id_strings(&handler, &alpha).await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![
            format!("%session-window-changed {session_id} {sourced_window_id}"),
            format!("%window-add {sourced_window_id}"),
        ]
    );
}

#[tokio::test]
async fn link_and_unlink_window_follow_detached_and_no_switch_semantics() {
    // Frozen tmux 3.7b, measured 2026-07-26: link-window publishes window-add
    // before a real selection change; -d omits the selection event. Unlinking
    // the selected linked window publishes the change before window-close.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    new_session(&handler, &alpha).await;
    new_session(&handler, &beta).await;
    let beta_source = new_window(&handler, &beta, true).await;
    let beta_detached = new_window(&handler, &beta, true).await;
    let (control_id, mut rx) = register_control(&handler, 31_002, &alpha).await;
    let _ = relevant_notifications(&mut rx);

    run_control(
        &handler,
        31_002,
        control_id,
        "link-window -s beta:1 -t alpha:1",
    )
    .await;
    let (alpha_id, linked_window_id, _) = active_id_strings(&handler, &alpha).await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![
            format!("%window-add {linked_window_id}"),
            format!("%session-window-changed {alpha_id} {linked_window_id}"),
        ]
    );

    run_control(
        &handler,
        31_002,
        control_id,
        "link-window -d -s beta:2 -t alpha:2",
    )
    .await;
    let detached_window_id = window_id(&handler, &beta_detached).await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![format!("%window-add {detached_window_id}")]
    );

    run_control(&handler, 31_002, control_id, "unlink-window -t alpha:1").await;
    let (_, fallback_window_id, _) = active_id_strings(&handler, &alpha).await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![
            format!("%session-window-changed {alpha_id} {fallback_window_id}"),
            format!("%unlinked-window-close {linked_window_id}"),
        ]
    );

    let _ = beta_source;
    run_control(&handler, 31_002, control_id, "unlink-window -t alpha:2").await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![format!("%unlinked-window-close {detached_window_id}")]
    );
}

#[tokio::test]
async fn move_window_orders_destination_and_source_stable_transitions() {
    // Frozen tmux 3.7b, measured 2026-07-26: add, destination selection,
    // source selection, close. With -d, the destination selection is absent.
    for (detached, expect_destination_change) in [(false, true), (true, false)] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        let beta = session_name("beta");
        new_session(&handler, &alpha).await;
        let moving = new_window(&handler, &alpha, false).await;
        new_session(&handler, &beta).await;
        let moving_window_id = window_id(&handler, &moving).await;
        let (control_id, mut rx) = register_control(&handler, 31_003, &alpha).await;
        let _ = relevant_notifications(&mut rx);

        let flag = if detached { "-d " } else { "" };
        run_control(
            &handler,
            31_003,
            control_id,
            &format!("move-window {flag}-s alpha:1 -t beta:1"),
        )
        .await;
        let (alpha_id, alpha_window_id, _) = active_id_strings(&handler, &alpha).await;
        let (beta_id, _, _) = active_id_strings(&handler, &beta).await;
        let mut expected = vec![format!("%unlinked-window-add {moving_window_id}")];
        if expect_destination_change {
            expected.push(format!(
                "%session-window-changed {beta_id} {moving_window_id}"
            ));
        }
        expected.extend([
            format!("%session-window-changed {alpha_id} {alpha_window_id}"),
            format!("%unlinked-window-close {moving_window_id}"),
        ]);
        assert_eq!(relevant_notifications(&mut rx), expected);
    }
}

#[tokio::test]
async fn linked_kill_window_interleaves_each_real_session_transition_and_close() {
    // Frozen tmux 3.7b, measured 2026-07-26 on a window linked into alpha and
    // beta: alpha change, close, beta change, close.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    new_session(&handler, &alpha).await;
    let shared = new_window(&handler, &alpha, false).await;
    new_session(&handler, &beta).await;
    link_window(
        &handler,
        shared.clone(),
        WindowTarget::with_window(beta.clone(), 1),
        false,
    )
    .await;
    let shared_id = window_id(&handler, &shared).await;
    let (control_id, mut rx) = register_control(&handler, 31_004, &alpha).await;
    let _ = relevant_notifications(&mut rx);

    run_control(&handler, 31_004, control_id, "kill-window -t alpha:1").await;
    let (alpha_id, alpha_window_id, _) = active_id_strings(&handler, &alpha).await;
    let (beta_id, beta_window_id, _) = active_id_strings(&handler, &beta).await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![
            format!("%session-window-changed {alpha_id} {alpha_window_id}"),
            format!("%unlinked-window-close {shared_id}"),
            format!("%session-window-changed {beta_id} {beta_window_id}"),
            format!("%unlinked-window-close {shared_id}"),
        ]
    );
}

#[tokio::test]
async fn split_and_kill_pane_preserve_tmux_transition_order_and_detached_silence() {
    // Frozen tmux 3.7b, measured 2026-07-26: split selects before layout;
    // kill-pane lays out before selecting. Detached split and inactive kill
    // publish layout only.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    new_session(&handler, &alpha).await;
    new_session(&handler, &beta).await;
    link_window(
        &handler,
        WindowTarget::with_window(alpha.clone(), 0),
        WindowTarget::with_window(beta, 1),
        true,
    )
    .await;
    let (control_id, mut rx) = register_control(&handler, 31_005, &alpha).await;
    let _ = relevant_notifications(&mut rx);

    run_control(&handler, 31_005, control_id, "split-window -t alpha:0.0").await;
    let (_, window_id, selected_pane_id) = active_id_strings(&handler, &alpha).await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![
            format!("%window-pane-changed {window_id} {selected_pane_id}"),
            format!("%layout-change {window_id}"),
        ]
    );

    run_control(&handler, 31_005, control_id, "kill-pane -t alpha:0.1").await;
    let (_, _, fallback_pane_id) = active_id_strings(&handler, &alpha).await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![
            format!("%layout-change {window_id}"),
            format!("%window-pane-changed {window_id} {fallback_pane_id}"),
        ]
    );

    run_control(&handler, 31_005, control_id, "split-window -d -t alpha:0.0").await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![format!("%layout-change {window_id}")]
    );
    run_control(&handler, 31_005, control_id, "kill-pane -t alpha:0.1").await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![format!("%layout-change {window_id}")]
    );

    run_control(&handler, 31_005, control_id, "split-window -t alpha:0.0").await;
    let _ = relevant_notifications(&mut rx);
    let active_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .expect("alpha exists")
            .window()
            .active_pane()
            .expect("active pane exists")
            .id()
    };
    assert!(matches!(
        handler
            .handle(Request::PaneKill(PaneKillRequest {
                target: PaneTargetRef::by_id(alpha.clone(), active_pane_id),
                kill_all_except: false,
            }))
            .await,
        Response::KillPane(_)
    ));
    let (_, _, fallback_pane_id) = active_id_strings(&handler, &alpha).await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![
            format!("%layout-change {window_id}"),
            format!("%window-pane-changed {window_id} {fallback_pane_id}"),
        ]
    );
}

#[tokio::test]
async fn break_pane_orders_source_selection_layout_add_and_destination_selection() {
    // Frozen tmux 3.7b, measured 2026-07-26: source pane change, source
    // layout, window-add, then session-window-changed. -d omits only the last.
    for detached in [false, true] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        let beta = session_name("beta");
        new_session(&handler, &alpha).await;
        new_session(&handler, &beta).await;
        link_window(
            &handler,
            WindowTarget::with_window(alpha.clone(), 0),
            WindowTarget::with_window(beta, 1),
            true,
        )
        .await;
        let (control_id, mut rx) = register_control(&handler, 31_006, &alpha).await;
        let _ = relevant_notifications(&mut rx);
        run_control(&handler, 31_006, control_id, "split-window -t alpha:0.0").await;
        let _ = relevant_notifications(&mut rx);
        let (_, source_window_id, _) = active_id_strings(&handler, &alpha).await;

        let flag = if detached { "-d " } else { "" };
        run_control(
            &handler,
            31_006,
            control_id,
            &format!("break-pane {flag}-s alpha:0.1"),
        )
        .await;
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("alpha exists");
        let source_pane_id = session
            .window_at(0)
            .and_then(rmux_core::Window::active_pane)
            .expect("source pane survives")
            .id()
            .to_string();
        let new_window = session
            .windows()
            .map(|(_, window)| window)
            .find(|window| window.id().to_string() != source_window_id)
            .expect("break destination exists");
        let new_window_id = new_window.id().to_string();
        let session_id = session.id().to_string();
        drop(state);

        let mut expected = vec![
            format!("%window-pane-changed {source_window_id} {source_pane_id}"),
            format!("%layout-change {source_window_id}"),
            format!("%window-add {new_window_id}"),
        ];
        if !detached {
            expected.push(format!(
                "%session-window-changed {session_id} {new_window_id}"
            ));
        }
        assert_eq!(relevant_notifications(&mut rx), expected);
    }
}

#[tokio::test]
async fn initial_window_is_published_before_sessions_changed() {
    // Frozen tmux 3.7b, measured 2026-07-26: detached creation is
    // unlinked-window-add then sessions-changed; attached creation uses
    // window-add in the same order.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_session(&handler, &alpha).await;
    let (control_id, mut rx) = register_control(&handler, 31_007, &alpha).await;
    let _ = relevant_notifications(&mut rx);

    run_control(&handler, 31_007, control_id, "new-session -d -s beta").await;
    let (_, beta_window_id, _) = active_id_strings(&handler, &session_name("beta")).await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![
            format!("%unlinked-window-add {beta_window_id}"),
            "%sessions-changed".to_owned(),
        ]
    );

    run_control(&handler, 31_007, control_id, "new-session -s gamma").await;
    let (_, gamma_window_id, _) = active_id_strings(&handler, &session_name("gamma")).await;
    assert_eq!(
        relevant_notifications(&mut rx),
        vec![
            format!("%window-add {gamma_window_id}"),
            "%sessions-changed".to_owned(),
        ]
    );
}
