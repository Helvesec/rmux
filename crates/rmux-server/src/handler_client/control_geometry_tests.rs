use super::*;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use rmux_proto::{
    ControlMode, DetachClientRequest, KillSessionRequest, NewSessionRequest, OptionName, Request,
    Response, ScopeSelector, SessionName, SetOptionMode, SetOptionRequest, SwitchClientRequest,
    TerminalSize, WindowTarget,
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
const CONTROL_NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_NOTIFICATION_SETTLE: Duration = Duration::from_millis(250);
const CONTROL_NOTIFICATION_POLL: Duration = Duration::from_millis(25);

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
async fn undeclared_control_client_never_shrinks_an_attached_session_like_tmux37() {
    // tmux 3.7b oracle, measured 2026-07-25 with a live 200x50 PTY client on a
    // 200x50 session, then `tmux -C attach -t main` and no `refresh-client -C`:
    //
    //   window-size   before   after
    //   latest        200x49   200x49
    //   largest       200x49   200x49
    //   smallest      200x49   200x49
    //   manual        200x50   200x50
    //
    // `ignore_client_size()` skips a CLIENT_CONTROL client that has no
    // CLIENT_SIZECHANGED, so the 80x24 placeholder a control client starts with
    // is not a size candidate. rmux used to hand it to the policy and crushed
    // the session to the placeholder under latest and smallest.
    // Probe: .rmux-audit/control-attach-8023/probe_ctl_attach_shrink.py
    for (index, policy) in ["latest", "largest", "smallest", "manual"]
        .into_iter()
        .enumerate()
    {
        let handler = RequestHandler::new();
        let session = session_name(&format!("control-undeclared-attached-{policy}"));
        create_session(&handler, session.clone(), INITIAL_SIZE).await;
        set_window_size_policy(&handler, &session, policy).await;
        let attach_pid = 92_700 + index as u32 * 2;
        let control_pid = attach_pid + 1;
        let (_attach_id, _attach_events) =
            register_attached_client(&handler, attach_pid, &session, CONTROL_SIZE).await;
        handler
            .reconcile_attached_session_size_and_emit(&session)
            .await
            .expect("the ordinary client owns the session geometry");
        let expected_size = if policy == "manual" {
            INITIAL_SIZE
        } else {
            CONTROL_SIZE
        };
        assert_eq!(
            session_size(&handler, &session).await,
            expected_size,
            "{policy} before the control client arrives"
        );

        let (_control_id, _control_events) =
            register_control_client_with_id(&handler, control_pid, &session).await;
        handler
            .reconcile_attached_session_size_and_emit(&session)
            .await
            .expect("a control arrival reconciles the session geometry");

        assert_eq!(
            session_size(&handler, &session).await,
            expected_size,
            "{policy}: a control client that never ran refresh-client -C must not resize"
        );
    }
}

#[tokio::test]
async fn undeclared_control_client_alone_never_resizes_the_session_like_tmux37() {
    // tmux 3.7b oracle, measured 2026-07-25: a 200x50 session with no other
    // client at all keeps 200x50 under every `window-size` value when a control
    // client attaches without declaring a size. With no ordinary client to
    // outvote it the placeholder also won `largest`, so this cell is the one
    // that pins the rule for every automatic policy.
    // Probe: .rmux-audit/control-attach-8023/probe_ctl_only_client.py
    for (index, policy) in ["latest", "largest", "smallest", "manual"]
        .into_iter()
        .enumerate()
    {
        let handler = RequestHandler::new();
        let session = session_name(&format!("control-undeclared-alone-{policy}"));
        create_session(&handler, session.clone(), CONTROL_SIZE).await;
        set_window_size_policy(&handler, &session, policy).await;
        let control_pid = 92_720 + index as u32;
        let (_control_id, _control_events) =
            register_control_client_with_id(&handler, control_pid, &session).await;
        handler
            .reconcile_attached_session_size_and_emit(&session)
            .await
            .expect("a control arrival reconciles the session geometry");

        assert_eq!(
            session_size(&handler, &session).await,
            CONTROL_SIZE,
            "{policy}: the session keeps its own geometry, the control client owns none"
        );
    }
}

#[tokio::test]
async fn undeclared_control_client_switch_never_resizes_the_target_like_tmux37() {
    // The same rule on the switch-client path, which feeds the reporting
    // client's own size straight into the policy instead of going through the
    // candidate list. tmux 3.7b, 2026-07-25: a control client that never ran
    // `refresh-client -C` leaves the destination session's geometry alone.
    for (index, policy) in ["latest", "largest", "smallest", "manual"]
        .into_iter()
        .enumerate()
    {
        let handler = RequestHandler::new();
        let source = session_name(&format!("control-undeclared-switch-source-{policy}"));
        create_session(&handler, source.clone(), INITIAL_SIZE).await;
        let (_control_id, _control_events) =
            register_control_client_with_id(&handler, std::process::id(), &source).await;

        let target = session_name(&format!("control-undeclared-switch-target-{policy}"));
        create_session(&handler, target.clone(), INITIAL_SIZE).await;
        set_window_size_policy(&handler, &target, policy).await;
        let (_attach_id, _attach_events) =
            register_attached_client(&handler, 92_740 + index as u32, &target, CONTROL_SIZE).await;
        handler
            .reconcile_attached_session_size_and_emit(&target)
            .await
            .expect("the ordinary client owns the destination geometry");
        let expected_size = if policy == "manual" {
            INITIAL_SIZE
        } else {
            CONTROL_SIZE
        };

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
            "{policy}: switching an undeclared control client must not resize the target"
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
async fn switching_control_client_notifies_the_source_session_layout_change_like_tmux37() {
    // Frozen tmux 3.7b oracle, measured 2026-07-25 with two control clients on
    // `source` (101x41 and 60x20, window-size largest). After the 101x41 client
    // runs `switch-client -t target`, the surviving 60x20 control client of the
    // source session receives, in this order:
    //     %client-session-changed client-71338 $1 target
    //     %layout-change @0 a1dd,60x20,0,0,0 a1dd,60x20,0,0,0 *
    // and the switched client receives `%session-changed $1 target` before its
    // own `%layout-change @1 aefe,101x41,0,0,1 ...`. The switched client, now
    // scoped to `target`, is never told about the source window.
    let handler = RequestHandler::new();
    let source = session_name("control-switch-notify-source");
    let target = session_name("control-switch-notify-target");
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
    let (_switching_id, mut switching_events) =
        register_control_client_with_id(&handler, switching_pid, &source).await;
    let (_surviving_id, mut surviving_events) =
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
    let source_window_id = active_window_id(&handler, &source).await;
    let source_layout_prefix = format!("%layout-change @{source_window_id} ");
    // The setup resizes publish asynchronously; wait for the streams to go
    // quiet so nothing from them is mistaken for a post-switch notification.
    settle_control_notifications(&mut switching_events).await;
    settle_control_notifications(&mut surviving_events).await;

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

    let surviving_lines =
        collect_control_notifications_through(&mut surviving_events, &source_layout_prefix).await;
    let layout_index = surviving_lines
        .iter()
        .position(|line| line.starts_with(&source_layout_prefix))
        .expect("the surviving control client must be told the source window shrank");
    assert_eq!(
        layout_change_geometry(&surviving_lines[layout_index]).as_deref(),
        Some("60x20"),
        "{:?}",
        surviving_lines[layout_index]
    );
    let session_changed_index = surviving_lines
        .iter()
        .position(|line| line.starts_with("%client-session-changed "))
        .expect("the surviving control client must be told the other client moved away");
    assert!(
        session_changed_index < layout_index,
        "tmux 3.7b reports the client move before the layout it causes: {surviving_lines:?}"
    );

    let target_window_id = active_window_id(&handler, &target).await;
    let target_layout_prefix = format!("%layout-change @{target_window_id} ");
    let switching_lines =
        collect_control_notifications_through(&mut switching_events, &target_layout_prefix).await;
    assert!(
        !switching_lines
            .iter()
            .any(|line| line.starts_with(&source_layout_prefix)),
        "the switched client left the source session and must not be told about its window: \
         {switching_lines:?}"
    );
    let target_layout_index = switching_lines
        .iter()
        .position(|line| line.starts_with(&target_layout_prefix))
        .expect("the switched client must be told its new window's layout");
    let own_session_changed_index = switching_lines
        .iter()
        .position(|line| line.starts_with("%session-changed "))
        .expect("the switched client must be told it changed session");
    assert!(
        own_session_changed_index < target_layout_index,
        "tmux 3.7b reports %session-changed before the switched client's own layout: \
         {switching_lines:?}"
    );
}

#[tokio::test]
async fn switching_attached_client_notifies_the_source_session_layout_change_like_tmux37() {
    // Frozen tmux 3.7b oracle, measured 2026-07-25 on a source session holding
    // a 101x41 PTY-attached client and a 60x20 control client, `window-size
    // largest`. After the PTY client runs `switch-client -t target` the source
    // window really shrinks to 60x20 and the surviving control client receives,
    // in this order:
    //     %client-session-changed /dev/ttys007 $1 target
    //     %layout-change @0 a1dd,60x20,0,0,0 a1dd,60x20,0,0,0 *
    let handler = RequestHandler::new();
    let source = session_name("attach-switch-notify-source");
    let target = session_name("attach-switch-notify-target");
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
    let (_attach_id, _attach_events) =
        register_attached_client(&handler, switching_pid, &source, switching_size).await;
    let (_surviving_id, mut surviving_events) =
        register_control_client_with_id(&handler, surviving_pid, &source).await;
    let response = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(surviving_pid, surviving_size),
        )))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(session_size(&handler, &source).await, switching_size);
    let source_window_id = active_window_id(&handler, &source).await;
    let source_layout_prefix = format!("%layout-change @{source_window_id} ");
    settle_control_notifications(&mut surviving_events).await;

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
        session_size(&handler, &source).await,
        surviving_size,
        "the source session must fall back to its surviving control client's geometry"
    );

    let lines =
        collect_control_notifications_through(&mut surviving_events, &source_layout_prefix).await;
    let layout_index = lines
        .iter()
        .position(|line| line.starts_with(&source_layout_prefix))
        .expect("the surviving control client must be told the source window shrank");
    assert_eq!(
        layout_change_geometry(&lines[layout_index]).as_deref(),
        Some("60x20"),
        "{:?}",
        lines[layout_index]
    );
    let session_changed_index = lines
        .iter()
        .position(|line| line.starts_with("%client-session-changed "))
        .expect("the surviving control client must be told the other client moved away");
    assert!(
        session_changed_index < layout_index,
        "tmux 3.7b reports the client move before the layout it causes: {lines:?}"
    );
}

