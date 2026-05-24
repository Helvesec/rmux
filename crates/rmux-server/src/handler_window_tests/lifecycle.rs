use super::*;

#[tokio::test]
async fn new_window_detached_leaves_the_active_window_unchanged() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::NewWindow(NewWindowRequest {
            target: alpha.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            target_window_index: None,
            insert_at_target: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::NewWindow(rmux_proto::NewWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 1),
        })
    );

    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&alpha)
        .expect("session should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(session.active_window_index(), 0);
    assert_eq!(session.last_window_index(), None);
}

#[tokio::test]
async fn select_window_updates_last_window_tracking() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;

    let response = handler
        .handle(Request::SelectWindow(SelectWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 1),
        }))
        .await;

    assert_eq!(
        response,
        Response::SelectWindow(rmux_proto::SelectWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 1),
        })
    );

    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&alpha)
        .expect("session should exist");
    assert_eq!(session.active_window_index(), 1);
    assert_eq!(session.last_window_index(), Some(0));
}

#[tokio::test]
async fn rename_window_persists_the_name_and_disables_automatic_rename() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;

    let response = handler
        .handle(Request::RenameWindow(RenameWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 1),
            name: "logs".to_owned(),
        }))
        .await;

    assert_eq!(
        response,
        Response::RenameWindow(rmux_proto::RenameWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 1),
        })
    );

    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&alpha)
        .expect("session should exist")
        .window_at(1)
        .expect("window should exist");
    assert_eq!(window.name(), Some("logs"));
    assert!(!window.automatic_rename());
}

#[tokio::test]
async fn kill_window_prefers_last_window_as_the_active_fallback() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;

    {
        let mut state = handler.state.lock().await;
        let session = state
            .sessions
            .session_mut(&alpha)
            .expect("session should exist");
        session.select_window(2).expect("window 2 select succeeds");
        session.select_window(1).expect("window 1 select succeeds");
    }
    let (removed_pane_id, surviving_pane_id) = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&alpha)
            .expect("session should exist");
        (
            session
                .window_at(1)
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("removed window has a pane"),
            session
                .window_at(2)
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id())
                .expect("surviving window has a pane"),
        )
    };
    let now = std::time::Instant::now();
    assert_eq!(
        handler.observe_pane_snapshot_revision(removed_pane_id, 1, now),
        Some(1)
    );
    assert_eq!(
        handler.observe_pane_snapshot_revision(surviving_pane_id, 9, now),
        Some(9)
    );

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 1),
            kill_all_others: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::KillWindow(rmux_proto::KillWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 2),
        })
    );

    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&alpha)
        .expect("session should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 2]
    );
    assert_eq!(session.active_window_index(), 2);
    assert_eq!(session.last_window_index(), None);
    drop(state);
    assert_eq!(
        handler.last_emitted_pane_snapshot_revision(removed_pane_id),
        None
    );
    assert_eq!(
        handler.last_emitted_pane_snapshot_revision(surviving_pane_id),
        Some(9)
    );
}

#[tokio::test]
async fn kill_window_falls_back_to_previous_then_next_when_needed() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;

    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .session_mut(&alpha)
            .expect("session should exist")
            .select_window(2)
            .expect("window 2 select succeeds");
    }

    assert_eq!(
        handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 0),
                kill_all_others: false,
            }))
            .await,
        Response::KillWindow(rmux_proto::KillWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 2),
        })
    );

    assert_eq!(
        handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 2),
                kill_all_others: false,
            }))
            .await,
        Response::KillWindow(rmux_proto::KillWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 1),
        })
    );

    insert_window(&handler, &alpha, 2).await;

    {
        let mut state = handler.state.lock().await;
        let session = state
            .sessions
            .session_mut(&alpha)
            .expect("session should exist");
        session.select_window(1).expect("window 1 select succeeds");
        session.select_window(2).expect("window 2 select succeeds");
    }

    assert_eq!(
        handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 1),
                kill_all_others: false,
            }))
            .await,
        Response::KillWindow(rmux_proto::KillWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 2),
        })
    );

    let beta = session_name("beta");
    create_session(&handler, "beta").await;
    insert_window(&handler, &beta, 2).await;

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(beta.clone(), 0),
            kill_all_others: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::KillWindow(rmux_proto::KillWindowResponse {
            target: WindowTarget::with_window(beta.clone(), 2),
        })
    );
}

#[tokio::test]
async fn kill_window_all_others_leaves_only_the_target_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 1),
            kill_all_others: true,
        }))
        .await;

    assert_eq!(
        response,
        Response::KillWindow(rmux_proto::KillWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 1),
        })
    );

    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&alpha)
        .expect("session should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(session.active_window_index(), 1);
}

