use super::RequestHandler;
use crate::pane_io::PaneExitEvent;
use rmux_core::{PaneId, WINLINK_SILENCE};
use rmux_proto::{
    BreakPaneRequest, KillPaneRequest, LinkWindowRequest, NewSessionExtRequest, NewWindowRequest,
    OptionName, PaneKillRequest, PaneTarget, PaneTargetRef, Request, Response, ScopeSelector,
    SessionName, SetOptionMode, SetOptionRequest, SplitDirection, SplitWindowExtRequest,
    SplitWindowTarget, TerminalSize, WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

#[cfg(unix)]
fn quiet_command() -> Vec<String> {
    ["/bin/sh", "-c", "sleep 60"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[cfg(windows)]
fn quiet_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
    ]
}

async fn create_quiet_session(handler: &RequestHandler, value: &str) -> SessionName {
    let session = session_name(value);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::new(session.clone(), 0))
        .await;
    session
}

async fn create_grouped_session(
    handler: &RequestHandler,
    value: &str,
    group_target: &SessionName,
) -> SessionName {
    let session = session_name(value);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: Some(group_target.clone()),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    session
}

async fn create_quiet_window(handler: &RequestHandler, session: &SessionName) -> WindowTarget {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: Some(quiet_command()),
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let Response::NewWindow(response) = response else {
        panic!("expected new-window response, got {response:?}");
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
            session.clone(),
            response.target.window_index(),
            0,
        ))
        .await;
    response.target
}

async fn split_quiet_window(
    handler: &RequestHandler,
    window: &WindowTarget,
) -> (PaneTarget, PaneId) {
    let response = handler
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Pane(PaneTarget::with_window(
                window.session_name().clone(),
                window.window_index(),
                0,
            )),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
            command: Some(quiet_command()),
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: true,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        })))
        .await;
    let Response::SplitWindow(response) = response else {
        panic!("expected split-window response, got {response:?}");
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&response.pane)
        .await;
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(response.pane.session_name())
            .and_then(|session| session.window_at(response.pane.window_index()))
            .and_then(|window| window.pane(response.pane.pane_index()))
            .map(|pane| pane.id())
            .expect("split pane exists")
    };
    (response.pane, pane_id)
}

async fn set_monitor_silence(handler: &RequestHandler, scope: ScopeSelector) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope,
            option: OptionName::MonitorSilence,
            value: "60".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn setup_non_last_pane_case(
    handler: &RequestHandler,
    value: &str,
) -> (SessionName, WindowTarget, PaneTarget, PaneId) {
    let session = create_quiet_session(handler, value).await;
    let monitored = WindowTarget::with_window(session.clone(), 0);
    let second_window = create_quiet_window(handler, &session).await;
    let (split_pane, pane_id) = split_quiet_window(handler, &second_window).await;
    set_monitor_silence(handler, ScopeSelector::Window(monitored.clone())).await;
    (session, monitored, split_pane, pane_id)
}

fn timer_snapshot(handler: &RequestHandler, target: &WindowTarget) -> (u64, tokio::time::Instant) {
    handler
        .silence_timer_snapshot_for_test(target)
        .expect("monitored window has a silence timer")
}

#[tokio::test]
async fn direct_non_last_pane_kill_preserves_other_window_silence_deadline() {
    let handler = RequestHandler::new();
    let (_session, monitored, split_pane, _pane_id) =
        setup_non_last_pane_case(&handler, "silence-direct-pane-kill").await;
    let before = timer_snapshot(&handler, &monitored);

    let response = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: split_pane,
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    assert_eq!(timer_snapshot(&handler, &monitored), before);
}

#[tokio::test]
async fn pane_id_non_last_kill_preserves_other_window_silence_deadline() {
    let handler = RequestHandler::new();
    let (session, monitored, _split_pane, pane_id) =
        setup_non_last_pane_case(&handler, "silence-pane-id-kill").await;
    let before = timer_snapshot(&handler, &monitored);

    let response = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(session, pane_id),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    assert_eq!(timer_snapshot(&handler, &monitored), before);
}

#[tokio::test]
async fn natural_non_last_pane_exit_preserves_other_window_silence_deadline() {
    let handler = RequestHandler::new();
    let (session, monitored, split_pane, pane_id) =
        setup_non_last_pane_case(&handler, "silence-natural-pane-exit").await;
    let before = timer_snapshot(&handler, &monitored);
    {
        let mut state = handler.state.lock().await;
        state
            .mark_pane_dead_without_exit_details(&split_pane)
            .expect("mark pane naturally exited");
    }

    handler
        .handle_pane_exit_event(PaneExitEvent::eof_published(session, pane_id, None))
        .await;

    assert_eq!(timer_snapshot(&handler, &monitored), before);
}

