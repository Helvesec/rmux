use super::*;

#[tokio::test]
async fn new_window_detached_leaves_the_active_window_unchanged() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
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
async fn kill_window_removes_latest_client_state_for_removed_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("latest-kill-window");
    let requester_pid = std::process::id();
    create_session(&handler, "latest-kill-window").await;
    insert_window(&handler, &alpha, 1).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    {
        let mut active_attach = handler.active_attach.lock().await;
        active_attach.seed_active_client_for_window(requester_pid, &alpha, 1);
    }

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 1),
            kill_all_others: false,
        }))
        .await;
    assert!(matches!(response, Response::KillWindow(_)), "{response:?}");

    let active_attach = handler.active_attach.lock().await;
    let windows = active_attach
        .active_client_by_window
        .get(&alpha)
        .expect("remaining window latest state survives");
    assert_eq!(windows.get(&0), Some(&requester_pid));
    assert_eq!(windows.get(&1), None);
}

#[tokio::test]
async fn named_new_window_disables_automatic_rename_option() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha-named-new-window");
    create_session(&handler, "alpha-named-new-window").await;

    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: Some("logs".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;

    assert_eq!(
        response,
        Response::NewWindow(rmux_proto::NewWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 1),
        })
    );

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .options
            .resolve_for_window(&alpha, 1, OptionName::AutomaticRename),
        Some("off")
    );
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
    assert_eq!(
        state
            .options
            .resolve_for_window(&alpha, 1, OptionName::AutomaticRename),
        Some("off")
    );
}

#[tokio::test]
async fn rename_window_propagates_linked_slots_to_their_session_group_peers() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    let delta = session_name("delta");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;
    create_session(&handler, "gamma").await;
    create_grouped_session(&handler, "delta", &gamma).await;

    let link = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(gamma.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: false,
        }))
        .await;
    assert!(
        matches!(link, Response::LinkWindow(_)),
        "expected link-window success, got {link:?}"
    );

    let response = handler
        .handle(Request::RenameWindow(RenameWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            name: "newname".to_owned(),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameWindow(_)),
        "expected rename-window success, got {response:?}"
    );

    let state = handler.state.lock().await;
    for (session_name, window_index) in [(&alpha, 0), (&beta, 0), (&gamma, 1), (&delta, 1)] {
        let window = state
            .sessions
            .session(session_name)
            .and_then(|session| session.window_at(window_index))
            .expect("linked window should exist");
        assert_eq!(
            window.name(),
            Some("newname"),
            "{session_name}:{window_index} should reflect linked rename"
        );
    }
}

#[tokio::test]
async fn rename_window_from_session_group_peer_propagates_linked_family() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    let delta = session_name("delta");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;
    create_session(&handler, "gamma").await;
    create_grouped_session(&handler, "delta", &gamma).await;

    let link = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(gamma.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: false,
        }))
        .await;
    assert!(
        matches!(link, Response::LinkWindow(_)),
        "expected link-window success, got {link:?}"
    );

    let response = handler
        .handle(Request::RenameWindow(RenameWindowRequest {
            target: WindowTarget::with_window(beta.clone(), 0),
            name: "peername".to_owned(),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameWindow(_)),
        "expected rename-window success, got {response:?}"
    );

    let state = handler.state.lock().await;
    for (session_name, window_index) in [(&alpha, 0), (&beta, 0), (&gamma, 1), (&delta, 1)] {
        let window = state
            .sessions
            .session(session_name)
            .and_then(|session| session.window_at(window_index))
            .expect("linked window should exist");
        assert_eq!(
            window.name(),
            Some("peername"),
            "{session_name}:{window_index} should reflect linked rename"
        );
    }
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
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: alpha.clone(),
                name: None,
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
                target_window_index: None,
                insert_at_target: false,
            })))
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
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: Some("reused".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
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
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(alpha.clone(), 0, 0))
        .await;

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
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
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
async fn killing_the_only_window_atomically_destroys_its_session_in_tmux_hook_order() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            kill_all_others: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::KillWindow(rmux_proto::KillWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 0),
        })
    );
    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .is_none());

    let first = timeout(Duration::from_secs(1), events.recv())
        .await
        .expect("window-unlinked event should arrive")
        .expect("lifecycle channel should stay open");
    let second = timeout(Duration::from_secs(1), events.recv())
        .await
        .expect("session-closed event should arrive")
        .expect("lifecycle channel should stay open");
    assert!(
        matches!(
            &first.event,
            rmux_core::LifecycleEvent::WindowUnlinked {
                session_name,
                target: Some(target),
                ..
            } if session_name == &alpha && target == &WindowTarget::with_window(alpha.clone(), 0)
        ),
        "{first:?}"
    );
    assert!(
        matches!(
            &second.event,
            rmux_core::LifecycleEvent::SessionClosed {
                session_name,
                ..
            } if session_name == &alpha
        ),
        "{second:?}"
    );
}

