#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use rmux_proto::{ControlMode, SwapWindowRequest};

use super::*;
use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};

#[derive(Clone, Copy)]
struct IntraSessionCase {
    name: &'static str,
    windows: u32,
    active: u32,
    source: u32,
    target: u32,
    detached: bool,
    expected_identity_slot: u32,
}

#[derive(Debug)]
struct StableSessionSelection {
    session_name: SessionName,
    session_id: String,
    window_id: String,
}

async fn register_swap_control(
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
        .expect("control attaches to swap test session");
    (control_id, event_rx)
}

async fn run_swap_control(
    handler: &RequestHandler,
    requester_pid: u32,
    control_id: u64,
    command: &str,
) {
    let commands = handler
        .parse_control_commands(command)
        .await
        .expect("swap control command parses");
    let result = handler
        .execute_control_commands_identity(requester_pid, control_id, commands)
        .await;
    assert!(result.error.is_none(), "{command}: {:?}", result.error);
}

fn swap_notifications(rx: &mut mpsc::Receiver<ControlServerEvent>) -> Vec<String> {
    let mut lines = Vec::new();
    while let Ok(event) = rx.try_recv() {
        let ControlServerEvent::Notification(line) = event else {
            continue;
        };
        if line.starts_with("%session-window-changed ") || line.starts_with("%layout-change ") {
            lines.push(line);
        }
    }
    lines
}

async fn create_indexed_windows(handler: &RequestHandler, name: &str, windows: u32) -> SessionName {
    create_session(handler, name).await;
    let session_name = session_name(name);
    for window_index in 1..windows {
        insert_window(handler, &session_name, window_index).await;
    }
    session_name
}

async fn select_window(handler: &RequestHandler, session_name: &SessionName, window_index: u32) {
    let mut state = handler.state.lock().await;
    state
        .sessions
        .session_setup(session_name)
        .expect("swap test session exists")
        .select_window(window_index)
        .expect("swap test window selection succeeds");
}

async fn session_window_ids(
    handler: &RequestHandler,
    session_name: &SessionName,
) -> (String, BTreeMap<u32, String>) {
    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(session_name)
        .expect("swap test session exists");
    (
        session.id().to_string(),
        session
            .windows()
            .map(|(index, window)| (index, window.id().to_string()))
            .collect(),
    )
}

async fn active_window_id(handler: &RequestHandler, session_name: &SessionName) -> String {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(session_name)
        .expect("swap test session exists")
        .window()
        .id()
        .to_string()
}

async fn active_window_model(
    handler: &RequestHandler,
    session_names: &[SessionName],
) -> HashMap<String, String> {
    let state = handler.state.lock().await;
    session_names
        .iter()
        .map(|session_name| {
            let session = state
                .sessions
                .session(session_name)
                .expect("model session exists");
            (session.id().to_string(), session.window().id().to_string())
        })
        .collect()
}

async fn stable_selection_snapshot(
    handler: &RequestHandler,
    session_names: &[SessionName],
) -> Vec<StableSessionSelection> {
    let state = handler.state.lock().await;
    session_names
        .iter()
        .map(|session_name| {
            let session = state
                .sessions
                .session(session_name)
                .expect("snapshot session exists");
            StableSessionSelection {
                session_name: session_name.clone(),
                session_id: session.id().to_string(),
                window_id: session.window().id().to_string(),
            }
        })
        .collect()
}

fn semantic_notifications_from_snapshots(
    before: &[StableSessionSelection],
    after: &[StableSessionSelection],
    target_then_source_family: &[SessionName],
) -> Vec<String> {
    let mut seen = HashSet::new();
    target_then_source_family
        .iter()
        .filter_map(|session_name| {
            let before = before
                .iter()
                .find(|selection| &selection.session_name == session_name)
                .expect("ordered session exists in before snapshot");
            let after = after
                .iter()
                .find(|selection| &selection.session_name == session_name)
                .expect("ordered session exists in after snapshot");
            assert_eq!(
                before.session_id, after.session_id,
                "session identity remains stable"
            );
            if before.window_id == after.window_id || !seen.insert(after.session_id.clone()) {
                return None;
            }
            Some(format!(
                "%session-window-changed {} {}",
                after.session_id, after.window_id
            ))
        })
        .collect()
}

