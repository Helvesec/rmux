use super::*;

#[tokio::test]
async fn move_window_with_d_keeps_the_next_window_active_when_moving_the_current_slot() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 2).await;

    {
        let mut state = handler.state.lock().await;
        let session = state
            .sessions
            .session_mut(&alpha)
            .expect("alpha should exist");
        session.select_window(2).expect("window 2 select succeeds");
        session.select_window(0).expect("window 0 select succeeds");
    }

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 4)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: alpha.clone(),
            target: Some(WindowTarget::with_window(alpha.clone(), 4)),
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![2, 4]
    );
    assert_eq!(session.active_window_index(), 2);
    assert_eq!(session.last_window_index(), None);
}

#[tokio::test]
async fn swap_window_same_source_and_destination_is_a_noop() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 2).await;

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(alpha.clone(), 2),
            detached: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(alpha.clone(), 2),
        })
    );
}

#[tokio::test]
async fn swap_window_without_d_preserves_active_slot() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 2).await;
    insert_window(&handler, &alpha, 5).await;

    {
        let mut state = handler.state.lock().await;
        let session = state
            .sessions
            .session_mut(&alpha)
            .expect("alpha should exist");
        session.select_window(5).expect("window 5 select succeeds");
        session.select_window(2).expect("window 2 select succeeds");
    }

    // Without -d, tmux preserves the active winlink. Here it already points to
    // index 2, so active remains 2 while the swapped content changes.
    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(alpha.clone(), 5),
            detached: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(alpha.clone(), 5),
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(session.active_window_index(), 2);
    assert_eq!(session.last_window_index(), Some(5));
}

#[tokio::test]
async fn swap_window_without_d_preserves_active_when_active_is_elsewhere() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 2).await;
    insert_window(&handler, &alpha, 5).await;

    // Active is at window 0 (default). Source=2, target=5.
    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(alpha.clone(), 5),
            detached: false,
        }))
        .await;

    assert!(matches!(response, Response::SwapWindow(_)));

    // Without -d, tmux preserves the active winlink at 0.
    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(session.active_window_index(), 0);
    assert_eq!(session.last_window_index(), None);
}

#[tokio::test]
async fn swap_window_with_d_selects_target_window_within_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 2).await;
    insert_window(&handler, &alpha, 5).await;

    // Active is at window 0 (default). Source=2, target=5.
    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(alpha.clone(), 5),
            detached: true,
        }))
        .await;

    assert!(matches!(response, Response::SwapWindow(_)));

    // With -d, tmux selects the destination winlink after swapping.
    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(session.active_window_index(), 5);
    assert_eq!(session.last_window_index(), Some(0));
}

#[tokio::test]
async fn move_window_reindex_with_source_ignores_source_and_preserves_active_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 2).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 2)),
            target: MoveWindowTarget::Session(alpha.clone()),
            renumber: true,
            kill_destination: false,
            detached: false,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: alpha.clone(),
            target: None,
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(session.active_window_index(), 0);
}

#[tokio::test]
async fn move_window_across_sessions_removes_source_session_when_moving_its_last_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .expect("alpha exists")
            .pane_id_in_window(0, 0)
            .expect("source pane exists")
    };

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(beta.clone(), 5)),
            renumber: false,
            kill_destination: false,
            detached: false,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: beta.clone(),
            target: Some(WindowTarget::with_window(beta.clone(), 5)),
        })
    );

    let state = handler.state.lock().await;
    assert!(
        state.sessions.session(&alpha).is_none(),
        "tmux removes a source session emptied by move-window"
    );
    let beta_session = state.sessions.session(&beta).expect("beta should exist");
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 5]
    );
    assert_eq!(beta_session.pane_id_in_window(5, 0), Some(moved_pane_id));
    state
        .pane_profile_in_window(&beta, 5, 0)
        .expect("moved pane terminal should live in the destination session");
    assert_eq!(
        state.pane_profile_in_window(&alpha, 0, 0).unwrap_err(),
        rmux_proto::RmuxError::SessionNotFound("alpha".to_owned())
    );
}

#[tokio::test]
async fn swap_window_rejects_cross_session_swap_within_same_session_group() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    // Create beta as a grouped session in the same group as alpha.
    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .create_grouped_session_with_base_index(
                beta.clone(),
                TerminalSize { cols: 80, rows: 24 },
                0,
                alpha.clone(),
            )
            .expect("grouped session creation succeeds");
    }

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 0),
            detached: false,
        }))
        .await;

    assert!(
        matches!(&response, Response::Error(e) if e.error.to_string().contains("sessions are grouped")),
        "expected session-group guard error, got {response:?}"
    );
}

#[tokio::test]
async fn swap_window_allows_cross_session_swap_between_different_groups() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &beta, 1).await;

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 0),
            detached: false,
        }))
        .await;

    assert!(
        matches!(response, Response::SwapWindow(_)),
        "expected swap success between ungrouped sessions, got {response:?}"
    );
}
