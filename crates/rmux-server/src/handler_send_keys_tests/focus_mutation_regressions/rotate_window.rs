use super::*;

async fn rotate_focus_fixture(
    label: &str,
) -> (
    RequestHandler,
    rmux_proto::SessionName,
    PaneTarget,
    PaneTarget,
    tokio::sync::broadcast::Receiver<QueuedLifecycleEvent>,
    mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
) {
    let handler = RequestHandler::new();
    let alpha = session_name(label);
    create_send_keys_test_session(&handler, &alpha).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    let first = PaneTarget::with_window(alpha.clone(), 0, 0);
    let second = PaneTarget::with_window(alpha.clone(), 0, 1);
    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: first.clone(),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");
    enable_focus_mode(&handler, &first).await;
    enable_focus_mode(&handler, &second).await;
    let control_rx = register_focused_attach(&handler, std::process::id(), &alpha).await;
    let lifecycle = handler.subscribe_lifecycle_events();
    (handler, alpha, first, second, lifecycle, control_rx)
}

#[tokio::test]
async fn focus_rotate_window_reconciles_both_directions_before_layout() {
    let (handler, alpha, first_slot, second_slot, mut lifecycle, _control_rx) =
        rotate_focus_fixture("focus-rotate-directions").await;
    for (direction, old_active_slot, new_active_slot) in [
        (
            RotateWindowDirection::Up,
            second_slot.clone(),
            first_slot.clone(),
        ),
        (
            RotateWindowDirection::Down,
            second_slot.clone(),
            first_slot.clone(),
        ),
    ] {
        {
            let state = handler.state.lock().await;
            state.start_pane_input_capture_for_test(&first_slot);
            state.start_pane_input_capture_for_test(&second_slot);
        }
        let response = handler
            .handle(Request::RotateWindow(RotateWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 0),
                direction,
                restore_zoom: false,
            }))
            .await;
        assert!(
            matches!(response, Response::RotateWindow(_)),
            "{response:?}"
        );

        let events = focus_and_window_events(&mut lifecycle);
        assert!(
            matches!(
                events.as_slice(),
                [
                    LifecycleEvent::PaneFocusOut { .. },
                    LifecycleEvent::PaneFocusIn { .. },
                    LifecycleEvent::WindowLayoutChanged { .. }
                ]
            ),
            "{direction:?}: {events:?}"
        );
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(&old_active_slot),
            Some(b"\x1b[O".to_vec()),
            "{direction:?}"
        );
        assert_eq!(
            state.pane_input_capture_for_test(&new_active_slot),
            Some(b"\x1b[I".to_vec()),
            "{direction:?}"
        );
    }
}

