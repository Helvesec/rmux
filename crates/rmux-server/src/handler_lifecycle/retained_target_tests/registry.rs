use super::*;

#[tokio::test]
async fn moved_pane_stays_live_while_its_destroyed_source_window_retires() {
    let handler = RequestHandler::new();
    let session_name = session_name("retained-pane-move");
    let mut state = handler.state.lock().await;
    state
        .sessions
        .create_session(session_name.clone(), terminal_size())
        .expect("create session");
    state
        .sessions
        .session_mut(&session_name)
        .expect("session exists")
        .create_window(terminal_size())
        .expect("create destination window");

    let source_pane = PaneTarget::with_window(session_name.clone(), 0, 0);
    let source_window = WindowTarget::with_window(session_name.clone(), 0);
    let pane_lease = state
        .capture_retained_pane_lifecycle_target(&source_pane)
        .expect("capture pane lease");
    let window_lease = state
        .capture_retained_window_lifecycle_target(&source_window)
        .expect("capture window lease");
    let pane_id = state
        .sessions
        .session(&session_name)
        .and_then(|session| session.window_at(0))
        .and_then(|window| window.pane(0))
        .expect("source pane exists")
        .id();

    state
        .sessions
        .session_mut(&session_name)
        .expect("session exists")
        .join_pane(
            SessionPaneTarget::new(0, 0),
            SessionPaneTarget::new(1, 0),
            PaneJoinOptions::new(SplitDirection::Vertical, false, false, false, None),
        )
        .expect("move last source pane into destination window");
    state.retire_removed_lifecycle_targets();

    let resolved_pane = match pane_lease.resolve(&state) {
        LeaseResolution::Live(Target::Pane(target)) => target,
        resolution => panic!("moved pane should stay live, got {resolution:?}"),
    };
    assert_eq!(resolved_pane.window_index(), 1);
    assert_eq!(
        state
            .sessions
            .session(&session_name)
            .and_then(|session| session.window_at(resolved_pane.window_index()))
            .and_then(|window| window.pane(resolved_pane.pane_index()))
            .map(rmux_core::Pane::id),
        Some(pane_id)
    );
    assert_retired(&window_lease, &state);

    state.retire_respawned_lifecycle_panes(&[pane_id]);
    assert_retired(&pane_lease, &state);
}

#[tokio::test]
async fn retained_targets_follow_surviving_aliases_deterministically() {
    let handler = RequestHandler::new();
    let alpha = create_handler_session(&handler, "retained-alias-alpha").await;
    let beta = create_handler_session(&handler, "retained-alias-beta").await;
    let gamma = create_handler_session(&handler, "retained-alias-gamma").await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    handler.wait_for_initial_panes_for_test().await;
    for (session_name, window_index) in [(&gamma, 3), (&gamma, 2), (&beta, 1)] {
        let response = handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(alpha.clone(), 0),
                target: WindowTarget::with_window(session_name.clone(), window_index),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await;
        assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    }

    let source_window = WindowTarget::with_window(alpha.clone(), 0);
    let source_pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    let (window_lease, pane_lease) = {
        let state = handler.state.lock().await;
        let window_lease = state
            .capture_retained_window_lifecycle_target(&source_window)
            .expect("capture retained window alias");
        let pane_lease = state
            .capture_retained_pane_lifecycle_target(&source_pane)
            .expect("capture retained pane alias");
        (window_lease, pane_lease)
    };

    let response = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: source_window,
            kill_if_last: false,
        }))
        .await;
    assert!(
        matches!(response, Response::UnlinkWindow(_)),
        "{response:?}"
    );

    let state = handler.state.lock().await;
    assert_eq!(
        window_lease.resolve(&state),
        LeaseResolution::Live(Target::Window(WindowTarget::with_window(gamma.clone(), 2,)))
    );
    assert_eq!(
        pane_lease.resolve(&state),
        LeaseResolution::Live(Target::Pane(PaneTarget::with_window(gamma, 2, 0)))
    );
}