/// Runs the destination half of an attached `switch-client`: the switching
/// client is the only one that changes session, and the control client that was
/// already sitting on the destination watches its window grow.
///
/// Frozen tmux 3.7b oracle, measured 2026-07-25
/// (`.rmux-audit/oracle/scenario_switch_destination.py`): with a 101x41 PTY
/// client on `source`, a 60x20 control client on `target`, `window-size
/// largest`, after `switch-client -c <tty> -t target` the destination window
/// grows 60x20 -> 101x41 and the destination control client receives
///     %client-session-changed /dev/ttys011 $1 target
///     %layout-change @1 aefe,101x41,0,0,1 aefe,101x41,0,0,1 *
/// in that order.
async fn assert_switch_notifies_the_destination_layout_change(
    handler: &RequestHandler,
    source: &SessionName,
    target: &SessionName,
    switch: impl AsyncFnOnce(&RequestHandler) -> Response,
) {
    set_window_size_policy(handler, source, "largest").await;
    set_window_size_policy(handler, target, "largest").await;

    let switching_pid = std::process::id();
    let destination_pid = switching_pid.saturating_add(3);
    let switching_size = TerminalSize {
        cols: 101,
        rows: 41,
    };
    let destination_size = TerminalSize { cols: 60, rows: 20 };
    let (_attach_id, _attach_events) =
        register_attached_client(handler, switching_pid, source, switching_size).await;
    let (_destination_id, mut destination_events) =
        register_control_client_with_id(handler, destination_pid, target).await;
    let response = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(destination_pid, destination_size),
        )))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(session_size(handler, target).await, destination_size);
    let target_window_id = active_window_id(handler, target).await;
    let target_layout_prefix = format!("%layout-change @{target_window_id} ");
    settle_control_notifications(&mut destination_events).await;

    let response = switch(handler).await;
    assert!(
        !matches!(response, Response::Error(_)),
        "the switch must succeed: {response:?}"
    );
    assert_eq!(
        session_size(handler, target).await,
        switching_size,
        "the destination window must grow to the arriving client's geometry"
    );

    let lines =
        collect_control_notifications_through(&mut destination_events, &target_layout_prefix).await;
    let layout_index = lines
        .iter()
        .position(|line| line.starts_with(&target_layout_prefix))
        .expect("the destination control client must be told its window grew");
    assert_eq!(
        layout_change_geometry(&lines[layout_index]).as_deref(),
        Some("101x41"),
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

#[tokio::test]
async fn switching_attached_client_notifies_the_destination_session_layout_change_like_tmux37() {
    let handler = RequestHandler::new();
    let source = session_name("attach-switch-dest-source");
    let target = session_name("attach-switch-dest-target");
    create_session(&handler, source.clone(), INITIAL_SIZE).await;
    create_session(&handler, target.clone(), TARGET_SIZE).await;
    let switch_target = target.clone();
    assert_switch_notifies_the_destination_layout_change(
        &handler,
        &source,
        &target,
        async |handler| {
            handler
                .handle(Request::SwitchClient(SwitchClientRequest {
                    target: switch_target,
                }))
                .await
        },
    )
    .await;
}

#[tokio::test]
async fn attach_session_from_an_attached_client_notifies_the_destination_layout_change_like_tmux37()
{
    // `attach-session` issued by an already-attached client re-enters the same
    // attached switch arm, so it owes the destination the same notification.
    let handler = RequestHandler::new();
    let source = session_name("attach-reattach-dest-source");
    let target = session_name("attach-reattach-dest-target");
    create_session(&handler, source.clone(), INITIAL_SIZE).await;
    create_session(&handler, target.clone(), TARGET_SIZE).await;
    let attach_target = target.clone();
    assert_switch_notifies_the_destination_layout_change(
        &handler,
        &source,
        &target,
        async |handler| {
            handler
                .handle(Request::AttachSession(rmux_proto::AttachSessionRequest {
                    target: attach_target,
                }))
                .await
        },
    )
    .await;
}

#[tokio::test]
async fn attaching_client_notifies_the_destination_session_layout_change_like_tmux37() {
    // Frozen tmux 3.7b oracle, measured 2026-07-25
    // (`.rmux-audit/oracle/scenario_attach_destination.py`): a 60x20 control
    // client already on `target`, `window-size largest`, watches a brand-new
    // 101x41 PTY client attach. The window grows 60x20 -> 101x41 and the
    // control client receives `%layout-change @0 aefd,101x41,0,0,0 ...`.
    let handler = RequestHandler::new();
    let target = session_name("attach-arrival-dest");
    create_session(&handler, target.clone(), TARGET_SIZE).await;
    set_window_size_policy(&handler, &target, "largest").await;

    let control_pid = std::process::id().saturating_add(5);
    let control_size = TerminalSize { cols: 60, rows: 20 };
    let arriving_size = TerminalSize {
        cols: 101,
        rows: 41,
    };
    let (_control_id, mut control_events) =
        register_control_client_with_id(&handler, control_pid, &target).await;
    let response = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(control_pid, control_size),
        )))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(session_size(&handler, &target).await, control_size);
    let target_window_id = active_window_id(&handler, &target).await;
    let target_layout_prefix = format!("%layout-change @{target_window_id} ");
    settle_control_notifications(&mut control_events).await;

    let response = handler
        .handle(Request::AttachSessionExt2(Box::new(
            rmux_proto::request::AttachSessionExt2Request {
                target: Some(target.clone()),
                target_spec: None,
                detach_other_clients: false,
                kill_other_clients: false,
                read_only: false,
                skip_environment_update: true,
                flags: None,
                working_directory: None,
                client_terminal: rmux_proto::ClientTerminalContext::default(),
                client_size: Some(arriving_size),
            },
        )))
        .await;
    assert!(
        matches!(response, Response::AttachSession(_)),
        "{response:?}"
    );
    assert_eq!(
        session_size(&handler, &target).await,
        arriving_size,
        "the attached window must grow to the arriving client's geometry"
    );

    let lines =
        collect_control_notifications_through(&mut control_events, &target_layout_prefix).await;
    let layout_index = lines
        .iter()
        .position(|line| line.starts_with(&target_layout_prefix))
        .expect("the control client must be told the attached window grew");
    assert_eq!(
        layout_change_geometry(&lines[layout_index]).as_deref(),
        Some("101x41"),
        "{:?}",
        lines[layout_index]
    );
}