fn apply_session_window_events(model: &mut HashMap<String, String>, notifications: &[String]) {
    for line in notifications {
        if !line.starts_with("%session-window-changed ") {
            continue;
        }
        let mut fields = line.split_whitespace();
        assert_eq!(fields.next(), Some("%session-window-changed"));
        let session_id = fields.next().expect("event has session id");
        let window_id = fields.next().expect("event has window id");
        assert!(fields.next().is_none(), "unexpected event fields: {line}");
        model.insert(session_id.to_owned(), window_id.to_owned());
    }
}

#[tokio::test]
async fn swap_window_intra_session_publishes_only_real_identity_changes() {
    // tmux 3.7b was measured before this assertion. RMUX intentionally
    // normalizes the oracle's redundant and missing notifications to exactly
    // one event per real stable-identity transition.
    let cases = [
        IntraSessionCase {
            name: "inactive-d",
            windows: 3,
            active: 2,
            source: 0,
            target: 1,
            detached: true,
            expected_identity_slot: 0,
        },
        IntraSessionCase {
            name: "inactive-default",
            windows: 3,
            active: 2,
            source: 0,
            target: 1,
            detached: false,
            expected_identity_slot: 2,
        },
        IntraSessionCase {
            name: "inverse-d",
            windows: 3,
            active: 2,
            source: 1,
            target: 0,
            detached: true,
            expected_identity_slot: 1,
        },
        IntraSessionCase {
            name: "same-target-d",
            windows: 3,
            active: 2,
            source: 0,
            target: 0,
            detached: true,
            expected_identity_slot: 2,
        },
        IntraSessionCase {
            name: "source-active-d",
            windows: 3,
            active: 0,
            source: 0,
            target: 1,
            detached: true,
            expected_identity_slot: 0,
        },
        IntraSessionCase {
            name: "target-active-d",
            windows: 3,
            active: 1,
            source: 0,
            target: 1,
            detached: true,
            expected_identity_slot: 0,
        },
        IntraSessionCase {
            name: "source-active-default",
            windows: 3,
            active: 0,
            source: 0,
            target: 1,
            detached: false,
            expected_identity_slot: 1,
        },
        IntraSessionCase {
            name: "target-active-default",
            windows: 3,
            active: 1,
            source: 0,
            target: 1,
            detached: false,
            expected_identity_slot: 0,
        },
        IntraSessionCase {
            name: "two-source-active-d",
            windows: 2,
            active: 0,
            source: 0,
            target: 1,
            detached: true,
            expected_identity_slot: 0,
        },
        IntraSessionCase {
            name: "two-target-active-d",
            windows: 2,
            active: 1,
            source: 0,
            target: 1,
            detached: true,
            expected_identity_slot: 0,
        },
    ];

    for (offset, case) in cases.into_iter().enumerate() {
        let handler = RequestHandler::new();
        let session_name = create_indexed_windows(&handler, case.name, case.windows).await;
        select_window(&handler, &session_name, case.active).await;
        let (session_id, initial_ids) = session_window_ids(&handler, &session_name).await;
        let before = active_window_id(&handler, &session_name).await;
        let expected_after = initial_ids
            .get(&case.expected_identity_slot)
            .expect("expected identity slot exists")
            .clone();
        let (_control_id, mut rx) =
            register_swap_control(&handler, 32_000 + offset as u32, &session_name).await;
        let _ = swap_notifications(&mut rx);

        let response = handler
            .handle(Request::SwapWindow(SwapWindowRequest {
                source: WindowTarget::with_window(session_name.clone(), case.source),
                target: WindowTarget::with_window(session_name.clone(), case.target),
                detached: case.detached,
            }))
            .await;
        assert!(
            matches!(response, Response::SwapWindow(_)),
            "{}: {response:?}",
            case.name
        );

        let notifications = swap_notifications(&mut rx);
        let expected_notifications = if before == expected_after {
            Vec::new()
        } else {
            vec![format!(
                "%session-window-changed {session_id} {expected_after}"
            )]
        };
        assert_eq!(
            notifications, expected_notifications,
            "{} must publish exactly its real identity transition",
            case.name
        );
        assert_eq!(
            active_window_id(&handler, &session_name).await,
            expected_after,
            "{} active identity",
            case.name
        );
    }
}