#[tokio::test]
async fn surviving_alias_becomes_the_retirement_slot_after_original_slot_reuse() {
    let handler = RequestHandler::new();
    let alpha = create_handler_session(&handler, "retained-cursor-alpha").await;
    let beta = create_handler_session(&handler, "retained-cursor-beta").await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    handler.wait_for_initial_panes_for_test().await;
    let source = WindowTarget::with_window(alpha.clone(), 0);
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: source.clone(),
            target: WindowTarget::with_window(beta.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    let lease = {
        let state = handler.state.lock().await;
        state
            .capture_retained_window_lifecycle_target(&source)
            .expect("capture retained aliased window")
    };

    let response = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: source,
            kill_if_last: false,
        }))
        .await;
    assert!(
        matches!(response, Response::UnlinkWindow(_)),
        "{response:?}"
    );
    {
        let state = handler.state.lock().await;
        assert_eq!(
            lease.resolve(&state),
            LeaseResolution::Live(Target::Window(WindowTarget::with_window(beta.clone(), 1)))
        );
    }

    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha,
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(0),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    handler.wait_for_initial_panes_for_test().await;

    let response = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: WindowTarget::with_window(beta, 1),
            kill_if_last: true,
        }))
        .await;
    assert!(
        matches!(response, Response::UnlinkWindow(_)),
        "{response:?}"
    );
    let state = handler.state.lock().await;
    assert_retired(&lease, &state);
}

#[tokio::test]
async fn respawn_boundary_retires_the_old_pane_lifetime_even_when_id_survives() {
    let handler = RequestHandler::new();
    let session_name = session_name("retained-pane-respawn");
    let mut state = handler.state.lock().await;
    state
        .sessions
        .create_session(session_name.clone(), terminal_size())
        .expect("create session");
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    let lease = state
        .capture_retained_pane_lifecycle_target(&target)
        .expect("capture pane lease");
    let pane_id = state
        .sessions
        .session(&session_name)
        .and_then(|session| session.window_at(0))
        .and_then(|window| window.pane(0))
        .expect("pane exists")
        .id();

    state.retire_respawned_lifecycle_panes(&[pane_id]);

    assert_retired(&lease, &state);
    assert!(
        state
            .sessions
            .session(&session_name)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .is_some_and(|pane| pane.id() == pane_id),
        "respawn retirement is a lifetime boundary, not numeric-id removal"
    );
}

#[tokio::test]
async fn same_transaction_slot_replacement_invalidates_instead_of_retargeting() {
    let handler = RequestHandler::new();
    let session_name = session_name("retained-window-replacement");
    let mut state = handler.state.lock().await;
    state
        .sessions
        .create_session(session_name.clone(), terminal_size())
        .expect("create session");
    let target = WindowTarget::with_window(session_name.clone(), 0);
    let lease = state
        .capture_retained_window_lifecycle_target(&target)
        .expect("capture window lease");

    let session = state
        .sessions
        .session_mut(&session_name)
        .expect("session exists");
    session
        .remove_window_allowing_empty(0)
        .expect("remove original window");
    session
        .insert_window_with_initial_pane(0, terminal_size())
        .expect("replace numeric slot");
    state.retire_removed_lifecycle_targets();

    assert_eq!(lease.resolve(&state), LeaseResolution::Replaced);
}

#[tokio::test]
async fn retired_window_never_reacquires_a_later_numeric_slot_reuse() {
    let handler = RequestHandler::new();
    let session_name = session_name("retained-window-reuse");
    let mut state = handler.state.lock().await;
    state
        .sessions
        .create_session(session_name.clone(), terminal_size())
        .expect("create session");
    let target = WindowTarget::with_window(session_name.clone(), 0);
    let lease = state
        .capture_retained_window_lifecycle_target(&target)
        .expect("capture window lease");
    assert!(matches!(lease.resolve(&state), LeaseResolution::Live(_)));

    state
        .sessions
        .session_mut(&session_name)
        .expect("session exists")
        .remove_window_allowing_empty(0)
        .expect("remove original window");
    state.retire_removed_lifecycle_targets();
    assert_retired(&lease, &state);

    state
        .sessions
        .session_mut(&session_name)
        .expect("session exists")
        .insert_window_with_initial_pane(0, terminal_size())
        .expect("reuse old numeric slot later");
    assert_retired(&lease, &state);
}