#[tokio::test]
async fn new_window_reuses_the_lowest_available_index_after_kill() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    assert_eq!(
        handler
            .handle(Request::NewWindow(NewWindowRequest {
                target: alpha.clone(),
                name: None,
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                target_window_index: None,
                insert_at_target: false,
            }))
            .await,
        Response::NewWindow(rmux_proto::NewWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 1),
        })
    );

    assert_eq!(
        handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 0),
                kill_all_others: false,
            }))
            .await,
        Response::KillWindow(rmux_proto::KillWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 1),
        })
    );

    let response = handler
        .handle(Request::NewWindow(NewWindowRequest {
            target: alpha.clone(),
            name: Some("reused".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            target_window_index: None,
            insert_at_target: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::NewWindow(rmux_proto::NewWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 0),
        })
    );

    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&alpha)
        .expect("session should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        session.window_at(0).and_then(|window| window.name()),
        Some("reused")
    );
}

#[tokio::test]
async fn new_window_does_not_mutate_the_session_when_existing_terminals_are_missing() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let removed_pane_id = {
        let mut state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&alpha)
            .expect("session should exist")
            .window()
            .pane(0)
            .expect("pane 0 should exist")
            .id();
        assert!(state.remove_pane_terminal(&alpha, pane_id));
        pane_id
    };

    let response = handler
        .handle(Request::NewWindow(NewWindowRequest {
            target: alpha.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            target_window_index: None,
            insert_at_target: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Server(format!(
                "missing pane terminal for pane id {} in session {}",
                removed_pane_id.as_u32(),
                alpha
            )),
        })
    );

    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&alpha)
        .expect("session should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(session.active_window_index(), 0);
    assert_eq!(session.last_window_index(), None);
}

#[tokio::test]
async fn killing_the_only_window_returns_an_explicit_error() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(alpha, 0),
            kill_all_others: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Server(
                "cannot kill the only window in session alpha".to_owned(),
            ),
        })
    );
}

#[tokio::test]
async fn kill_window_all_others_prevalidates_the_full_removal_set() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;

    let (window_zero_pane_id, missing_pane_id) = {
        let mut state = handler.state.lock().await;
        let (window_zero_pane_id, missing_pane_id) = {
            let session = state
                .sessions
                .session(&alpha)
                .expect("session should exist");
            (
                session
                    .window_at(0)
                    .expect("window 0 should exist")
                    .pane(0)
                    .expect("pane 0 should exist")
                    .id(),
                session
                    .window_at(2)
                    .expect("window 2 should exist")
                    .pane(0)
                    .expect("pane 0 should exist")
                    .id(),
            )
        };
        assert!(state.remove_pane_terminal(&alpha, missing_pane_id));
        (window_zero_pane_id, missing_pane_id)
    };

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 1),
            kill_all_others: true,
        }))
        .await;

    assert_eq!(
        response,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Server(format!(
                "missing pane terminal for pane id {} in session {}",
                missing_pane_id.as_u32(),
                alpha
            )),
        })
    );

    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&alpha)
        .expect("session should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(session.active_window_index(), 0);
    assert_eq!(session.last_window_index(), None);
    state
        .ensure_panes_exist(&alpha, &[window_zero_pane_id])
        .expect("window 0 pane terminal should remain intact");
}

#[tokio::test]
async fn kill_window_cleans_grouped_member_window_metadata_before_synchronizing() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;

    let grouped = handler
        .handle(Request::NewSessionExt(NewSessionExtRequest {
            session_name: Some(beta.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),
            environment: None,
            group_target: Some(alpha.clone()),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            passthrough: false,
        }))
        .await;
    assert!(matches!(grouped, Response::NewSession(_)));

    let alpha_target = WindowTarget::with_window(alpha.clone(), 1);
    let beta_target = WindowTarget::with_window(beta.clone(), 1);
    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Window(beta_target.clone()),
                OptionName::AutomaticRename,
                "off".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("window option set succeeds");
        state
            .hooks
            .set(
                ScopeSelector::Window(beta_target.clone()),
                HookName::WindowLayoutChanged,
                "display-message beta".to_owned(),
                HookLifecycle::Persistent,
            )
            .expect("window hook set succeeds");
        state.mark_auto_named_window(&beta, 1);

        assert_eq!(
            state
                .options
                .window_value(&beta_target, OptionName::AutomaticRename),
            Some("off")
        );
        assert_eq!(
            state
                .hooks
                .window_command(&beta_target, HookName::WindowLayoutChanged),
            Some("display-message beta")
        );
        assert!(state.tracks_auto_named_window(&beta, 1));
    }

    let killed = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: alpha_target.clone(),
            kill_all_others: false,
        }))
        .await;
    assert_eq!(
        killed,
        Response::KillWindow(rmux_proto::KillWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 0),
        })
    );

    let state = handler.state.lock().await;
    assert!(
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(1))
            .is_none(),
        "killed window should be absent from source session"
    );
    assert!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(1))
            .is_none(),
        "grouped session listing should be synchronized after kill-window"
    );
    assert_eq!(
        state
            .options
            .window_value(&beta_target, OptionName::AutomaticRename),
        None
    );
    assert_eq!(
        state
            .hooks
            .window_command(&beta_target, HookName::WindowLayoutChanged),
        None
    );
    assert!(!state.tracks_auto_named_window(&beta, 1));
}