#[tokio::test]
async fn swap_window_repetition_keeps_snapshot_event_model_exact_without_rescan() {
    let handler = RequestHandler::new();
    let alpha = create_indexed_windows(&handler, "repeat-alpha", 3).await;
    select_window(&handler, &alpha, 2).await;
    let (session_id, initial_ids) = session_window_ids(&handler, &alpha).await;
    let requester_pid = 32_100;
    let (control_id, mut rx) = register_swap_control(&handler, requester_pid, &alpha).await;
    let _ = swap_notifications(&mut rx);
    let mut model = active_window_model(&handler, std::slice::from_ref(&alpha)).await;

    for expected_slot in [0, 1] {
        run_swap_control(
            &handler,
            requester_pid,
            control_id,
            "swap-window -d -s repeat-alpha:0 -t repeat-alpha:1",
        )
        .await;
        let expected_window_id = initial_ids
            .get(&expected_slot)
            .expect("repetition identity exists");
        let notifications = swap_notifications(&mut rx);
        assert_eq!(
            notifications,
            vec![format!(
                "%session-window-changed {session_id} {expected_window_id}"
            )]
        );
        apply_session_window_events(&mut model, &notifications);
        assert_eq!(
            model.get(&session_id),
            Some(expected_window_id),
            "snapshot plus events predicts the repeated transition"
        );
        assert_eq!(
            active_window_id(&handler, &alpha).await,
            *expected_window_id,
            "final query validates but does not update the model"
        );
    }
}

#[tokio::test]
async fn cross_session_swap_orders_each_real_transition_target_then_source() {
    let handler = RequestHandler::new();
    let alpha = create_indexed_windows(&handler, "cross-alpha", 2).await;
    let beta = create_indexed_windows(&handler, "cross-beta", 2).await;
    select_window(&handler, &alpha, 1).await;
    select_window(&handler, &beta, 1).await;
    let (alpha_id, alpha_windows) = session_window_ids(&handler, &alpha).await;
    let (beta_id, beta_windows) = session_window_ids(&handler, &beta).await;
    let (_control_id, mut rx) = register_swap_control(&handler, 32_200, &alpha).await;
    let _ = swap_notifications(&mut rx);
    let sessions = [alpha.clone(), beta.clone()];
    let mut model = active_window_model(&handler, &sessions).await;

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 0),
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::SwapWindow(_)), "{response:?}");

    let source_window_id = alpha_windows.get(&0).expect("source identity");
    let target_window_id = beta_windows.get(&0).expect("target identity");
    let notifications = swap_notifications(&mut rx);
    assert_eq!(
        notifications,
        vec![
            format!("%session-window-changed {beta_id} {source_window_id}"),
            format!("%session-window-changed {alpha_id} {target_window_id}"),
        ],
        "tmux 3.7b orders cross-session selection target then source"
    );
    apply_session_window_events(&mut model, &notifications);
    assert_eq!(model, active_window_model(&handler, &sessions).await);
}