#[tokio::test]
async fn focus_rotate_window_reconciles_through_linked_alias() {
    let (handler, alpha, first, second, _initial_lifecycle, _owner_control_rx) =
        rotate_focus_fixture("focus-rotate-linked-owner").await;
    let peer = session_name("focus-rotate-linked-peer");
    create_send_keys_test_session(&handler, &peer).await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(peer.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: false,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    let peer_first = PaneTarget::with_window(peer.clone(), 1, 0);
    let peer_second = PaneTarget::with_window(peer.clone(), 1, 1);
    let (old_active_id, new_active_id) = {
        let state = handler.state.lock().await;
        let owner = state.sessions.session(&alpha).unwrap();
        let alias = state.sessions.session(&peer).unwrap();
        assert_eq!(owner.active_window_index(), 0);
        assert_eq!(
            alias.active_window_index(),
            1,
            "the peer attach must contribute focus through the linked alias"
        );
        let owner_window = owner.window_at(0).unwrap();
        let alias_window = alias.window_at(1).unwrap();
        let old_active_id = owner_window.pane(0).unwrap().id();
        let new_active_id = owner_window.pane(1).unwrap().id();
        assert_ne!(old_active_id, new_active_id);
        assert_eq!(owner_window.active_pane().unwrap().id(), old_active_id);
        assert_eq!(alias_window.active_pane().unwrap().id(), old_active_id);
        assert_eq!(
            state.runtime_session_name_for_window(&alpha, 0),
            state.runtime_session_name_for_window(&peer, 1),
            "both window routes resolve to the same runtime owner"
        );
        for (owner_target, alias_target, pane_id) in [
            (&first, &peer_first, old_active_id),
            (&second, &peer_second, new_active_id),
        ] {
            assert_eq!(
                alias_window.pane(alias_target.pane_index()).unwrap().id(),
                pane_id
            );
            let generation = state.pane_output_generation_for_target(owner_target, pane_id);
            assert!(
                generation > 0,
                "the shared pane has a live runtime generation"
            );
            assert_eq!(
                generation,
                state.pane_output_generation_for_target(alias_target, pane_id),
                "aliases share the same pane runtime incarnation"
            );
        }
        (old_active_id, new_active_id)
    };
    let owner_pid = std::process::id();
    let peer_pid = owner_pid.saturating_add(1);
    let _peer_control_rx = register_focused_attach(&handler, peer_pid, &peer).await;
    {
        let active_attach = handler.active_attach.lock().await;
        assert_eq!(active_attach.by_pid.len(), 2);
        for (pid, session) in [(owner_pid, &alpha), (peer_pid, &peer)] {
            let active = &active_attach.by_pid[&pid];
            assert_eq!(&active.session_name, session);
            assert_eq!(
                active.focus_origin,
                crate::handler::focus_support::AttachFocusOrigin::Native
            );
            assert!(active.reported_focused);
            assert!(!active.closing.load(std::sync::atomic::Ordering::SeqCst));
            assert!(active.overlay.is_none() && active.display_panes.is_none());
        }
        assert_eq!(
            active_attach.focused_panes.len(),
            1,
            "two native focused attaches contribute to one shared runtime"
        );
    }
    let mut lifecycle = handler.subscribe_lifecycle_events();
    {
        let state = handler.state.lock().await;
        for target in [&first, &second, &peer_first, &peer_second] {
            state.start_pane_input_capture_for_test(target);
        }
    }

    let alias_window = WindowTarget::with_window(peer.clone(), 1);
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        handler.handle(Request::RotateWindow(RotateWindowRequest {
            target: alias_window.clone(),
            direction: RotateWindowDirection::Up,
            restore_zoom: false,
        })),
    )
    .await
    .expect("alias rotation drains focus before its layout ticket");
    assert!(
        matches!(response, Response::RotateWindow(_)),
        "{response:?}"
    );
    let events = focus_and_window_events(&mut lifecycle);
    assert!(
        matches!(
            events.as_slice(),
            [
                LifecycleEvent::PaneFocusOut { .. },
                LifecycleEvent::PaneFocusIn { .. },
                LifecycleEvent::WindowLayoutChanged { target }
            ] if target == &alias_window
        ),
        "one shared runtime delta is emitted for linked aliases: {events:?}"
    );
    {
        let state = handler.state.lock().await;
        let mut focus_out = Vec::new();
        let mut focus_in = Vec::new();
        // Captures are route-local. Group every alias by its post-rotation
        // PaneId so a duplicate write through another alias cannot hide.
        for (targets, pane_id, bytes) in [
            ([&second, &peer_second], old_active_id, &mut focus_out),
            ([&first, &peer_first], new_active_id, &mut focus_in),
        ] {
            for target in targets {
                let window = state
                    .sessions
                    .session(target.session_name())
                    .unwrap()
                    .window_at(target.window_index())
                    .unwrap();
                assert_eq!(window.pane(target.pane_index()).unwrap().id(), pane_id);
                assert_eq!(window.active_pane().unwrap().id(), new_active_id);
                bytes.extend(state.pane_input_capture_for_test(target).unwrap());
            }
        }
        assert_eq!(focus_out, b"\x1b[O", "one PO for the old shared runtime");
        assert_eq!(focus_in, b"\x1b[I", "one PI for the new shared runtime");
    }
    let witness =
        tokio::time::timeout(Duration::from_secs(2), add_detached_window(&handler, &peer))
            .await
            .expect("the lifecycle FIFO must accept the command after alias rotation");
    assert_eq!(witness.session_name(), &peer);
}

#[tokio::test]
async fn focus_rotate_window_without_spectator_and_rejected_rotation_publish_no_delta() {
    let handler = RequestHandler::new();
    let alpha = session_name("focus-rotate-no-spectator");
    create_send_keys_test_session(&handler, &alpha).await;
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let rejected = handler
        .handle(Request::RotateWindow(RotateWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 99),
            direction: RotateWindowDirection::Up,
            restore_zoom: false,
        }))
        .await;
    assert!(matches!(rejected, Response::Error(_)), "{rejected:?}");
    assert!(focus_and_window_events(&mut lifecycle).is_empty());

    let response = handler
        .handle(Request::RotateWindow(RotateWindowRequest {
            target: WindowTarget::with_window(alpha, 0),
            direction: RotateWindowDirection::Up,
            restore_zoom: false,
        }))
        .await;
    assert!(
        matches!(response, Response::RotateWindow(_)),
        "{response:?}"
    );
    assert!(
        focus_and_window_events(&mut lifecycle)
            .iter()
            .all(|event| !matches!(
                event,
                LifecycleEvent::PaneFocusIn { .. } | LifecycleEvent::PaneFocusOut { .. }
            )),
        "rotation without a spectator has no focus contribution"
    );
}
