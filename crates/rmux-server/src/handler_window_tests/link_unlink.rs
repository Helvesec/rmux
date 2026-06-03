use super::*;

#[tokio::test]
async fn link_window_shares_runtime_tracks_linked_sessions_and_unlinks_cleanly() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: false,
        }))
        .await;

    assert!(
        matches!(&response, Response::LinkWindow(r) if r.target == WindowTarget::with_window(beta.clone(), 1)),
        "expected link-window success, got {response:?}"
    );

    {
        let state = handler.state.lock().await;
        let alpha_window = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .expect("alpha window 0 should exist");
        let beta_window = state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(1))
            .expect("beta window 1 should exist");

        assert_eq!(alpha_window.id(), beta_window.id());
        assert_eq!(state.window_link_count(&alpha, 0), 2);
        assert_eq!(state.window_linked_session_count(&alpha, 0), 2);
        assert_eq!(
            state.window_linked_sessions_list(&alpha, 0),
            vec![alpha.clone(), beta.clone()]
        );
        assert!(
            state.pane_profile_in_window(&beta, 1, 0).is_ok(),
            "linked target should resolve pane runtime through the shared terminal owner"
        );
    }

    let linked_formats = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Window(WindowTarget::with_window(alpha.clone(), 0))),
            print: true,
            message: Some(
                "#{window_linked}:#{window_linked_sessions}:#{window_linked_sessions_list}"
                    .to_owned(),
            ),
        }))
        .await
        .command_output()
        .expect("window linked format output")
        .stdout()
        .to_vec();
    assert_eq!(String::from_utf8_lossy(&linked_formats), "1:2:alpha,beta\n");

    let rename = handler
        .handle(Request::RenameWindow(RenameWindowRequest {
            target: WindowTarget::with_window(beta.clone(), 1),
            name: "logs".to_owned(),
        }))
        .await;
    assert!(
        matches!(&rename, Response::RenameWindow(r) if r.target == WindowTarget::with_window(beta.clone(), 1)),
        "expected rename-window success, got {rename:?}"
    );

    {
        let state = handler.state.lock().await;
        let alpha_window = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .expect("alpha window 0 should exist after rename");
        let beta_window = state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(1))
            .expect("beta window 1 should exist after rename");

        assert_eq!(alpha_window.name(), Some("logs"));
        assert_eq!(beta_window.name(), Some("logs"));
    }

    let unlink = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: WindowTarget::with_window(beta.clone(), 1),
            kill_if_last: false,
        }))
        .await;
    assert!(
        matches!(&unlink, Response::UnlinkWindow(r) if r.target == WindowTarget::with_window(beta.clone(), 0)),
        "expected unlink-window success, got {unlink:?}"
    );

    let state = handler.state.lock().await;
    assert_eq!(state.window_link_count(&alpha, 0), 1);
    assert_eq!(state.window_linked_session_count(&alpha, 0), 1);
    assert_eq!(
        state.window_linked_sessions_list(&alpha, 0),
        vec![alpha.clone()]
    );
    assert!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(1))
            .is_none(),
        "unlink-window should remove the target slot from beta"
    );
    assert!(
        state.pane_profile_in_window(&beta, 1, 0).is_err(),
        "unlinked target slot should no longer resolve pane runtime"
    );
}

#[tokio::test]
async fn linked_windows_survive_runtime_owner_session_rename() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;

    assert!(matches!(
        handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(alpha.clone(), 0),
                target: WindowTarget::with_window(beta.clone(), 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: false,
            }))
            .await,
        Response::LinkWindow(_)
    ));

    assert!(matches!(
        handler
            .handle(Request::RenameSession(RenameSessionRequest {
                target: alpha,
                new_name: gamma.clone(),
            }))
            .await,
        Response::RenameSession(_)
    ));

    {
        let state = handler.state.lock().await;
        assert_eq!(state.window_link_count(&gamma, 0), 2);
        assert_eq!(state.window_link_count(&beta, 1), 2);
        assert_eq!(
            state.window_linked_sessions_list(&beta, 1),
            vec![gamma.clone(), beta.clone()]
        );
        assert!(
            state.pane_profile_in_window(&beta, 1, 0).is_ok(),
            "linked target should still resolve through renamed runtime owner"
        );
    }

    let list = handler
        .handle(Request::ListPanes(ListPanesRequest {
            target: beta,
            target_window_index: Some(1),
            format: Some("#{session_name}:#{window_index}:#{pane_index}".to_owned()),
        }))
        .await;
    let Response::ListPanes(list) = list else {
        panic!("linked list-panes should survive owner rename, got {list:?}");
    };
    assert_eq!(String::from_utf8_lossy(list.output.stdout()), "beta:1:0\n");
}

