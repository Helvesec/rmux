use super::*;

#[tokio::test]
async fn move_window_across_sessions_migrates_the_terminal_ownership_map() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 1).await;

    let moved_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .expect("alpha should exist")
            .window_at(1)
            .expect("window 1 should exist")
            .pane(0)
            .expect("pane 0 should exist")
            .id()
    };

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 1)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(beta.clone(), 4)),
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
            session_name: beta.clone(),
            target: Some(WindowTarget::with_window(beta.clone(), 4)),
        })
    );

    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should exist");
    let beta_session = state.sessions.session(&beta).expect("beta should exist");
    assert_eq!(
        alpha_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 4]
    );
    assert_eq!(
        beta_session
            .window_at(4)
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(moved_pane_id)
    );
    state
        .pane_profile_in_window(&beta, 4, 0)
        .expect("moved pane terminal should exist in the destination session");
    assert_eq!(
        state.pane_profile_in_window(&alpha, 1, 0).unwrap_err(),
        rmux_proto::RmuxError::invalid_target("alpha:1", "window index does not exist in session")
    );
}

#[tokio::test]
async fn move_window_within_session_moves_linked_slot_metadata() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;

    let link = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(link, Response::LinkWindow(_)));

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 2)),
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
            target: Some(WindowTarget::with_window(alpha.clone(), 2)),
        })
    );

    {
        let state = handler.state.lock().await;
        assert_eq!(state.window_link_count(&alpha, 2), 2);
        assert_eq!(state.window_link_count(&beta, 1), 2);
        assert_eq!(state.window_link_count(&alpha, 0), 1);
        assert_eq!(
            state.window_linked_sessions_list(&beta, 1),
            vec![alpha.clone(), beta.clone()]
        );
    }

    let rename = handler
        .handle(Request::RenameWindow(RenameWindowRequest {
            target: WindowTarget::with_window(beta.clone(), 1),
            name: "logs".to_owned(),
        }))
        .await;
    assert!(matches!(rename, Response::RenameWindow(_)));

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(2))
            .and_then(|window| window.name()),
        Some("logs")
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.name()),
        Some("logs")
    );
}

#[tokio::test]
async fn move_window_from_group_peer_moves_runtime_state_and_removes_empty_group() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;
    create_session(&handler, "gamma").await;

    let moved_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("grouped pane should exist")
    };

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(beta.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(gamma.clone(), 1)),
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
            session_name: gamma.clone(),
            target: Some(WindowTarget::with_window(gamma.clone(), 1)),
        })
    );

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&alpha).is_none());
    assert!(state.sessions.session(&beta).is_none());
    assert_eq!(
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(moved_pane_id)
    );
    state
        .pane_profile_in_window(&gamma, 1, 0)
        .expect("moved group pane terminal should live in the destination session");
}

#[tokio::test]
async fn move_window_rejects_cross_session_move_within_same_session_group() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;

    let shared_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("grouped pane should exist before move-window")
    };

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(beta.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 5)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert!(
        matches!(&response, Response::Error(error) if error.error.to_string().contains("sessions are grouped")),
        "expected grouped-session rejection, got {response:?}"
    );

    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should remain");
    let beta_session = state.sessions.session(&beta).expect("beta should remain");
    assert_eq!(
        alpha_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(alpha_session.pane_id_in_window(0, 0), Some(shared_pane_id));
    assert_eq!(beta_session.pane_id_in_window(0, 0), Some(shared_pane_id));
    state
        .pane_profile_in_window(&alpha, 0, 0)
        .expect("alpha pane terminal should remain");
    state
        .pane_profile_in_window(&beta, 0, 0)
        .expect("beta grouped pane terminal should remain");
}

#[tokio::test]
async fn move_window_relative_rejects_cross_session_move_within_same_session_group() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(beta.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: true,
            before: false,
        }))
        .await;

    assert!(
        matches!(&response, Response::Error(error) if error.error.to_string().contains("sessions are grouped")),
        "expected grouped-session rejection, got {response:?}"
    );

    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should remain");
    let beta_session = state.sessions.session(&beta).expect("beta should remain");
    assert_eq!(
        alpha_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0]
    );
}

#[tokio::test]
async fn move_window_from_group_peer_linked_source_removes_empty_group() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    let delta = session_name("delta");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;
    create_session(&handler, "gamma").await;
    create_session(&handler, "delta").await;

    let linked_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("grouped linked pane should exist")
    };

    let link = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(gamma.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(link, Response::LinkWindow(_)));

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(beta.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(delta.clone(), 1)),
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
            session_name: delta.clone(),
            target: Some(WindowTarget::with_window(delta.clone(), 1)),
        })
    );

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&alpha).is_none());
    assert!(state.sessions.session(&beta).is_none());
    assert_eq!(
        state
            .sessions
            .session(&delta)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(linked_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(linked_pane_id)
    );
    state
        .pane_profile_in_window(&delta, 1, 0)
        .expect("moved linked pane should live in the target runtime");
    state
        .pane_profile_in_window(&gamma, 1, 0)
        .expect("surviving linked peer should keep runtime access");
    assert_eq!(state.window_link_count(&delta, 1), 2);
    assert_eq!(state.window_link_count(&gamma, 1), 2);
}