#[tokio::test]
async fn kill_last_window_commit_excludes_a_concurrent_new_window() {
    let handler = std::sync::Arc::new(RequestHandler::new());
    let alpha = session_name("kill-last-window-race");
    create_session(&handler, alpha.as_str()).await;
    let target = WindowTarget::with_window(alpha.clone(), 0);
    let pause = handler.install_kill_window_commit_pause(target.clone());

    let killing_handler = std::sync::Arc::clone(&handler);
    let killing_target = target.clone();
    let killing = tokio::spawn(async move {
        killing_handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: killing_target,
                kill_all_others: false,
            }))
            .await
    });
    timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("kill-window should reach the in-lock commit barrier");

    let creating_handler = std::sync::Arc::clone(&handler);
    let creating_target = alpha.clone();
    let mut creating = tokio::spawn(async move {
        creating_handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: creating_target,
                name: None,
                detached: true,
                environment: None,
                command: None,
                start_directory: None,
                target_window_index: None,
                insert_at_target: false,
                process_command: None,
            })))
            .await
    });
    assert!(
        timeout(Duration::from_millis(50), &mut creating)
            .await
            .is_err(),
        "new-window must wait behind the kill-window state transaction"
    );

    pause.release.notify_one();
    assert!(matches!(
        timeout(Duration::from_secs(1), killing)
            .await
            .expect("kill-window should leave the barrier")
            .expect("kill-window task should join"),
        Response::KillWindow(_)
    ));
    assert!(matches!(
        timeout(Duration::from_secs(1), creating)
            .await
            .expect("new-window should resume after the committed kill")
            .expect("new-window task should join"),
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::SessionNotFound(_),
        })
    ));
}

#[tokio::test]
async fn kill_last_linked_window_orders_target_session_before_surviving_alias() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha-linked-last");
    let beta = session_name("beta-linked-last");
    create_session(&handler, alpha.as_str()).await;
    create_session(&handler, beta.as_str()).await;
    insert_window(&handler, &beta, 1).await;
    assert!(matches!(
        handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(alpha.clone(), 0),
                target: WindowTarget::with_window(beta.clone(), 2),
                after: false,
                before: false,
                kill_destination: false,
                detached: false,
            }))
            .await,
        Response::LinkWindow(_)
    ));
    let mut events = handler.subscribe_lifecycle_events();

    assert!(matches!(
        handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 0),
                kill_all_others: false,
            }))
            .await,
        Response::KillWindow(_)
    ));

    let mut observed = Vec::new();
    for _ in 0..3 {
        let queued = timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("linked kill lifecycle event should arrive")
            .expect("lifecycle channel should stay open");
        observed.push(match queued.event {
            rmux_core::LifecycleEvent::WindowUnlinked { session_name, .. } => {
                format!("window:{session_name}")
            }
            rmux_core::LifecycleEvent::SessionClosed { session_name, .. } => {
                format!("session:{session_name}")
            }
            other => panic!("unexpected lifecycle event: {other:?}"),
        });
    }
    assert_eq!(
        observed,
        [
            "window:alpha-linked-last",
            "session:alpha-linked-last",
            "window:beta-linked-last",
        ]
    );

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&alpha).is_none());
    let beta_session = state.sessions.session(&beta).expect("beta should survive");
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[tokio::test]
async fn kill_last_grouped_window_matches_tmux_peer_hook_order() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha-grouped-last");
    let beta = session_name("beta-grouped-last");
    create_session(&handler, alpha.as_str()).await;
    create_grouped_session(&handler, beta.as_str(), &alpha).await;
    create_session(&handler, "survivor-grouped-last").await;
    let mut events = handler.subscribe_lifecycle_events();

    assert!(matches!(
        handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 0),
                kill_all_others: false,
            }))
            .await,
        Response::KillWindow(_)
    ));

    let mut observed = Vec::new();
    for _ in 0..4 {
        let queued = timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("grouped kill lifecycle event should arrive")
            .expect("lifecycle channel should stay open");
        observed.push(match queued.event {
            rmux_core::LifecycleEvent::WindowUnlinked { session_name, .. } => {
                format!("window:{session_name}")
            }
            rmux_core::LifecycleEvent::SessionClosed { session_name, .. } => {
                format!("session:{session_name}")
            }
            other => panic!("unexpected lifecycle event: {other:?}"),
        });
    }
    assert_eq!(
        observed,
        [
            "window:alpha-grouped-last",
            "session:alpha-grouped-last",
            "session:beta-grouped-last",
            "window:beta-grouped-last",
        ]
    );
    let state = handler.state.lock().await;
    assert!(state.sessions.session(&alpha).is_none());
    assert!(state.sessions.session(&beta).is_none());
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
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
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
            client_environment: None,
            skip_environment_update: false,
        })))
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