#[tokio::test]
async fn last_pane_window_kill_removes_grouped_alias_timers_and_preserves_survivors() {
    let handler = RequestHandler::new();
    let owner = create_quiet_session(&handler, "silence-last-pane-owner").await;
    let removed_owner = create_quiet_window(&handler, &owner).await;
    let peer = create_grouped_session(&handler, "silence-last-pane-peer", &owner).await;
    let survivor_owner = WindowTarget::with_window(owner.clone(), 0);
    let survivor_peer = WindowTarget::with_window(peer.clone(), 0);
    let removed_peer = WindowTarget::with_window(peer.clone(), removed_owner.window_index());
    set_monitor_silence(&handler, ScopeSelector::Global).await;
    let survivor_owner_before = timer_snapshot(&handler, &survivor_owner);
    let survivor_peer_before = timer_snapshot(&handler, &survivor_peer);
    assert!(handler
        .silence_timer_snapshot_for_test(&removed_owner)
        .is_some());
    assert!(handler
        .silence_timer_snapshot_for_test(&removed_peer)
        .is_some());

    let response = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(owner.clone(), removed_owner.window_index(), 0),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    assert_eq!(
        handler.silence_timer_snapshot_for_test(&removed_owner),
        None
    );
    assert_eq!(handler.silence_timer_snapshot_for_test(&removed_peer), None);
    assert_eq!(
        timer_snapshot(&handler, &survivor_owner),
        survivor_owner_before
    );
    assert_eq!(
        timer_snapshot(&handler, &survivor_peer),
        survivor_peer_before
    );
}

#[tokio::test]
async fn break_last_pane_across_sessions_preserves_silence_deadline_and_identity() {
    let handler = RequestHandler::new();
    let source_session = create_quiet_session(&handler, "silence-break-source").await;
    let destination_session = create_quiet_session(&handler, "silence-break-destination").await;
    let source = WindowTarget::with_window(source_session.clone(), 0);
    let unrelated = WindowTarget::with_window(destination_session.clone(), 0);
    set_monitor_silence(&handler, ScopeSelector::Global).await;

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(121);
    handler.replace_silence_timer_deadline_for_test(&source, deadline);
    let source_before = timer_snapshot(&handler, &source);
    let source_identity_before = handler
        .silence_timer_identity_for_test(&source)
        .expect("source timer has stable identity");
    let unrelated_before = timer_snapshot(&handler, &unrelated);
    let destination_session_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&destination_session)
            .expect("destination session exists")
            .id()
    };

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(source_session.clone(), 0, 0),
            target: Some(WindowTarget::with_window(destination_session.clone(), 1)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected cross-session break-pane success, got {response:?}");
    };
    let destination = WindowTarget::with_window(destination_session, 1);
    assert_eq!(
        response.target,
        PaneTarget::with_window(destination.session_name().clone(), 1, 0)
    );

    assert_eq!(handler.silence_timer_snapshot_for_test(&source), None);
    let destination_after = timer_snapshot(&handler, &destination);
    assert_eq!(destination_after.1, source_before.1);
    assert!(destination_after.0 > source_before.0);
    let destination_identity = handler
        .silence_timer_identity_for_test(&destination)
        .expect("destination timer keeps the moved identity");
    assert_eq!(destination_identity.0, destination_session_id);
    assert_eq!(destination_identity.1, source_identity_before.1);
    assert!(destination_identity.2 > source_identity_before.2);
    assert_eq!(timer_snapshot(&handler, &unrelated), unrelated_before);
}