#[tokio::test]
async fn linked_windows_survive_runtime_owner_session_removal_after_rename() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;

    assert!(matches!(
        handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(alpha.clone(), 0),
                target: WindowTarget::with_window(beta.clone(), 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: false,
            }))
            .await,
        Response::LinkWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::RenameSession(RenameSessionRequest {
                target: alpha,
                new_name: gamma.clone(),
            }))
            .await,
        Response::RenameSession(_)
    ));

    let kill = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: gamma.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
        }))
        .await;
    assert!(
        matches!(kill, Response::KillSession(_)),
        "expected kill-session success, got {kill:?}"
    );

    {
        let state = handler.state.lock().await;
        assert!(
            state.sessions.session(&gamma).is_none(),
            "runtime owner session should be removed"
        );
        assert_eq!(state.window_link_count(&beta, 1), 1);
        assert_eq!(
            state.window_linked_sessions_list(&beta, 1),
            vec![beta.clone()]
        );
        assert!(
            state.pane_profile_in_window(&beta, 1, 0).is_ok(),
            "surviving linked target should adopt the removed owner's pane runtime"
        );
    }

    let list = handler
        .handle(Request::ListPanes(ListPanesRequest {
            target: beta,
            target_window_index: Some(1),
            format: Some("#{session_name}:#{window_index}:#{pane_index}".to_owned()),
        }))
        .await;
    let Response::ListPanes(list) = list else {
        panic!("linked list-panes should survive owner removal, got {list:?}");
    };
    assert_eq!(String::from_utf8_lossy(list.output.stdout()), "beta:1:0\n");
}

#[tokio::test]
async fn link_window_shares_pane_base_index_with_linked_slots() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;

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
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 0)),
                option: OptionName::PaneBaseIndex,
                value: "1".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(alpha.clone(), 0),
                target: WindowTarget::with_window(beta.clone(), 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: false,
            }))
            .await,
        Response::LinkWindow(_)
    ));

    let list = handler
        .handle(Request::ListPanes(ListPanesRequest {
            target: beta.clone(),
            target_window_index: Some(1),
            format: Some("#{pane_index}".to_owned()),
        }))
        .await;
    let Response::ListPanes(list) = list else {
        panic!("linked list-panes should succeed, got {list:?}");
    };
    assert_eq!(
        String::from_utf8_lossy(list.output.stdout()),
        "1\n2\n",
        "linked windows should render the source pane-base-index"
    );

    let resolved = handler
        .handle(Request::ResolveTarget(ResolveTargetRequest {
            target: Some("beta:1.1".to_owned()),
            target_type: ResolveTargetType::Pane,
            window_index: false,
            prefer_unattached: false,
        }))
        .await;
    let Response::ResolveTarget(resolved) = resolved else {
        panic!("linked visible pane target should resolve, got {resolved:?}");
    };
    assert_eq!(
        resolved.target,
        Target::Pane(PaneTarget::with_window(beta, 1, 0))
    );
}

#[tokio::test]
async fn unlink_window_kill_if_last_deletes_an_unshared_window_slot() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;

    let response = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 1),
            kill_if_last: true,
        }))
        .await;

    assert!(
        matches!(&response, Response::UnlinkWindow(r) if r.target == WindowTarget::with_window(alpha.clone(), 0)),
        "expected unlink-window -k to remove the unshared slot, got {response:?}"
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert!(
        session.window_at(1).is_none(),
        "unlink-window -k should delete the unshared destination window"
    );
    assert_eq!(session.active_window_index(), 0);
}

#[tokio::test]
async fn unlink_window_restores_previous_last_window_flag_after_active_link_removal() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;

    assert!(matches!(
        handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 1),
            }))
            .await,
        Response::SelectWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 0),
            }))
            .await,
        Response::SelectWindow(_)
    ));

    assert!(matches!(
        handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(alpha.clone(), 0),
                target: WindowTarget::with_window(alpha.clone(), 9),
                after: false,
                before: false,
                kill_destination: false,
                detached: false,
            }))
            .await,
        Response::LinkWindow(_)
    ));
    {
        let state = handler.state.lock().await;
        assert_eq!(state.window_link_count(&alpha, 0), 2);
        assert_eq!(state.window_linked_session_count(&alpha, 0), 1);
        assert_eq!(
            state.window_linked_sessions_list(&alpha, 0),
            vec![alpha.clone()]
        );
    }
    assert!(matches!(
        handler
            .handle(Request::UnlinkWindow(UnlinkWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 9),
                kill_if_last: true,
            }))
            .await,
        Response::UnlinkWindow(_)
    ));

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(session.active_window_index(), 0);
    assert_eq!(session.last_window_index(), Some(1));
}
