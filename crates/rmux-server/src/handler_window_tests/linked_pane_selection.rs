use super::*;

struct LinkedPaneFixture {
    owner: SessionName,
    grouped_peer: SessionName,
    linked_peer: SessionName,
    pane_one_id: rmux_proto::PaneId,
}

async fn linked_two_pane_fixture(handler: &RequestHandler, label: &str) -> LinkedPaneFixture {
    let owner = session_name(&format!("{label}-owner"));
    let grouped_peer = session_name(&format!("{label}-grouped"));
    let linked_peer = session_name(&format!("{label}-linked"));
    create_session(handler, owner.as_str()).await;
    create_grouped_session(handler, grouped_peer.as_str(), &owner).await;

    // The default command is an interactive shell whose startup output is
    // unbounded in time. Split the quiet command every other fixture pane runs
    // so this window stops producing pane activity once its panes are up.
    let split = handler
        .handle(Request::SplitWindowExt(Box::new(
            rmux_proto::SplitWindowExtRequest {
                target: SplitWindowTarget::Session(owner.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
                command: Some(quiet_window_test_command()),
                process_command: None,
                start_directory: None,
                keep_alive_on_exit: None,
                detached: false,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            },
        )))
        .await;
    let Response::SplitWindow(split) = split else {
        panic!("expected split-window response, got {split:?}");
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&split.pane)
        .await;

    create_session(handler, linked_peer.as_str()).await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(linked_peer.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");

    let pane_one_id = {
        let mut state = handler.state.lock().await;
        for target in [
            WindowTarget::with_window(owner.clone(), 0),
            WindowTarget::with_window(grouped_peer.clone(), 0),
            WindowTarget::with_window(linked_peer.clone(), 1),
        ] {
            state
                .sessions
                .session_setup(target.session_name())
                .expect("fixture session exists")
                .select_pane_in_window(target.window_index(), 0)
                .expect("fixture pane zero selection succeeds");
        }
        state
            .sessions
            .session(&owner)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(1))
            .expect("fixture pane one exists")
            .id()
    };

    let fixture = LinkedPaneFixture {
        owner,
        grouped_peer,
        linked_peer,
        pane_one_id,
    };
    settle_linked_fixture_activity(handler, &fixture).await;
    fixture
}

/// Waits until the fixture's shared window stops recording pane activity.
///
/// The fixture runs real pane processes, and output from a pane that is not its
/// session's active pane refreshes every attached client of the linked family:
/// `pane_alert_callback` coalesces such output for 50 ms and then calls
/// `refresh_attached_session` once per family member, which reaches every client
/// of those sessions. That refresh is correct and independent of the command
/// under test, so a client's control channel only reports what a command sent
/// once the panes behind it are quiet. Draining the channels is not enough on
/// its own: the drain stops after a fixed idle window while pane startup can
/// still be running.
async fn settle_linked_fixture_activity(handler: &RequestHandler, fixture: &LinkedPaneFixture) {
    const POLL_INTERVAL: Duration = Duration::from_millis(25);
    const STABLE_FOR: Duration = Duration::from_millis(300);
    const SETTLE_TIMEOUT: Duration = Duration::from_secs(15);

    let target = WindowTarget::with_window(fixture.owner.clone(), 0);
    let deadline = tokio::time::Instant::now() + SETTLE_TIMEOUT;
    let mut previous = linked_fixture_activity(handler, &target).await;
    let mut stable_since = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let current = linked_fixture_activity(handler, &target).await;
        let now = tokio::time::Instant::now();
        if current == previous {
            if now.duration_since(stable_since) >= STABLE_FOR {
                return;
            }
        } else {
            previous = current;
            stable_since = now;
        }
        assert!(
            now < deadline,
            "linked fixture pane activity did not settle for {target}: {previous:?}"
        );
    }
}

async fn linked_fixture_activity(handler: &RequestHandler, target: &WindowTarget) -> Vec<i64> {
    let state = handler.state.lock().await;
    let Some(window) = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
    else {
        return Vec::new();
    };
    std::iter::once(window.activity_at())
        .chain(window.panes().iter().map(rmux_core::Pane::activity_at))
        .collect()
}

async fn assert_linked_active_pane(
    handler: &RequestHandler,
    fixture: &LinkedPaneFixture,
    expected: u32,
) {
    let state = handler.state.lock().await;
    for target in [
        WindowTarget::with_window(fixture.owner.clone(), 0),
        WindowTarget::with_window(fixture.grouped_peer.clone(), 0),
        WindowTarget::with_window(fixture.linked_peer.clone(), 1),
    ] {
        assert_eq!(
            state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .expect("linked window alias exists")
                .active_pane_index(),
            expected,
            "active pane diverged for {target}"
        );
    }
}