#[tokio::test]
async fn attached_client_departure_notifies_the_surviving_control_layout_change_like_tmux37() {
    // Frozen tmux 3.7b oracle, measured 2026-07-25 on a session holding a
    // 101x41 PTY-attached client and a 60x20 control client, `window-size
    // largest`. When the PTY client's process dies, the control client
    // receives
    //     %client-detached /dev/ttys007
    //     %layout-change @0 a1dd,60x20,0,0,0 a1dd,60x20,0,0,0 *
    // in that order: tmux reports the loss immediately and the resize it
    // causes on the next server loop.
    let handler = RequestHandler::new();
    let session = session_name("attach-departure-notify");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    set_window_size_policy(&handler, &session, "largest").await;

    let departing_pid = std::process::id();
    let surviving_pid = departing_pid.saturating_add(1);
    let departing_size = TerminalSize {
        cols: 101,
        rows: 41,
    };
    let surviving_size = TerminalSize { cols: 60, rows: 20 };
    let (attach_id, _attach_events) =
        register_attached_client(&handler, departing_pid, &session, departing_size).await;
    let (_control_id, mut control_events) =
        register_control_client_with_id(&handler, surviving_pid, &session).await;
    let response = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(surviving_pid, surviving_size),
        )))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(session_size(&handler, &session).await, departing_size);
    let layout_prefix = format!(
        "%layout-change @{} ",
        active_window_id(&handler, &session).await
    );
    settle_control_notifications(&mut control_events).await;

    handler.finish_attach(departing_pid, attach_id).await;

    assert_eq!(
        session_size(&handler, &session).await,
        surviving_size,
        "the session must fall back to its surviving control client's geometry"
    );
    let lines = collect_control_notifications_through(&mut control_events, &layout_prefix).await;
    let layout_index = lines
        .iter()
        .position(|line| line.starts_with(&layout_prefix))
        .expect("the surviving control client must be told the window shrank");
    assert_eq!(
        layout_change_geometry(&lines[layout_index]).as_deref(),
        Some("60x20"),
        "{:?}",
        lines[layout_index]
    );
    let detached_index = lines
        .iter()
        .position(|line| line.starts_with("%client-detached "))
        .expect("the surviving control client must be told the other client left");
    assert!(
        detached_index < layout_index,
        "tmux 3.7b reports a lost client before the resize it causes: {lines:?}"
    );
}

