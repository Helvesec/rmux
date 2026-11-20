//! `choose-tree`'s default action is `switch-client -Zt`, so it owes both
//! halves of a switch the same geometry notifications any other `switch-client`
//! owes.

use super::super::mode_tree_order::{pane_item_id, session_item_id};
use super::*;
#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;

use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};
use rmux_proto::NewWindowRequest;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

const NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(5);
const NOTIFICATION_POLL: Duration = Duration::from_millis(25);
const NOTIFICATION_SETTLE: Duration = Duration::from_millis(250);

/// Frozen tmux 3.7b oracle, measured 2026-07-25
/// (`oracle/scenario_switch_destination.py`): a 101x41 client
/// switching onto a session whose only other client is a 60x20 control client
/// stores the 101x41 terminal size while the default one-line status leaves a
/// 101x40 content layout, and that control client receives
///     %client-session-changed <client> $1 target
///     %layout-change @1 aefe,101x40,0,0,1 aefe,101x40,0,0,1 *
/// in that order. `choose-tree` reaches `switch-client -Zt` through its own
/// action path, so it is asserted separately from the command form.
#[tokio::test]
async fn choose_tree_switch_notifies_the_destination_session_layout_change_like_tmux37() {
    let handler = RequestHandler::new();
    let source = SessionName::new("choose-tree-switch-source").expect("valid session");
    let target = SessionName::new("choose-tree-switch-target").expect("valid session");
    create_test_session(&handler, &source).await;
    create_test_session(&handler, &target).await;
    set_window_size_largest(&handler, &source).await;
    set_window_size_largest(&handler, &target).await;

    let attach_pid = std::process::id().saturating_add(211);
    let control_pid = attach_pid.saturating_add(1);
    let switching_size = TerminalSize {
        cols: 101,
        rows: 41,
    };
    let destination_size = TerminalSize { cols: 60, rows: 20 };

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, source.clone(), control_tx)
        .await;
    set_attached_client_size(&handler, attach_pid, switching_size).await;

    let (event_tx, mut control_events) = tokio::sync::mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    handler
        .register_control_with_closing(
            control_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: rmux_proto::ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(control_pid, Some(target.clone()))
        .await
        .expect("set control session");
    assert!(matches!(
        handler
            .handle(Request::RefreshClient(Box::new(control_size_request(
                control_pid,
                destination_size
            ))))
            .await,
        Response::RefreshClient(_)
    ));
    assert_eq!(window_size(&handler, &target).await, destination_size);

    let target_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&target)
        .expect("target session exists")
        .id();
    let target_window_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&target)
        .expect("target session exists")
        .window()
        .id()
        .as_u32();
    let layout_prefix = format!("%layout-change @{target_window_id} ");

    let parsed = CommandParser::new()
        .parse_arguments(["choose-tree"])
        .expect("choose-tree parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("mode-tree command parses")
        .expect("mode-tree command recognized");
    handler
        .execute_queued_mode_tree(
            attach_pid,
            command,
            &QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("choose-tree opens");
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get_mut(&attach_pid)
        .and_then(|active| active.mode_tree.as_mut())
        .expect("choose-tree remains active")
        .selected_id = Some(session_item_id(target_session_id));
    settle_notifications(&mut control_events).await;

    handler
        .accept_mode_tree_selection(attach_pid)
        .await
        .expect("choose-tree switch-client -Zt succeeds");

    assert_eq!(
        window_size(&handler, &target).await,
        TerminalSize {
            cols: switching_size.cols,
            rows: switching_size.rows - 1,
        },
        "choose-tree's switch must grow the destination window"
    );
    let lines = notifications_through(&mut control_events, &layout_prefix).await;
    let layout_index = lines
        .iter()
        .position(|line| line.starts_with(&layout_prefix))
        .expect("the destination control client must be told its window grew");
    assert_eq!(
        lines[layout_index]
            .split_whitespace()
            .nth(2)
            .and_then(|layout| layout.split(',').nth(1)),
        Some("101x40"),
        "{:?}",
        lines[layout_index]
    );
    let session_changed_index = lines
        .iter()
        .position(|line| line.starts_with("%client-session-changed "))
        .expect("the destination control client must be told the client arrived");
    assert!(
        session_changed_index < layout_index,
        "tmux 3.7b reports the client move before the layout it causes: {lines:?}"
    );
}

/// `choose-tree` reaches the same switch helper as `switch-client`, so a client
/// that never declared a size must carry its outer terminal anchor — not the
/// status-subtracted content rows it was registered against — onto the
/// destination session.
#[tokio::test]
async fn choose_tree_switch_carries_a_sizeless_client_outer_terminal_anchor() {
    let handler = RequestHandler::new();
    let source = SessionName::new("choose-tree-sizeless-source").expect("valid session");
    let target = SessionName::new("choose-tree-sizeless-target").expect("valid session");
    create_test_session(&handler, &source).await;
    create_test_session(&handler, &target).await;
    for session in [&source, &target] {
        assert!(matches!(
            handler
                .handle(Request::SetOption(SetOptionRequest {
                    scope: ScopeSelector::Session((*session).clone()),
                    option: OptionName::Status,
                    value: "2".to_owned(),
                    mode: SetOptionMode::Replace,
                }))
                .await,
            Response::SetOption(_)
        ));
    }

    let declared_pid = std::process::id().saturating_add(311);
    let sizeless_pid = declared_pid.saturating_add(1);
    let (declared_tx, _declared_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(declared_pid, source.clone(), declared_tx)
        .await;
    handler
        .handle_attached_resize(declared_pid, TerminalSize { cols: 80, rows: 24 })
        .await
        .expect("declared terminal geometry seeds status-aware content size");
    assert_eq!(
        window_size(&handler, &source).await,
        TerminalSize { cols: 80, rows: 22 }
    );

    let (sizeless_tx, _sizeless_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(sizeless_pid, source.clone(), sizeless_tx)
        .await;

    let target_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&target)
        .expect("target session exists")
        .id();
    let parsed = CommandParser::new()
        .parse_arguments(["choose-tree"])
        .expect("choose-tree parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("mode-tree command parses")
        .expect("mode-tree command recognized");
    handler
        .execute_queued_mode_tree(
            sizeless_pid,
            command,
            &QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("choose-tree opens");
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get_mut(&sizeless_pid)
        .and_then(|active| active.mode_tree.as_mut())
        .expect("choose-tree remains active")
        .selected_id = Some(session_item_id(target_session_id));

    handler
        .accept_mode_tree_selection(sizeless_pid)
        .await
        .expect("choose-tree switch-client -Zt succeeds");

    assert_eq!(
        session_terminal_size(&handler, &target).await,
        TerminalSize { cols: 80, rows: 24 },
        "choose-tree must carry the outer terminal anchor to the destination"
    );
    assert_eq!(
        window_size(&handler, &target).await,
        TerminalSize { cols: 80, rows: 22 },
        "the destination subtracts its own status rows exactly once"
    );
}

#[tokio::test]
async fn choose_tree_pane_switch_keeps_the_control_selection_model_current() {
    let handler = RequestHandler::new();
    let source = SessionName::new("choose-tree-selection-source").expect("valid session");
    let target = SessionName::new("choose-tree-selection-target").expect("valid session");
    create_test_session(&handler, &source).await;
    create_test_session(&handler, &target).await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: target.clone(),
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
    let Response::NewWindow(window) = response else {
        panic!("target window creation failed: {response:?}");
    };
    assert_eq!(window.target.window_index(), 1);
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(target.clone(), 1, 0,)),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    handler.wait_for_initial_panes_for_test().await;

    let (
        pane_item,
        session_id,
        initial_window_id,
        target_window_id,
        initial_pane_id,
        target_pane_id,
    ) = {
        let mut state = handler.state.lock().await;
        state.ensure_live_window_link_occurrences();
        let mut session = state
            .sessions
            .session_setup(&target)
            .expect("target session exists");
        session
            .select_pane_in_window(1, 0)
            .expect("inactive target pane selected for setup");
        session
            .select_window(0)
            .expect("source target window selected for setup");
        let session_id = session.id();
        let initial_window_id = session.window().id();
        let target_window = session.window_at(1).expect("target window exists");
        let target_window_id = target_window.id();
        let initial_pane_id = target_window
            .active_pane()
            .expect("target window has an active pane")
            .id();
        let target_pane_id = target_window
            .pane(1)
            .expect("inactive target pane exists")
            .id();
        let occurrence_id = state
            .window_link_occurrence_id(&target, 1)
            .expect("target occurrence has a stable identity");
        (
            pane_item_id(
                session_id,
                1,
                target_window_id,
                occurrence_id,
                target_pane_id,
            ),
            session_id,
            initial_window_id,
            target_window_id,
            initial_pane_id,
            target_pane_id,
        )
    };

    let attach_pid = std::process::id().saturating_add(212);
    let control_pid = attach_pid.saturating_add(1);
    let (attach_tx, _attach_rx) = mpsc::unbounded_channel();
    handler.register_attach(attach_pid, source, attach_tx).await;
    let (event_tx, mut control_events) = tokio::sync::mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    handler
        .register_control_with_closing(
            control_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: rmux_proto::ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(control_pid, Some(target.clone()))
        .await
        .expect("set control session");

    let parsed = CommandParser::new()
        .parse_arguments(["choose-tree"])
        .expect("choose-tree parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("mode-tree command parses")
        .expect("mode-tree command recognized");
    handler
        .execute_queued_mode_tree(
            attach_pid,
            command,
            &QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("choose-tree opens");
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get_mut(&attach_pid)
        .and_then(|active| active.mode_tree.as_mut())
        .expect("choose-tree remains active")
        .selected_id = Some(pane_item);
    settle_notifications(&mut control_events).await;

    handler
        .accept_mode_tree_selection(attach_pid)
        .await
        .expect("choose-tree pane switch succeeds");
    let lines = notifications_through(&mut control_events, "%client-session-changed ").await;
    let transitions = lines
        .iter()
        .filter_map(|line| {
            let event = line.split_whitespace().next()?;
            matches!(
                event,
                "%window-pane-changed" | "%session-window-changed" | "%client-session-changed"
            )
            .then_some(event)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        transitions,
        vec![
            "%window-pane-changed",
            "%session-window-changed",
            "%client-session-changed",
        ]
    );

    let session_id = session_id.to_string();
    let target_window_id = target_window_id.to_string();
    let target_pane_id = target_pane_id.to_string();
    let mut predicted_window_id = initial_window_id.to_string();
    let mut predicted_pane_id = initial_pane_id.to_string();
    for line in &lines {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        match fields.first().copied() {
            Some("%session-window-changed")
                if fields.get(1).copied() == Some(session_id.as_str()) =>
            {
                predicted_window_id = fields[2].to_owned();
            }
            Some("%window-pane-changed")
                if fields.get(1).copied() == Some(target_window_id.as_str()) =>
            {
                predicted_pane_id = fields[2].to_owned();
            }
            _ => {}
        }
    }
    assert_eq!(predicted_window_id, target_window_id);
    assert_eq!(predicted_pane_id, target_pane_id);
}

async fn create_test_session(handler: &RequestHandler, session_name: &SessionName) {
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
}

fn control_size_request(
    control_pid: u32,
    size: TerminalSize,
) -> rmux_proto::request::RefreshClientRequest {
    rmux_proto::request::RefreshClientRequest {
        target_client: Some(control_pid.to_string()),
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

async fn set_window_size_largest(handler: &RequestHandler, session_name: &SessionName) {
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(session_name.clone()),
                option: OptionName::WindowSize,
                value: "largest".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
}

async fn set_attached_client_size(handler: &RequestHandler, attach_pid: u32, size: TerminalSize) {
    let size_sequence = handler.next_client_size_sequence();
    let mut active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get_mut(&attach_pid)
        .expect("attached client remains registered");
    active.set_declared_client_size(size);
    active.size_sequence = size_sequence;
    drop(active_attach);
    handler.bump_active_attach_epoch();
}

async fn session_terminal_size(
    handler: &RequestHandler,
    session_name: &SessionName,
) -> TerminalSize {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(session_name)
        .expect("session remains present")
        .terminal_size()
}

async fn window_size(handler: &RequestHandler, session_name: &SessionName) -> TerminalSize {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(session_name)
        .expect("session remains present")
        .window()
        .size()
}

async fn notifications_through(
    events: &mut tokio::sync::mpsc::Receiver<ControlServerEvent>,
    prefix: &str,
) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + NOTIFICATION_TIMEOUT;
    let mut lines = Vec::new();
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(NOTIFICATION_POLL, events.recv()).await {
            Ok(Some(ControlServerEvent::Notification(line))) => {
                let matched = line.starts_with(prefix);
                lines.push(line);
                if matched {
                    return lines;
                }
            }
            Ok(Some(_)) | Err(_) => continue,
            Ok(None) => break,
        }
    }
    lines
}

async fn settle_notifications(events: &mut tokio::sync::mpsc::Receiver<ControlServerEvent>) {
    let mut deadline = tokio::time::Instant::now() + NOTIFICATION_SETTLE;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(NOTIFICATION_POLL, events.recv()).await {
            Ok(Some(_)) => deadline = tokio::time::Instant::now() + NOTIFICATION_SETTLE,
            Ok(None) => break,
            Err(_) => continue,
        }
    }
}