#[tokio::test]
async fn select_pane_synchronizes_linked_and_grouped_window_aliases() {
    let handler = RequestHandler::new();
    let fixture = linked_two_pane_fixture(&handler, "select-linked").await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(7_091, fixture.grouped_peer.clone(), control_tx)
        .await;
    drain_attach_controls(&mut control_rx).await;

    let response = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::with_window(fixture.owner.clone(), 0, 1),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(response, Response::SelectPane(_)), "{response:?}");
    assert_linked_active_pane(&handler, &fixture, 1).await;

    let refreshed = timeout(Duration::from_secs(2), control_rx.recv())
        .await
        .expect("attached grouped alias refresh is bounded")
        .expect("attached grouped alias remains registered");
    assert!(
        matches!(refreshed, AttachControl::Switch(_)),
        "{refreshed:?}"
    );
}

#[tokio::test]
async fn focus_aggregation_uses_linked_runtime_instead_of_session_alias() {
    let handler = RequestHandler::new();
    let fixture = linked_two_pane_fixture(&handler, "focus-linked").await;
    let owner_pid = std::process::id();
    let grouped_pid = owner_pid.saturating_add(1);
    let pane = PaneTarget::with_window(fixture.owner.clone(), 0, 0);
    let grouped_pane = PaneTarget::with_window(fixture.grouped_peer.clone(), 0, 0);
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&fixture.owner, 0, 0, b"\x1b[?1004h")
            .expect("shared pane enables focus reporting");
    }
    let (owner_tx, _owner_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(owner_pid, fixture.owner.clone(), owner_tx)
        .await;
    let (grouped_tx, _grouped_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(grouped_pid, fixture.grouped_peer.clone(), grouped_tx)
        .await;
    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&pane);
        state.start_pane_input_capture_for_test(&grouped_pane);
    }

    handler
        .handle_attached_live_input_for_test(owner_pid, b"\x1b[O")
        .await
        .expect("first alias focus-out succeeds");
    {
        let state = handler.state.lock().await;
        assert_eq!(state.pane_input_capture_for_test(&pane), Some(Vec::new()));
        assert_eq!(
            state.pane_input_capture_for_test(&grouped_pane),
            Some(Vec::new())
        );
    }
    handler
        .handle_attached_live_input_for_test(grouped_pid, b"\x1b[O")
        .await
        .expect("last alias focus-out succeeds");
    handler
        .handle_attached_live_input_for_test(owner_pid, b"\x1b[I")
        .await
        .expect("first alias focus-in succeeds");
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(&pane),
            Some(b"\x1b[I".to_vec())
        );
        assert_eq!(
            state.pane_input_capture_for_test(&grouped_pane),
            Some(b"\x1b[O".to_vec())
        );
        state.start_pane_input_capture_for_test(&pane);
        state.start_pane_input_capture_for_test(&grouped_pane);
    }

    let switched = handler
        .handle_switch_client_ext3(
            owner_pid,
            rmux_proto::request::SwitchClientExt3Request {
                target_client: None,
                target: Some(format!("{}:0.0", fixture.grouped_peer)),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: true,
                zoom: false,
            },
        )
        .await;
    assert!(
        matches!(switched, Response::SwitchClient(_)),
        "{switched:?}"
    );
    let state = handler.state.lock().await;
    assert_eq!(
        state.pane_input_capture_for_test(&pane),
        Some(Vec::new()),
        "switching a focused client between aliases of one runtime must not emit O,I"
    );
    assert_eq!(
        state.pane_input_capture_for_test(&grouped_pane),
        Some(Vec::new()),
        "the same-runtime alias switch must be silent on every route"
    );
}

#[tokio::test]
async fn sdk_select_synchronizes_linked_and_grouped_window_aliases() {
    let handler = RequestHandler::new();
    let fixture = linked_two_pane_fixture(&handler, "sdk-select-linked").await;

    let response = handler
        .handle(Request::PaneSelect(PaneSelectRequest {
            target: PaneTargetRef::by_id(fixture.linked_peer.clone(), fixture.pane_one_id),
            title: None,
        }))
        .await;
    assert!(matches!(response, Response::SelectPane(_)), "{response:?}");
    assert_linked_active_pane(&handler, &fixture, 1).await;
}