async fn assert_expired_single_pane_break_does_not_rearm(
    label: &str,
    attach_source: bool,
    expect_silence_flag: bool,
) {
    let handler = RequestHandler::new();
    let source_session = create_quiet_session(&handler, &format!("{label}-source")).await;
    let source = WindowTarget::with_window(source_session.clone(), 0);
    let owner = create_quiet_session(&handler, &format!("{label}-owner")).await;
    let peer = create_grouped_session(&handler, &format!("{label}-peer"), &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    set_monitor_silence(&handler, ScopeSelector::Global).await;
    let _attached_control = if attach_source {
        let (control_tx, control_rx) = tokio::sync::mpsc::unbounded_channel();
        let _attach_id = handler
            .register_attach(918, source_session.clone(), control_tx)
            .await;
        Some(control_rx)
    } else {
        None
    };

    let expired_identity = handler
        .silence_timer_identity_for_test(&source)
        .expect("source timer is armed before expiry");
    handler
        .expire_silence_timer_for_test(
            source.clone(),
            expired_identity.0,
            expired_identity.1,
            expired_identity.2,
        )
        .await;
    assert_eq!(handler.silence_timer_snapshot_for_test(&source), None);
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&source_session)
                .expect("source session exists before break")
                .winlink_alert_flags(0)
                .contains(WINLINK_SILENCE),
            expect_silence_flag,
            "source winlink silence persistence must match its attached/current state"
        );
    }

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(source_session.clone(), 0, 0),
            target: Some(WindowTarget::with_window(owner.clone(), 1)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected expired cross-session break success, got {response:?}");
    };
    assert_eq!(
        response.target,
        PaneTarget::with_window(owner.clone(), 1, 0)
    );

    assert_eq!(handler.silence_timer_snapshot_for_test(&source), None);
    let destinations = [
        WindowTarget::with_window(owner, 1),
        WindowTarget::with_window(peer, 1),
    ];
    let state = handler.state.lock().await;
    assert!(state.sessions.session(&source_session).is_none());
    for destination in &destinations {
        let session = state
            .sessions
            .session(destination.session_name())
            .expect("destination group member survives");
        assert_eq!(
            session
                .window_at(destination.window_index())
                .expect("moved window exists in every destination group member")
                .id(),
            expired_identity.1
        );
        assert_eq!(
            session
                .winlink_alert_flags(destination.window_index())
                .contains(WINLINK_SILENCE),
            expect_silence_flag,
            "moved alias {destination} must preserve the source winlink flag state"
        );
        assert_eq!(
            handler.silence_timer_snapshot_for_test(destination),
            None,
            "structural break must not rearm already-fired alias {destination}"
        );
    }
}

#[tokio::test]
async fn expired_single_pane_break_moves_silence_without_rearming() {
    assert_expired_single_pane_break_does_not_rearm("silence-break-expired", false, true).await;
}

#[tokio::test]
async fn attached_current_expired_single_pane_break_does_not_rearm_without_flag() {
    assert_expired_single_pane_break_does_not_rearm("silence-break-attached-expired", true, false)
        .await;
}

#[tokio::test]
async fn grouped_break_reorders_distinct_duplicate_alias_silence_deadlines() {
    let handler = RequestHandler::new();
    let owner = create_quiet_session(&handler, "silence-break-duplicate-owner").await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(owner.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    let peer = create_grouped_session(&handler, "silence-break-duplicate-peer", &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    set_monitor_silence(&handler, ScopeSelector::Global).await;

    let targets = [
        WindowTarget::with_window(owner.clone(), 0),
        WindowTarget::with_window(owner.clone(), 1),
        WindowTarget::with_window(peer.clone(), 0),
        WindowTarget::with_window(peer.clone(), 1),
    ];
    let now = tokio::time::Instant::now();
    for (offset, target) in targets.iter().enumerate() {
        handler.replace_silence_timer_deadline_for_test(
            target,
            now + tokio::time::Duration::from_secs(121 + offset as u64),
        );
    }
    let before = targets
        .clone()
        .map(|target| timer_snapshot(&handler, &target));

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 1, 0),
            target: Some(WindowTarget::with_window(owner.clone(), 0)),
            name: None,
            detached: true,
            after: false,
            before: true,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected duplicate-alias break-pane success, got {response:?}");
    };
    assert_eq!(response.target, PaneTarget::with_window(owner, 0, 0));

    for (target, expected) in [
        (&targets[0], before[1]),
        (&targets[1], before[0]),
        (&targets[2], before[3]),
        (&targets[3], before[2]),
    ] {
        let after = timer_snapshot(&handler, target);
        assert_eq!(after.1, expected.1, "deadline follows {target}");
        assert!(after.0 > expected.0, "generation advances for {target}");
    }
}