#[tokio::test]
async fn detach_client_notifies_the_surviving_control_layout_change_like_tmux37() {
    // Same oracle session as the departure test, but the 101x41 client leaves
    // through `detach-client`. tmux 3.7b, measured 2026-07-25, then sends
    //     %layout-change @0 a1dd,60x20,0,0,0 a1dd,60x20,0,0,0 *
    //     %client-detached /dev/ttys007
    // - the opposite order, because the command queue applies the resize before
    // the client is actually lost.
    let handler = RequestHandler::new();
    let session = session_name("detach-client-notify");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    set_window_size_policy(&handler, &session, "largest").await;

    let detaching_pid = std::process::id();
    let surviving_pid = detaching_pid.saturating_add(1);
    let detaching_size = TerminalSize {
        cols: 101,
        rows: 41,
    };
    let surviving_size = TerminalSize { cols: 60, rows: 20 };
    let (_attach_id, _attach_events) =
        register_attached_client(&handler, detaching_pid, &session, detaching_size).await;
    let (_control_id, mut control_events) =
        register_control_client_with_id(&handler, surviving_pid, &session).await;
    let response = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(surviving_pid, surviving_size),
        )))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(session_size(&handler, &session).await, detaching_size);
    let layout_prefix = format!(
        "%layout-change @{} ",
        active_window_id(&handler, &session).await
    );
    settle_control_notifications(&mut control_events).await;

    // `detach-client` sends the terminal control message and then reconciles;
    // the client is only unregistered when its connection finishes, so the
    // reconcile has to see the departure itself.
    let response = handler
        .handle(Request::DetachClient(DetachClientRequest))
        .await;
    assert!(
        matches!(response, Response::DetachClient(_)),
        "{response:?}"
    );

    assert_eq!(
        session_size(&handler, &session).await,
        surviving_size,
        "the session must fall back to its surviving control client's geometry"
    );
    let lines = collect_control_notifications_through(&mut control_events, &layout_prefix).await;
    let layout_index = lines
        .iter()
        .position(|line| line.starts_with(&layout_prefix))
        .expect("the surviving control client must be told the window shrank");
    assert_eq!(
        layout_change_geometry(&lines[layout_index]).as_deref(),
        Some("60x20"),
        "{:?}",
        lines[layout_index]
    );
}