#[tokio::test]
async fn adjacent_select_synchronizes_linked_and_grouped_window_aliases() {
    let handler = RequestHandler::new();
    let fixture = linked_two_pane_fixture(&handler, "adjacent-linked").await;

    let response = handler
        .handle(Request::SelectPaneAdjacent(SelectPaneAdjacentRequest {
            target: PaneTarget::with_window(fixture.owner.clone(), 0, 0),
            direction: SelectPaneDirection::Down,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(response, Response::SelectPane(_)), "{response:?}");
    assert_linked_active_pane(&handler, &fixture, 1).await;
}

#[tokio::test]
async fn last_pane_synchronizes_linked_and_grouped_window_aliases() {
    let handler = RequestHandler::new();
    let fixture = linked_two_pane_fixture(&handler, "last-linked").await;

    let response = handler
        .handle(Request::LastPane(LastPaneRequest {
            target: WindowTarget::with_window(fixture.owner.clone(), 0),
            preserve_zoom: false,
            input_disabled: None,
        }))
        .await;
    assert!(matches!(response, Response::LastPane(_)), "{response:?}");
    assert_linked_active_pane(&handler, &fixture, 1).await;
}

#[tokio::test]
async fn attached_mouse_focus_synchronizes_linked_and_grouped_window_aliases() {
    let handler = RequestHandler::new();
    let fixture = linked_two_pane_fixture(&handler, "mouse-select-linked").await;
    let requester_pid = std::process::id();
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, fixture.owner.clone(), control_tx)
        .await;
    let identity = handler.active_attach_identity_for_test(requester_pid).await;
    let (session_id, window_id) = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&fixture.owner)
            .expect("fixture owner exists");
        (session.id(), session.window().id().as_u32())
    };

    handler
        .select_attached_mouse_focus(
            identity,
            &fixture.owner,
            session_id,
            window_id,
            fixture.pane_one_id,
        )
        .await
        .expect("attached mouse focus succeeds");
    assert_linked_active_pane(&handler, &fixture, 1).await;
}

#[tokio::test]
async fn switch_client_pane_target_synchronizes_linked_and_grouped_window_aliases() {
    let handler = RequestHandler::new();
    let fixture = linked_two_pane_fixture(&handler, "switch-select-linked").await;
    let requester_pid = std::process::id();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, fixture.owner.clone(), control_tx)
        .await;
    let (peer_control_tx, mut peer_control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(
            requester_pid.saturating_add(1),
            fixture.grouped_peer.clone(),
            peer_control_tx,
        )
        .await;
    drain_attach_control_pair(&mut control_rx, &mut peer_control_rx).await;

    let response = handler
        .handle_switch_client_ext3(
            requester_pid,
            rmux_proto::request::SwitchClientExt3Request {
                target_client: None,
                target: Some(format!("{}:0.1", fixture.owner)),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: false,
            },
        )
        .await;
    assert!(
        matches!(response, Response::SwitchClient(_)),
        "{response:?}"
    );
    assert_linked_active_pane(&handler, &fixture, 1).await;

    let requester_switch = timeout(Duration::from_secs(2), control_rx.recv())
        .await
        .expect("switched client update is bounded")
        .expect("switched client remains registered");
    assert!(
        matches!(requester_switch, AttachControl::Switch(_)),
        "{requester_switch:?}"
    );
    let peer_refresh = timeout(Duration::from_secs(2), peer_control_rx.recv())
        .await
        .expect("linked peer refresh is bounded")
        .expect("linked peer remains registered");
    assert!(
        matches!(peer_refresh, AttachControl::Switch(_)),
        "{peer_refresh:?}"
    );
    let redundant = control_rx.try_recv();
    assert!(
        redundant.is_err(),
        "the switched client must not receive a redundant refresh: {redundant:?}"
    );
}

#[tokio::test]
async fn control_switch_client_pane_target_synchronizes_linked_and_grouped_window_aliases() {
    let handler = RequestHandler::new();
    let fixture = linked_two_pane_fixture(&handler, "control-switch-select-linked").await;
    let requester_pid = std::process::id().saturating_add(200);
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::channel(crate::control::CONTROL_SERVER_EVENT_CAPACITY);
    handler
        .register_control_with_closing(
            requester_pid,
            crate::control::ControlModeUpgrade {
                initial_command_count: 0,
                mode: rmux_proto::ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            event_tx,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(requester_pid, Some(fixture.owner.clone()))
        .await
        .expect("control session registration succeeds");
    while event_rx.try_recv().is_ok() {}

    let (peer_control_tx, mut peer_control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(
            requester_pid.saturating_add(1),
            fixture.grouped_peer.clone(),
            peer_control_tx,
        )
        .await;
    drain_attach_controls(&mut peer_control_rx).await;

    let response = handler
        .handle_switch_client_ext3(
            requester_pid,
            rmux_proto::request::SwitchClientExt3Request {
                target_client: None,
                target: Some(format!("{}:0.1", fixture.owner)),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: false,
            },
        )
        .await;
    assert!(
        matches!(response, Response::SwitchClient(_)),
        "{response:?}"
    );
    assert_linked_active_pane(&handler, &fixture, 1).await;

    let peer_refresh = timeout(Duration::from_secs(2), peer_control_rx.recv())
        .await
        .expect("control switch linked peer refresh is bounded")
        .expect("linked peer remains registered");
    assert!(
        matches!(peer_refresh, AttachControl::Switch(_)),
        "{peer_refresh:?}"
    );
}
