use super::*;

#[tokio::test]
async fn swap_window_with_d_selects_the_swapped_slots_across_sessions() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 2).await;
    insert_window(&handler, &beta, 4).await;

    // Both sessions have active_window at 0 by default.
    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(beta.clone(), 4),
            detached: true,
        }))
        .await;

    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(beta.clone(), 4),
        })
    );

    // tmux cmd-swap-window.c selects the source/target winlinks when -d is
    // present.
    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should exist");
    let beta_session = state.sessions.session(&beta).expect("beta should exist");
    assert_eq!(alpha_session.active_window_index(), 2);
    assert_eq!(alpha_session.last_window_index(), Some(0));
    assert_eq!(beta_session.active_window_index(), 4);
    assert_eq!(beta_session.last_window_index(), Some(0));
}

#[tokio::test]
async fn swap_window_without_d_preserves_active_slots_across_sessions() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 2).await;
    insert_window(&handler, &beta, 4).await;

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(beta.clone(), 4),
            detached: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(beta.clone(), 4),
        })
    );

    // Without -d, tmux preserves the current winlinks; only their contents are
    // swapped.
    let state = handler.state.lock().await;
    let alpha_session = state.sessions.session(&alpha).expect("alpha should exist");
    let beta_session = state.sessions.session(&beta).expect("beta should exist");
    assert_eq!(alpha_session.active_window_index(), 0);
    assert_eq!(alpha_session.last_window_index(), None);
    assert_eq!(beta_session.active_window_index(), 0);
    assert_eq!(beta_session.last_window_index(), None);
}

#[tokio::test]
async fn swap_window_across_sessions_swaps_linked_slot_metadata() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    create_session(&handler, "gamma").await;

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
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 0),
            detached: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 0),
        })
    );

    {
        let state = handler.state.lock().await;
        assert_eq!(state.window_link_count(&alpha, 0), 1);
        assert_eq!(state.window_link_count(&beta, 0), 2);
        assert_eq!(state.window_link_count(&gamma, 1), 2);
        assert_eq!(
            state.window_linked_sessions_list(&gamma, 1),
            vec![beta.clone(), gamma.clone()]
        );
    }

    let rename = handler
        .handle(Request::RenameWindow(RenameWindowRequest {
            target: WindowTarget::with_window(gamma.clone(), 1),
            name: "logs".to_owned(),
        }))
        .await;
    assert!(matches!(rename, Response::RenameWindow(_)));

    let state = handler.state.lock().await;
    assert_ne!(
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.name()),
        Some("logs")
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.name()),
        Some("logs")
    );
    assert_eq!(
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.name()),
        Some("logs")
    );
}

#[tokio::test]
async fn swap_window_from_linked_slot_preserves_runtime_owners() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    create_session(&handler, "gamma").await;

    let linked_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("alpha pane should exist")
    };
    let gamma_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("gamma pane should exist")
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
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(beta.clone(), 1),
            target: WindowTarget::with_window(gamma.clone(), 0),
            detached: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(beta.clone(), 1),
            target: WindowTarget::with_window(gamma.clone(), 0),
        })
    );

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(linked_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(gamma_pane_id)
    );
    state
        .pane_profile_in_window(&gamma, 0, 0)
        .expect("linked window pane should remain reachable after swap");
    state
        .pane_profile_in_window(&beta, 1, 0)
        .expect("unlinked target pane should move to the source slot runtime");
    assert_eq!(state.window_link_count(&alpha, 0), 2);
    assert_eq!(state.window_link_count(&gamma, 0), 2);
    assert_eq!(state.window_link_count(&beta, 1), 1);
}

#[tokio::test]
async fn swap_window_from_group_peer_swaps_runtime_state() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;
    create_session(&handler, "gamma").await;

    let grouped_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("grouped pane should exist")
    };
    let gamma_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("gamma pane should exist")
    };

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(beta.clone(), 0),
            target: WindowTarget::with_window(gamma.clone(), 0),
            detached: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(beta.clone(), 0),
            target: WindowTarget::with_window(gamma.clone(), 0),
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
        Some(gamma_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(gamma_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(grouped_pane_id)
    );
    state
        .pane_profile_in_window(&beta, 0, 0)
        .expect("swapped grouped pane terminal should live in the group runtime");
    state
        .pane_profile_in_window(&gamma, 0, 0)
        .expect("swapped target pane terminal should live in gamma");
}

#[tokio::test]
async fn swap_window_from_group_peer_linked_source_moves_link_metadata_to_target() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    let delta = session_name("delta");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;
    create_session(&handler, "gamma").await;
    create_session(&handler, "delta").await;

    let (linked_pane_id, delta_pane_id) = {
        let state = handler.state.lock().await;
        (
            state
                .sessions
                .session(&beta)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("grouped pane should exist"),
            state
                .sessions
                .session(&delta)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("delta pane should exist"),
        )
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
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(beta.clone(), 0),
            target: WindowTarget::with_window(delta.clone(), 0),
            detached: true,
        }))
        .await;
    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(beta.clone(), 0),
            target: WindowTarget::with_window(delta.clone(), 0),
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
        Some(delta_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(delta_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&delta)
            .and_then(|session| session.window_at(0))
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
        .pane_profile_in_window(&delta, 0, 0)
        .expect("linked source pane should move to target runtime");
    state
        .pane_profile_in_window(&gamma, 1, 0)
        .expect("surviving linked peer should keep runtime access");
    assert_eq!(state.window_link_count(&alpha, 0), 1);
    assert_eq!(state.window_link_count(&delta, 0), 2);
    assert_eq!(state.window_link_count(&gamma, 1), 2);
}