#[tokio::test]
async fn window_keyed_reconcile_notifies_control_clients_like_tmux37() {
    // The window-keyed reconcile is the one kill-window, kill-session,
    // link/move/swap/unlink-window and pane kill-by-id use for the windows that
    // survive them. tmux 3.7b's `resize_window()` notifies
    // `window-layout-changed` and then `window-resized` for every applied
    // resize, so a control client on that window always receives
    //     %layout-change @0 a1dd,60x20,0,0,0 a1dd,60x20,0,0,0 *
    // (measured 2026-07-25; identical line to the departure oracle, which
    // reaches the same 60x20 geometry).
    let handler = RequestHandler::new();
    let session = session_name("window-reconcile-notify");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    set_window_size_policy(&handler, &session, "largest").await;

    let control_pid = std::process::id();
    let control_size = TerminalSize { cols: 60, rows: 20 };
    let (_control_id, mut control_events) =
        register_control_client_with_id(&handler, control_pid, &session).await;
    let response = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(control_pid, control_size),
        )))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(session_size(&handler, &session).await, control_size);
    let layout_prefix = format!(
        "%layout-change @{} ",
        active_window_id(&handler, &session).await
    );

    // Stand in for the departed client whose geometry the window still carries.
    force_window_size(
        &handler,
        &session,
        0,
        TerminalSize {
            cols: 101,
            rows: 41,
        },
    )
    .await;
    settle_control_notifications(&mut control_events).await;

    handler
        .reconcile_attached_window_size_and_emit(&WindowTarget::with_window(session.clone(), 0))
        .await
        .expect("the window reconcile succeeds");

    assert_eq!(session_size(&handler, &session).await, control_size);
    let lines = collect_control_notifications_through(&mut control_events, &layout_prefix).await;
    let layout = lines
        .iter()
        .find(|line| line.starts_with(&layout_prefix))
        .expect("the control client must be told the window was resized");
    assert_eq!(
        layout_change_geometry(layout).as_deref(),
        Some("60x20"),
        "{layout}"
    );
}

