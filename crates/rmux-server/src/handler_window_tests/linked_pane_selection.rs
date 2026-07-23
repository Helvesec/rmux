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

    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(owner.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");

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
                .session_mut(target.session_name())
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

    LinkedPaneFixture {
        owner,
        grouped_peer,
        linked_peer,
        pane_one_id,
    }
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
