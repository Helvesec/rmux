use super::*;

#[tokio::test]
async fn l05_1_stale_pane_id_kill_respawn_slot_reuse() {
    let handler = RequestHandler::new();
    let session = session_name("stale-pane-id");
    create_session(&handler, &session).await;
    handler.wait_for_initial_panes_for_test().await;
    let original = pane_identity(&handler, &session).await;
    let killed = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(session.clone(), original.0),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)), "{killed:?}");
    create_session(&handler, &session).await;
    handler.wait_for_initial_panes_for_test().await;
    let replacement = pane_identity(&handler, &session).await;
    assert_ne!(replacement.0, original.0);
    for request in [
        Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(session.clone(), original.0),
            kill_all_except: false,
        }),
        Request::PaneRespawn(Box::new(PaneRespawnRequest {
            target: PaneTargetRef::by_id(session.clone(), original.0),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            keep_alive_on_exit: None,
        })),
    ] {
        let response = handler.handle(request).await;
        assert!(
            matches!(response, Response::Error(_)),
            "stale stable ID: {response:?}"
        );
        assert_eq!(
            pane_identity(&handler, &session).await,
            replacement,
            "stale request must not kill or respawn the replacement process"
        );
    }
}

async fn transferred_pane(
    handler: &RequestHandler,
) -> (SessionName, rmux_proto::PaneId, PaneTarget) {
    let source = session_name("identity-source");
    let destination = session_name("identity-destination");
    create_session(handler, &source).await;
    create_session(handler, &destination).await;
    let id = pane_identity(handler, &source).await.0;
    let rename = handler
        .handle(Request::RenameSession(rmux_proto::RenameSessionRequest {
            target: source.clone(),
            new_name: session_name("identity-renamed"),
        }))
        .await;
    assert!(matches!(rename, Response::RenameSession(_)), "{rename:?}");
    let moved = handler
        .handle(Request::MovePane(MovePaneRequest {
            source: PaneTarget::with_window(session_name("identity-renamed"), 0, 0),
            target: PaneTarget::with_window(destination.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::MovePane(moved) = moved else {
        panic!("{moved:?}");
    };
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&destination)
            .unwrap()
            .window()
            .pane(moved.target.pane_index())
            .unwrap()
            .id(),
        id
    );
    (destination, id, moved.target)
}

#[tokio::test]
async fn l05_1_by_id_after_rename_and_move() {
    let handler = RequestHandler::new();
    let (destination, id, moved) = transferred_pane(&handler).await;
    let original_name = session_name("identity-source");
    create_session(&handler, &original_name).await;
    handler.wait_for_initial_panes_for_test().await;
    let replacement = pane_identity(&handler, &original_name).await;
    let replacement_target = PaneTarget::with_window(original_name.clone(), 0, 0);
    for target in [&moved, &replacement_target] {
        handler.disable_pane_focus_reports_for_test(target).await;
        handler
            .state
            .lock()
            .await
            .start_pane_input_capture_for_test(target);
    }
    let response = handler
        .handle(Request::PaneInput(PaneInputRequest {
            target: PaneTargetRef::by_id(destination.clone(), id),
            keys: vec!["stable bytes".to_owned()],
            literal: true,
        }))
        .await;
    assert!(matches!(response, Response::SendKeys(_)), "{response:?}");
    handler
        .state
        .lock()
        .await
        .append_bytes_to_runtime_pane_transcript(&destination, id, b"\x1b[2J\x1b[HID-MOVED")
        .unwrap();
    let snapshot = handler
        .handle(Request::PaneSnapshotRef(
            rmux_proto::PaneSnapshotRefRequest {
                target: PaneTargetRef::by_id(destination, id),
            },
        ))
        .await;
    let Response::PaneSnapshot(snapshot) = snapshot else {
        panic!("{snapshot:?}");
    };
    assert!(snapshot
        .cells
        .iter()
        .map(|cell| cell.text.as_str())
        .collect::<String>()
        .contains("ID-MOVED"));
    let state = handler.state.lock().await;
    assert_eq!(
        state.pane_input_capture_for_test(&moved),
        Some(b"stable bytes".to_vec())
    );
    assert_eq!(
        state.pane_input_capture_for_test(&replacement_target),
        Some(Vec::new())
    );
    drop(state);
    assert_eq!(pane_identity(&handler, &original_name).await, replacement);
}

#[tokio::test]
async fn l05_1_by_id_select_after_move_preserves_focus_order() {
    use rmux_core::LifecycleEvent;
    let handler = RequestHandler::new();
    let (destination, id, moved) = transferred_pane(&handler).await;
    handler.wait_for_initial_panes_for_test().await;
    let prior = PaneTarget::with_window(destination.clone(), 0, 0);
    for target in [&prior, &moved] {
        handler
            .state
            .lock()
            .await
            .append_bytes_to_pane_transcript_for_test(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
                b"\x1b[?1004h",
            )
            .unwrap();
    }
    let (tx, _rx) = mpsc::unbounded_channel();
    handler
        .register_attach(978_017, destination.clone(), tx)
        .await;
    for target in [&prior, &moved] {
        handler
            .state
            .lock()
            .await
            .start_pane_input_capture_for_test(target);
    }
    let mut events = handler.subscribe_lifecycle_events();
    let select = handler
        .handle(Request::PaneSelect(rmux_proto::PaneSelectRequest {
            target: PaneTargetRef::by_id(destination.clone(), id),
            title: None,
        }))
        .await;
    assert!(matches!(select, Response::SelectPane(_)), "{select:?}");
    let focus = std::iter::from_fn(|| events.try_recv().ok())
        .map(|event| event.event)
        .filter(|event| {
            matches!(
                event,
                LifecycleEvent::PaneFocusIn { .. } | LifecycleEvent::PaneFocusOut { .. }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        focus,
        vec![
            LifecycleEvent::PaneFocusOut {
                target: prior.clone()
            },
            LifecycleEvent::PaneFocusIn {
                target: moved.clone()
            }
        ]
    );
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(&prior),
            Some(b"\x1b[O".to_vec())
        );
        assert_eq!(
            state.pane_input_capture_for_test(&moved),
            Some(b"\x1b[I".to_vec())
        );
    }
    let killed = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(destination.clone(), id),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)), "{killed:?}");
    let mut events = handler.subscribe_lifecycle_events();
    let before = handler
        .state
        .lock()
        .await
        .pane_input_capture_for_test(&prior);
    let stale = handler
        .handle(Request::PaneSelect(rmux_proto::PaneSelectRequest {
            target: PaneTargetRef::by_id(destination, id),
            title: None,
        }))
        .await;
    assert!(matches!(stale, Response::Error(_)), "{stale:?}");
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .pane_input_capture_for_test(&prior),
        before
    );
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
            event.event,
            LifecycleEvent::PaneFocusIn { .. } | LifecycleEvent::PaneFocusOut { .. }
        ))
    );
}