#[tokio::test]
async fn destroyed_control_session_rehome_reconciles_target_geometry_like_tmux37() {
    let handler = RequestHandler::new();
    let target = session_name("control-destroy-rehome-target");
    let source = session_name("control-destroy-rehome-source");
    create_session(
        &handler,
        target.clone(),
        TerminalSize { cols: 60, rows: 20 },
    )
    .await;
    create_session(&handler, source.clone(), INITIAL_SIZE).await;
    set_window_size_policy(&handler, &target, "latest").await;
    set_detach_on_destroy(&handler, &source, "off").await;

    let requester_pid = std::process::id();
    let (_control_id, _control_events) =
        register_control_client_with_id(&handler, requester_pid, &source).await;
    let control_size = TerminalSize {
        cols: 101,
        rows: 41,
    };
    let refreshed = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(requester_pid, control_size),
        )))
        .await;
    assert!(
        matches!(refreshed, Response::RefreshClient(_)),
        "{refreshed:?}"
    );

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: source,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;

    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    assert_eq!(session_size(&handler, &target).await, control_size);
    let active_control = handler.active_control.lock().await;
    assert_eq!(
        active_control
            .by_pid
            .get(&requester_pid)
            .and_then(|active| active.session_name.as_ref()),
        Some(&target)
    );
}

