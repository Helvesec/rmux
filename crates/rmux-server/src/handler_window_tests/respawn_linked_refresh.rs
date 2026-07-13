use super::*;

fn respawn_refresh_target(
    control: AttachControl,
    expected_session: &SessionName,
) -> crate::pane_io::AttachTarget {
    let AttachControl::Switch(target) = control else {
        panic!("expected linked alias switch refresh, got {control:?}");
    };
    assert_eq!(&target.session_name, expected_session);
    *target
}

async fn receive_latest_respawn_refresh(
    receiver: &mut mpsc::UnboundedReceiver<AttachControl>,
    expected_session: &SessionName,
) -> crate::pane_io::AttachTarget {
    let control = timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("linked alias must receive a respawn refresh")
        .expect("linked alias control channel remains open");
    let mut latest = respawn_refresh_target(control, expected_session);
    while let Ok(control) = receiver.try_recv() {
        latest = respawn_refresh_target(control, expected_session);
    }
    latest
}

#[tokio::test]
async fn respawn_window_refreshes_attached_link_aliases_after_active_pane_is_removed() {
    let handler = RequestHandler::new();
    let owner = session_name("respawn-refresh-owner");
    let alias = session_name("respawn-refresh-alias");
    let alias_peer = session_name("respawn-refresh-alias-peer");
    create_session(&handler, owner.as_str()).await;
    create_session(&handler, alias.as_str()).await;
    create_grouped_session(&handler, alias_peer.as_str(), &alias).await;

    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(owner.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    let split_target = match split {
        Response::SplitWindow(response) => response.pane,
        response => panic!("expected owner split success, got {response:?}"),
    };
    let (retained_pane_id, removed_active_pane_id) = {
        let mut state = handler.state.lock().await;
        let owner_session = state.sessions.session_mut(&owner).expect("owner exists");
        owner_session
            .select_pane_in_window(0, split_target.pane_index())
            .expect("split pane becomes active");
        let window = owner_session.window_at(0).expect("owner window exists");
        (
            window.pane(0).expect("retained pane exists").id(),
            window.active_pane().expect("active split pane exists").id(),
        )
    };
    assert_ne!(retained_pane_id, removed_active_pane_id);

    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(alias.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    {
        let state = handler.state.lock().await;
        for session_name in [&owner, &alias, &alias_peer] {
            let window = state
                .sessions
                .session(session_name)
                .and_then(|session| session.window_at(0))
                .expect("linked family window exists");
            assert_eq!(window.panes().len(), 2);
            assert_eq!(
                window
                    .active_pane()
                    .expect("linked active pane exists")
                    .id(),
                removed_active_pane_id,
                "{session_name} must initially render the pane respawn-window removes"
            );
        }
    }

    let (alias_tx, mut alias_rx) = mpsc::unbounded_channel();
    let (peer_tx, mut peer_rx) = mpsc::unbounded_channel();
    handler.register_attach(51, alias.clone(), alias_tx).await;
    handler
        .register_attach(52, alias_peer.clone(), peer_tx)
        .await;
    tokio::join!(
        drain_attach_controls(&mut alias_rx),
        drain_attach_controls(&mut peer_rx),
    );

    let response = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: WindowTarget::with_window(owner.clone(), 0),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(quiet_window_test_command()),
        })))
        .await;
    assert!(
        matches!(response, Response::RespawnWindow(_)),
        "{response:?}"
    );

    let mut alias_target = receive_latest_respawn_refresh(&mut alias_rx, &alias).await;
    let mut peer_target = receive_latest_respawn_refresh(&mut peer_rx, &alias_peer).await;

    let output = {
        let state = handler.state.lock().await;
        for session_name in [&owner, &alias, &alias_peer] {
            let window = state
                .sessions
                .session(session_name)
                .and_then(|session| session.window_at(0))
                .expect("respawned linked family window exists");
            assert_eq!(window.panes().len(), 1);
            assert_eq!(
                window.active_pane().expect("respawned pane exists").id(),
                retained_pane_id,
                "{session_name} must select the retained respawned pane"
            );
        }
        state
            .pane_output_for_target(&alias, 0, 0)
            .expect("respawned alias output exists")
            .clone()
    };
    let expected = b"respawn-linked-alias-live-output".to_vec();
    output.send(expected.clone());
    for target in [&mut alias_target, &mut peer_target] {
        let received = timeout(Duration::from_secs(2), target.pane_output.recv())
            .await
            .expect("refreshed alias receiver follows the respawned runtime");
        let rmux_core::events::OutputCursorItem::Event(event) = received else {
            panic!("expected respawned pane output, got {received:?}");
        };
        assert_eq!(event.bytes(), expected);
    }
}