#[tokio::test]
async fn move_window_kill_destination_preserves_surviving_linked_window_runtime() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let gamma = session_name("gamma");
    let delta = session_name("delta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "gamma").await;
    create_session(&handler, "delta").await;

    let (source_pane_id, linked_pane_id) = {
        let state = handler.state.lock().await;
        (
            state
                .sessions
                .session(&alpha)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("alpha pane should exist"),
            state
                .sessions
                .session(&gamma)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("gamma pane should exist"),
        )
    };

    let link = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(gamma.clone(), 0),
            target: WindowTarget::with_window(delta.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(link, Response::LinkWindow(_)));

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(gamma.clone(), 0)),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: gamma.clone(),
            target: Some(WindowTarget::with_window(gamma.clone(), 0)),
        })
    );

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&alpha).is_none());
    assert_eq!(
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(source_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&delta)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(linked_pane_id)
    );
    state
        .pane_profile_in_window(&gamma, 0, 0)
        .expect("moved source pane should live in gamma");
    state
        .pane_profile_in_window(&delta, 1, 0)
        .expect("surviving linked pane should keep a runtime after overwrite");
    assert_eq!(state.window_link_count(&gamma, 0), 1);
    assert_eq!(state.window_link_count(&delta, 1), 1);
}

#[tokio::test]
async fn move_window_within_session_kill_destination_preserves_surviving_linked_runtime() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 2).await;

    let (source_pane_id, linked_pane_id) = {
        let state = handler.state.lock().await;
        (
            state
                .sessions
                .session(&alpha)
                .and_then(|session| session.window_at(2))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("alpha:2 pane should exist"),
            state
                .sessions
                .session(&alpha)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("alpha:0 pane should exist"),
        )
    };

    let link = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(link, Response::LinkWindow(_)));

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 2)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: alpha.clone(),
            target: Some(WindowTarget::with_window(alpha.clone(), 0)),
        })
    );

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(source_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(linked_pane_id)
    );
    state
        .pane_profile_in_window(&alpha, 0, 0)
        .expect("moved source pane should remain available");
    state
        .pane_profile_in_window(&beta, 1, 0)
        .expect("surviving linked peer should keep its runtime");
    assert_eq!(state.window_link_count(&alpha, 0), 1);
    assert_eq!(state.window_link_count(&beta, 1), 1);
}

#[tokio::test]
async fn move_window_within_session_restores_the_killed_destination_when_resize_fails() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;

    let (source_pane_id, destination_pane_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("alpha should exist");
        (
            session
                .window_at(0)
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("window 0 pane should exist"),
            session
                .window_at(1)
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("window 1 pane should exist"),
        )
    };

    {
        let mut state = handler.state.lock().await;
        state.fail_next_resize_for_test();
    }

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 1)),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Server(
                "injected pane terminal resize failure".to_owned()
            ),
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(session.pane_id_in_window(0, 0), Some(source_pane_id));
    assert_eq!(session.pane_id_in_window(1, 0), Some(destination_pane_id));
    state
        .pane_profile_in_window(&alpha, 0, 0)
        .expect("source pane terminal should be restored");
    state
        .pane_profile_in_window(&alpha, 1, 0)
        .expect("destination pane terminal should be restored");
}

#[tokio::test]
async fn move_window_reindex_compacts_sparse_window_indices() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 3).await;
    insert_window(&handler, &alpha, 7).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: None,
            target: MoveWindowTarget::Session(alpha.clone()),
            renumber: true,
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
            target: None,
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[tokio::test]
async fn move_window_reindex_with_source_ignores_source_and_renumbers_target_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 3).await;
    insert_window(&handler, &beta, 4).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 3)),
            target: MoveWindowTarget::Session(beta.clone()),
            renumber: true,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: beta.clone(),
            target: None,
        })
    );

    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should exist");
    assert!(alpha_session.window_at(3).is_some());
    let beta_session = state.sessions.session(&beta).expect("beta should exist");
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[tokio::test]
async fn move_window_reindex_with_window_target_renumbers_target_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 5).await;
    insert_window(&handler, &alpha, 9).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: None,
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 9)),
            renumber: true,
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
            target: None,
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[tokio::test]
async fn move_window_reindex_with_source_and_window_target_ignores_source() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 2).await;
    insert_window(&handler, &alpha, 5).await;
    insert_window(&handler, &beta, 4).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 5)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(beta.clone(), 4)),
            renumber: true,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: beta.clone(),
            target: None,
        })
    );

    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        alpha_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 2, 5]
    );
    let beta_session = state.sessions.session(&beta).expect("beta should exist");
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[tokio::test]
async fn move_window_after_source_already_after_target_matches_tmux_gap_shape() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;

    let (source_pane_id, trailing_pane_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("alpha should exist");
        (
            session
                .pane_id_in_window(1, 0)
                .expect("source pane should exist"),
            session
                .pane_id_in_window(2, 0)
                .expect("trailing pane should exist"),
        )
    };

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 1)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
            renumber: false,
            kill_destination: false,
            detached: false,
            after: true,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: alpha.clone(),
            target: Some(WindowTarget::with_window(alpha.clone(), 1)),
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1, 3]
    );
    assert_eq!(session.pane_id_in_window(1, 0), Some(source_pane_id));
    assert_eq!(session.pane_id_in_window(3, 0), Some(trailing_pane_id));
    assert_eq!(session.active_window_index(), 1);
}