#[tokio::test]
async fn destroyed_session_rehome_notifies_the_attached_destination_layout_change_like_tmux37() {
    // Frozen tmux 3.7b oracle, measured 2026-07-25
    // (`.rmux-audit/oracle/scenario_destroy_rehome_destination.py`): `doomed`
    // holds a 101x41 PTY client with `detach-on-destroy off`, `keep` holds a
    // 60x20 control client, `window-size largest`. Killing `doomed` rehomes the
    // PTY client onto `keep`, whose window grows 60x20 -> 101x41, and the
    // destination control client receives
    //     %client-session-changed /dev/ttys010 $0 keep
    //     ...
    //     %layout-change @0 aefd,101x41,0,0,0 aefd,101x41,0,0,0 *
    let handler = RequestHandler::new();
    let keep = session_name("rehome-dest-keep");
    let doomed = session_name("rehome-dest-doomed");
    create_session(&handler, keep.clone(), TARGET_SIZE).await;
    create_session(&handler, doomed.clone(), INITIAL_SIZE).await;
    set_window_size_policy(&handler, &keep, "largest").await;
    set_window_size_policy(&handler, &doomed, "largest").await;
    set_detach_on_destroy(&handler, &doomed, "off").await;

    let attach_pid = std::process::id().saturating_add(7);
    let control_pid = attach_pid.saturating_add(1);
    let rehomed_size = TerminalSize {
        cols: 101,
        rows: 41,
    };
    let destination_size = TerminalSize { cols: 60, rows: 20 };
    let (_attach_id, _attach_events) =
        register_attached_client(&handler, attach_pid, &doomed, rehomed_size).await;
    let (_control_id, mut control_events) =
        register_control_client_with_id(&handler, control_pid, &keep).await;
    let response = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(control_pid, destination_size),
        )))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(session_size(&handler, &keep).await, destination_size);
    let keep_window_id = active_window_id(&handler, &keep).await;
    let keep_layout_prefix = format!("%layout-change @{keep_window_id} ");
    settle_control_notifications(&mut control_events).await;

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: doomed,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    assert_eq!(
        session_size(&handler, &keep).await,
        rehomed_size,
        "the rehome destination must grow to the arriving client's geometry"
    );

    let lines =
        collect_control_notifications_through(&mut control_events, &keep_layout_prefix).await;
    let layout_index = lines
        .iter()
        .position(|line| line.starts_with(&keep_layout_prefix))
        .expect("the destination control client must be told the rehome grew its window");
    assert_eq!(
        layout_change_geometry(&lines[layout_index]).as_deref(),
        Some("101x41"),
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

/// Frozen tmux 3.7b oracle, 2026-07-25, `tmux -C attach-session` fed one
/// command per flush:
///
/// ```text
/// %begin 1785012377 305 1
/// %end 1785012377 305 1
/// %layout-change @0 c33d,130x46,0,0,0 c33d,130x46,0,0,0 *
/// ```
///
/// tmux flushes the notification for a resize its command applied before it
/// reads the next command, so a control frontend never has to send an
/// unrelated command to learn the new geometry. RMUX keeps that guarantee with
/// the applied-window-resize backstop, which used to run only for requests
/// arriving on a plain socket connection — a control client's own command
/// stream, a multi-command CLI invocation, a hook, a key binding and the
/// identity-checked choose-tree / web-share dispatch all bypass
/// `dispatch_for_connection`.
async fn assert_queued_command_publishes_a_pending_window_resize(
    session_suffix: &str,
    control_pid: u32,
    run_queue: impl AsyncFnOnce(&RequestHandler, u32, SessionName),
) {
    let handler = RequestHandler::new();
    let session = session_name(&format!("queued-resize-backstop-{session_suffix}"));
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    let (_control_id, mut events) =
        register_control_client_with_id(&handler, control_pid, &session).await;
    let _ = settle_control_notifications(&mut events).await;

    // A geometry write that reached the chokepoint without publishing at its
    // own ordering point: exactly what the backstop exists to catch.
    force_window_size(&handler, &session, 0, TARGET_SIZE).await;

    run_queue(&handler, control_pid, session.clone()).await;

    let lines = settle_control_notifications(&mut events).await;
    let geometry = lines
        .iter()
        .filter(|line| line.starts_with("%layout-change"))
        .filter_map(|line| layout_change_geometry(line))
        .collect::<Vec<_>>();
    assert_eq!(
        geometry,
        vec![format!("{}x{}", TARGET_SIZE.cols, TARGET_SIZE.rows)],
        "the queued command must publish the pending resize exactly once; got {lines:?}"
    );
    assert!(
        handler
            .state
            .lock()
            .await
            .take_applied_window_resizes()
            .is_empty(),
        "the queued command must leave the geometry queue empty"
    );
}

#[tokio::test]
async fn a_control_queue_command_publishes_a_pending_applied_window_resize() {
    assert_queued_command_publishes_a_pending_window_resize(
        "control",
        92_800,
        async |handler: &RequestHandler, control_pid: u32, _session: SessionName| {
            let parsed = handler
                .parse_control_commands("list-sessions")
                .await
                .expect("list-sessions parses");
            let result = handler.execute_control_commands(control_pid, parsed).await;
            assert!(result.error.is_none(), "{:?}", result.error);
        },
    )
    .await;
}

#[tokio::test]
async fn a_detached_queue_command_publishes_a_pending_applied_window_resize() {
    // The detached queue is what runs a multi-command CLI invocation, a hook
    // body and a key binding, none of which reach `dispatch_for_connection`.
    assert_queued_command_publishes_a_pending_window_resize(
        "detached",
        92_810,
        async |handler: &RequestHandler, _control_pid: u32, _session: SessionName| {
            let parsed = handler
                .parse_control_commands("list-sessions")
                .await
                .expect("list-sessions parses");
            handler
                .execute_parsed_commands_for_test(92_811, parsed)
                .await
                .expect("list-sessions succeeds");
        },
    )
    .await;
}

#[tokio::test]
async fn an_identity_checked_dispatch_publishes_a_pending_applied_window_resize() {
    // choose-tree's kill actions (handler_mode_tree/tree_kill.rs) and the
    // web-share request path run their hooks through `finish_identity_dispatch`
    // instead of `dispatch_for_connection`.
    assert_queued_command_publishes_a_pending_window_resize(
        "identity",
        92_820,
        async |handler: &RequestHandler, control_pid: u32, session: SessionName| {
            let session_id = handler
                .state
                .lock()
                .await
                .sessions
                .session(&session)
                .expect("session remains present")
                .id();
            let response = crate::handler::dispatch_with_expected_session_identity(
                handler,
                control_pid,
                session.clone(),
                session_id,
                Request::HasSession(rmux_proto::HasSessionRequest { target: session }),
            )
            .await;
            assert!(matches!(response, Response::HasSession(_)), "{response:?}");
        },
    )
    .await;
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

async fn set_detach_on_destroy(handler: &RequestHandler, session: &SessionName, value: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(session.clone()),
            option: OptionName::DetachOnDestroy,
            value: value.to_owned(),
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

/// Puts a window back at a geometry no live client asks for, standing in for
/// the client whose departure the reconcile has to notice.
async fn force_window_size(
    handler: &RequestHandler,
    session: &SessionName,
    window_index: u32,
    size: TerminalSize,
) {
    let mut state = handler.state.lock().await;
    state
        .mutate_session_and_resize_window_terminal(session, window_index, |session| {
            session.resize_window(window_index, size)
        })
        .expect("test window resize succeeds");
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

async fn active_window_id(handler: &RequestHandler, session: &SessionName) -> u32 {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(session)
        .expect("session remains present")
        .window()
        .id()
        .as_u32()
}

/// `%layout-change @<id> <layout> <visible layout> <flags>` carries the window
/// geometry in the second comma-separated field of the layout cell.
fn layout_change_geometry(line: &str) -> Option<String> {
    line.split_whitespace()
        .nth(2)
        .and_then(|layout| layout.split(',').nth(1))
        .map(ToOwned::to_owned)
}

/// Collects the notifications in delivery order up to and including the first
/// line starting with `prefix`, so a test can assert on their relative order.
async fn collect_control_notifications_through(
    events: &mut mpsc::Receiver<ControlServerEvent>,
    prefix: &str,
) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + CONTROL_NOTIFICATION_TIMEOUT;
    let mut lines = Vec::new();
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(CONTROL_NOTIFICATION_POLL, events.recv()).await {
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

/// Collects everything the client receives until its stream stays quiet for
/// [`CONTROL_NOTIFICATION_SETTLE`].
async fn settle_control_notifications(
    events: &mut mpsc::Receiver<ControlServerEvent>,
) -> Vec<String> {
    let mut deadline = tokio::time::Instant::now() + CONTROL_NOTIFICATION_SETTLE;
    let mut lines = Vec::new();
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(CONTROL_NOTIFICATION_POLL, events.recv()).await {
            Ok(Some(event)) => {
                deadline = tokio::time::Instant::now() + CONTROL_NOTIFICATION_SETTLE;
                if let ControlServerEvent::Notification(line) = event {
                    lines.push(line);
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    lines
}