#[tokio::test]
async fn cross_session_swap_orders_complete_grouped_families_from_stable_snapshots() {
    let handler = RequestHandler::new();
    let alpha = create_indexed_windows(&handler, "family-alpha", 3).await;
    let gamma = session_name("family-gamma");
    create_grouped_session(&handler, gamma.as_str(), &alpha).await;
    let beta = create_indexed_windows(&handler, "family-beta", 3).await;
    let unchanged = create_indexed_windows(&handler, "family-unchanged", 1).await;
    for session_name in [&alpha, &gamma, &beta, &unchanged] {
        select_window(&handler, session_name, 0).await;
    }

    let (_alpha_id, alpha_windows) = session_window_ids(&handler, &alpha).await;
    let (_beta_id, beta_windows) = session_window_ids(&handler, &beta).await;
    let source_window_id = alpha_windows.get(&0).expect("source identity").clone();
    let target_window_id = beta_windows.get(&0).expect("target identity").clone();
    let session_names = [
        alpha.clone(),
        gamma.clone(),
        beta.clone(),
        unchanged.clone(),
    ];
    let requester_pid = 32_250;
    let (control_id, mut rx) = register_swap_control(&handler, requester_pid, &alpha).await;
    let _ = swap_notifications(&mut rx);

    let family_orders = [
        vec![
            beta.clone(),
            alpha.clone(),
            gamma.clone(),
            beta.clone(),
            unchanged.clone(),
        ],
        vec![
            alpha.clone(),
            gamma.clone(),
            beta.clone(),
            alpha.clone(),
            unchanged.clone(),
        ],
    ];
    for (operation, family_order) in family_orders.into_iter().enumerate() {
        let before = stable_selection_snapshot(&handler, &session_names).await;
        run_swap_control(
            &handler,
            requester_pid,
            control_id,
            &format!("swap-window -s {source_window_id} -t {target_window_id}"),
        )
        .await;
        let after = stable_selection_snapshot(&handler, &session_names).await;
        let expected = semantic_notifications_from_snapshots(&before, &after, &family_order);
        let notifications = swap_notifications(&mut rx);
        assert_eq!(
            notifications,
            expected,
            "operation {} publishes the whole target family before the source family",
            operation + 1
        );

        let mut model = before
            .iter()
            .map(|selection| (selection.session_id.clone(), selection.window_id.clone()))
            .collect::<HashMap<_, _>>();
        apply_session_window_events(&mut model, &notifications);
        let final_snapshot = after
            .iter()
            .map(|selection| (selection.session_id.clone(), selection.window_id.clone()))
            .collect::<HashMap<_, _>>();
        assert_eq!(
            model,
            final_snapshot,
            "snapshot plus ordered events reconstructs operation {}",
            operation + 1
        );
    }
}

#[tokio::test]
async fn grouped_and_linked_peers_do_not_receive_identity_stable_noise() {
    let handler = RequestHandler::new();
    let owner = create_indexed_windows(&handler, "group-owner", 3).await;
    let peer = session_name("group-peer");
    create_grouped_session(&handler, peer.as_str(), &owner).await;
    select_window(&handler, &owner, 2).await;
    select_window(&handler, &peer, 2).await;
    let (owner_id, owner_windows) = session_window_ids(&handler, &owner).await;
    let (_peer_id, peer_windows) = session_window_ids(&handler, &peer).await;
    let peer_before = peer_windows.get(&2).expect("peer active identity").clone();
    let (_control_id, mut rx) = register_swap_control(&handler, 32_300, &owner).await;
    let _ = swap_notifications(&mut rx);

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(owner.clone(), 1),
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::SwapWindow(_)), "{response:?}");
    assert_eq!(
        swap_notifications(&mut rx),
        vec![format!(
            "%session-window-changed {owner_id} {}",
            owner_windows.get(&0).expect("owner source identity")
        )]
    );
    assert_eq!(active_window_id(&handler, &peer).await, peer_before);

    let handler = RequestHandler::new();
    let owner = create_indexed_windows(&handler, "link-owner", 3).await;
    let peer = create_indexed_windows(&handler, "link-peer", 1).await;
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(peer.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    select_window(&handler, &owner, 2).await;
    select_window(&handler, &peer, 1).await;
    let (owner_id, owner_windows) = session_window_ids(&handler, &owner).await;
    let peer_before = active_window_id(&handler, &peer).await;
    let (_control_id, mut rx) = register_swap_control(&handler, 32_301, &owner).await;
    let _ = swap_notifications(&mut rx);

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(owner.clone(), 1),
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::SwapWindow(_)), "{response:?}");
    assert_eq!(
        swap_notifications(&mut rx),
        vec![format!(
            "%session-window-changed {owner_id} {}",
            owner_windows.get(&0).expect("owner source identity")
        )]
    );
    assert_eq!(active_window_id(&handler, &peer).await, peer_before);
}