#[tokio::test]
async fn swap_window_from_group_peer_linked_target_moves_link_metadata_to_source_family() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    let delta = session_name("delta");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;
    create_session(&handler, "gamma").await;
    create_session(&handler, "delta").await;

    let (grouped_pane_id, linked_pane_id) = {
        let state = handler.state.lock().await;
        (
            state
                .sessions
                .session(&beta)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("grouped pane should exist"),
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
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(beta.clone(), 0),
            target: WindowTarget::with_window(gamma.clone(), 0),
            detached: true,
        }))
        .await;
    assert_eq!(
        response,
        Response::SwapWindow(rmux_proto::SwapWindowResponse {
            source: WindowTarget::with_window(beta.clone(), 0),
            target: WindowTarget::with_window(gamma.clone(), 0),
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
        Some(linked_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(linked_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&gamma)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id()),
        Some(grouped_pane_id)
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
        .pane_profile_in_window(&alpha, 0, 0)
        .expect("linked target pane should move to the source family runtime");
    state
        .pane_profile_in_window(&delta, 1, 0)
        .expect("surviving linked peer should keep runtime access");
    assert_eq!(state.window_link_count(&alpha, 0), 2);
    assert_eq!(state.window_link_count(&beta, 0), 2);
    assert_eq!(state.window_link_count(&delta, 1), 2);
    assert_eq!(state.window_link_count(&gamma, 0), 1);
}

#[tokio::test]
async fn rotate_window_updates_the_active_pane_after_reordering_the_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    let previous_pane_ids = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .expect("alpha should exist")
            .window_at(0)
            .expect("window 0 should exist")
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>()
    };

    let response = handler
        .handle(Request::RotateWindow(RotateWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            direction: RotateWindowDirection::Up,
            restore_zoom: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::RotateWindow(rmux_proto::RotateWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 0),
        })
    );

    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&alpha)
        .expect("alpha should exist")
        .window_at(0)
        .expect("window 0 should exist");
    assert_eq!(window.active_pane_index(), 2);
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>(),
        vec![
            previous_pane_ids[1],
            previous_pane_ids[2],
            previous_pane_ids[0]
        ]
    );
}

#[tokio::test]
async fn rotate_window_down_selects_the_previous_pane_in_window_order() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    let previous_pane_ids = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .expect("alpha should exist")
            .window_at(0)
            .expect("window 0 should exist")
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>()
    };

    let response = handler
        .handle(Request::RotateWindow(RotateWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            direction: RotateWindowDirection::Down,
            restore_zoom: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::RotateWindow(rmux_proto::RotateWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 0),
        })
    );

    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&alpha)
        .expect("alpha should exist")
        .window_at(0)
        .expect("window 0 should exist");
    assert_eq!(window.active_pane_index(), 2);
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>(),
        vec![
            previous_pane_ids[2],
            previous_pane_ids[0],
            previous_pane_ids[1]
        ]
    );
}

#[tokio::test]
async fn move_window_rejects_nonexistent_source() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 99)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 5)),
            renumber: false,
            kill_destination: false,
            detached: false,
            after: false,
            before: false,
        }))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn swap_window_rejects_nonexistent_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(alpha.clone(), 99),
            detached: false,
        }))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn rotate_window_rejects_nonexistent_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::RotateWindow(RotateWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 99),
            direction: RotateWindowDirection::Up,
            restore_zoom: false,
        }))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn move_window_same_source_and_destination_is_noop_without_kill() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
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
            session_name: alpha.clone(),
            target: Some(WindowTarget::with_window(alpha.clone(), 0)),
        })
    );
}

#[tokio::test]
async fn move_window_same_index_noop_does_not_consume_link_hooks() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    {
        let mut state = handler.state.lock().await;
        state
            .hooks
            .set(
                ScopeSelector::Global,
                HookName::WindowUnlinked,
                "true".to_owned(),
                HookLifecycle::OneShot,
            )
            .expect("window-unlinked hook set succeeds");
        state
            .hooks
            .set(
                ScopeSelector::Global,
                HookName::WindowLinked,
                "true".to_owned(),
                HookLifecycle::OneShot,
            )
            .expect("window-linked hook set succeeds");
    }

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
            renumber: false,
            kill_destination: false,
            detached: false,
            after: false,
            before: false,
        }))
        .await;

    assert!(matches!(response, Response::MoveWindow(_)));
    let state = handler.state.lock().await;
    assert_eq!(
        state.hooks.global_command(HookName::WindowUnlinked),
        Some("true")
    );
    assert_eq!(
        state.hooks.global_command(HookName::WindowLinked),
        Some("true")
    );
}

#[tokio::test]
async fn move_window_same_source_and_destination_with_kill_reports_same_index() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
            renumber: false,
            kill_destination: true,
            detached: false,
            after: false,
            before: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Server("same index: 0".to_owned()),
        })
    );
}