#[tokio::test]
async fn move_window_before_source_is_target_matches_tmux_gap_shape() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;

    let (source_pane_id, next_pane_id, trailing_pane_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("alpha should exist");
        (
            session
                .pane_id_in_window(0, 0)
                .expect("source pane should exist"),
            session
                .pane_id_in_window(1, 0)
                .expect("next pane should exist"),
            session
                .pane_id_in_window(2, 0)
                .expect("trailing pane should exist"),
        )
    };

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
            renumber: false,
            kill_destination: false,
            detached: false,
            after: false,
            before: true,
        }))
        .await;

    assert_eq!(
        response,
        Response::MoveWindow(rmux_proto::MoveWindowResponse {
            session_name: alpha.clone(),
            target: Some(WindowTarget::with_window(alpha.clone(), 0)),
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 2, 3]
    );
    assert_eq!(session.pane_id_in_window(0, 0), Some(source_pane_id));
    assert_eq!(session.pane_id_in_window(2, 0), Some(next_pane_id));
    assert_eq!(session.pane_id_in_window(3, 0), Some(trailing_pane_id));
    assert_eq!(session.active_window_index(), 0);
}

#[tokio::test]
async fn move_window_reindex_starts_at_base_index() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 3).await;
    insert_window(&handler, &alpha, 7).await;

    let set_base_index = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha.clone()),
            option: OptionName::BaseIndex,
            value: "2".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_base_index, Response::SetOption(_)));

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: None,
            target: MoveWindowTarget::Session(alpha.clone()),
            renumber: true,
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
            target: None,
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
}

#[tokio::test]
async fn move_window_reindex_remaps_window_metadata() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 2).await;
    insert_window(&handler, &alpha, 3).await;

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 3)),
                option: OptionName::WindowStyle,
                value: "fg=colour3".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetHook(rmux_proto::SetHookRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 3)),
                hook: HookName::WindowLayoutChanged,
                command: "display-message remapped".to_owned(),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await,
        Response::SetHook(_)
    ));

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: None,
            target: MoveWindowTarget::Session(alpha.clone()),
            renumber: true,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)));

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .options
            .resolve_for_window(&alpha, 2, OptionName::WindowStyle),
        Some("fg=colour3")
    );
    assert_eq!(
        state.hooks.window_command(
            &WindowTarget::with_window(alpha, 2),
            HookName::WindowLayoutChanged
        ),
        Some("display-message remapped")
    );
}

#[tokio::test]
async fn move_window_across_sessions_restores_terminal_ownership_when_resize_fails() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &beta, 4).await;

    let (moved_pane_id, replaced_pane_id) = {
        let state = handler.state.lock().await;
        let alpha_session = state.sessions.session(&alpha).expect("alpha should exist");
        let beta_session = state.sessions.session(&beta).expect("beta should exist");
        (
            alpha_session
                .window_at(1)
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("alpha window 1 pane should exist"),
            beta_session
                .window_at(4)
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("beta window 4 pane should exist"),
        )
    };

    {
        let mut state = handler.state.lock().await;
        state.fail_next_resize_for_test();
    }

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 1)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(beta.clone(), 4)),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Server(
                "injected pane terminal resize failure".to_owned()
            ),
        })
    );

    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should exist");
    let beta_session = state.sessions.session(&beta).expect("beta should exist");
    assert_eq!(
        alpha_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 4]
    );
    assert_eq!(alpha_session.pane_id_in_window(1, 0), Some(moved_pane_id));
    assert_eq!(beta_session.pane_id_in_window(4, 0), Some(replaced_pane_id));
    state
        .pane_profile_in_window(&alpha, 1, 0)
        .expect("moved pane terminal should return to the source session");
    state
        .pane_profile_in_window(&beta, 4, 0)
        .expect("replaced pane terminal should return to the destination session");
}
