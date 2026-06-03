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