#[tokio::test]
async fn cross_session_multi_pane_break_preserves_shifted_duplicate_alias_deadlines() {
    let handler = RequestHandler::new();
    let source_session = create_quiet_session(&handler, "silence-break-multi-source").await;
    let source = WindowTarget::with_window(source_session.clone(), 0);
    let (moved_pane, _) = split_quiet_window(&handler, &source).await;
    let owner = create_quiet_session(&handler, "silence-break-multi-owner").await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(owner.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    let peer = create_grouped_session(&handler, "silence-break-multi-peer", &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    set_monitor_silence(&handler, ScopeSelector::Global).await;

    let old_targets = [
        source.clone(),
        WindowTarget::with_window(owner.clone(), 0),
        WindowTarget::with_window(owner.clone(), 1),
        WindowTarget::with_window(peer.clone(), 0),
        WindowTarget::with_window(peer.clone(), 1),
    ];
    let base_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(121);
    for (offset, target) in old_targets.iter().enumerate() {
        handler.replace_silence_timer_deadline_for_test(
            target,
            base_deadline + tokio::time::Duration::from_secs(offset as u64 * 7),
        );
    }
    let before = old_targets
        .clone()
        .map(|target| timer_snapshot(&handler, &target));

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: moved_pane,
            target: Some(WindowTarget::with_window(owner.clone(), 0)),
            name: None,
            detached: true,
            after: false,
            before: true,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected multi-pane cross-session break success, got {response:?}");
    };
    assert_eq!(
        response.target,
        PaneTarget::with_window(owner.clone(), 0, 0)
    );

    assert_eq!(timer_snapshot(&handler, &source), before[0]);
    for (target, expected) in [
        (WindowTarget::with_window(owner.clone(), 1), before[1]),
        (WindowTarget::with_window(owner.clone(), 2), before[2]),
        (WindowTarget::with_window(peer.clone(), 1), before[3]),
        (WindowTarget::with_window(peer.clone(), 2), before[4]),
    ] {
        let after = timer_snapshot(&handler, &target);
        assert_eq!(after.1, expected.1, "deadline follows {target}");
        assert!(after.0 > expected.0, "generation advances for {target}");
    }

    let inserted_owner = WindowTarget::with_window(owner, 0);
    let inserted_peer = WindowTarget::with_window(peer, 0);
    let old_deadlines = before.map(|snapshot| snapshot.1);
    for target in [inserted_owner, inserted_peer] {
        let inserted = timer_snapshot(&handler, &target);
        assert!(
            !old_deadlines.contains(&inserted.1),
            "newly-created {target} must arm a fresh silence deadline"
        );
    }
}

#[tokio::test]
async fn cross_session_single_pane_break_preserves_moved_and_shifted_alias_deadlines() {
    let handler = RequestHandler::new();
    let source_session = create_quiet_session(&handler, "silence-break-linked-source").await;
    let source = WindowTarget::with_window(source_session.clone(), 0);
    let owner = create_quiet_session(&handler, "silence-break-linked-owner").await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: source.clone(),
            target: WindowTarget::with_window(owner.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    let peer = create_grouped_session(&handler, "silence-break-linked-peer", &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    set_monitor_silence(&handler, ScopeSelector::Global).await;

    let old_targets = [
        source.clone(),
        WindowTarget::with_window(owner.clone(), 0),
        WindowTarget::with_window(owner.clone(), 1),
        WindowTarget::with_window(peer.clone(), 0),
        WindowTarget::with_window(peer.clone(), 1),
    ];
    let base_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(121);
    for (offset, target) in old_targets.iter().enumerate() {
        handler.replace_silence_timer_deadline_for_test(
            target,
            base_deadline + tokio::time::Duration::from_secs(offset as u64 * 7),
        );
    }
    let before = old_targets
        .clone()
        .map(|target| timer_snapshot(&handler, &target));

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(source_session, 0, 0),
            target: Some(WindowTarget::with_window(owner.clone(), 1)),
            name: None,
            detached: true,
            after: false,
            before: true,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected linked cross-session break success, got {response:?}");
    };
    assert_eq!(
        response.target,
        PaneTarget::with_window(owner.clone(), 1, 0)
    );
    assert_eq!(handler.silence_timer_snapshot_for_test(&source), None);

    for (target, expected) in [
        (WindowTarget::with_window(owner.clone(), 1), before[0]),
        (WindowTarget::with_window(owner.clone(), 2), before[2]),
        (WindowTarget::with_window(peer.clone(), 2), before[4]),
    ] {
        let after = timer_snapshot(&handler, &target);
        assert_eq!(after.1, expected.1, "deadline follows {target}");
        assert!(after.0 > expected.0, "generation advances for {target}");
    }
    assert_eq!(
        timer_snapshot(&handler, &WindowTarget::with_window(owner.clone(), 0)),
        before[1]
    );
    assert_eq!(
        timer_snapshot(&handler, &WindowTarget::with_window(peer.clone(), 0)),
        before[3]
    );

    let inserted_peer = timer_snapshot(&handler, &WindowTarget::with_window(peer, 1));
    assert_eq!(
        inserted_peer.1, before[0].1,
        "the new non-syntactic group alias inherits the addressed source deadline"
    );
    assert!(inserted_peer.0 > before[0].0);
}
