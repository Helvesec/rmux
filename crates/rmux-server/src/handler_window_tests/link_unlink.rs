use super::*;

async fn assert_send_keys_succeeds(handler: &RequestHandler, target: PaneTarget) {
    let response = handler
        .handle(Request::SendKeys(rmux_proto::SendKeysRequest {
            target,
            keys: vec!["x".to_owned()],
        }))
        .await;
    assert!(matches!(response, Response::SendKeys(_)), "{response:?}");
}

#[tokio::test]
async fn link_window_refreshes_attached_non_syntactic_group_peer_output_receiver() {
    let handler = RequestHandler::new();
    let owner = session_name("linked-refresh-owner");
    let peer = session_name("linked-refresh-peer");
    let source = session_name("linked-refresh-source");
    create_session(&handler, owner.as_str()).await;
    create_grouped_session(&handler, peer.as_str(), &owner).await;
    create_session(&handler, source.as_str()).await;

    let source_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&source)
            .expect("source session exists")
            .window_at(0)
            .expect("source window exists")
            .active_pane()
            .expect("source active pane exists")
            .id()
    };

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler.register_attach(42, peer.clone(), control_tx).await;
    drain_attach_controls(&mut control_rx).await;

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source.clone(), 0),
            target: WindowTarget::with_window(owner, 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let control = timeout(Duration::from_secs(2), control_rx.recv())
        .await
        .expect("attached group peer must be refreshed after link-window")
        .expect("attached group peer control channel remains open");
    let AttachControl::Switch(mut target) = control else {
        panic!("expected attached group peer switch, got {control:?}");
    };
    assert_eq!(target.session_name, peer);

    let output = {
        let state = handler.state.lock().await;
        let peer_pane_id = state
            .sessions
            .session(&peer)
            .expect("peer session exists")
            .window_at(0)
            .expect("peer window exists")
            .active_pane()
            .expect("peer active pane exists")
            .id();
        assert_eq!(peer_pane_id, source_pane_id);
        state
            .pane_output_for_target(&peer, 0, 0)
            .expect("linked peer output exists")
            .clone()
    };
    let expected = b"linked-peer-live-output".to_vec();
    output.send(expected.clone());

    let received = timeout(Duration::from_secs(2), target.pane_output.recv())
        .await
        .expect("refreshed peer receiver must follow the linked pane runtime");
    let rmux_core::events::OutputCursorItem::Event(event) = received else {
        panic!("expected linked pane output, got {received:?}");
    };
    assert_eq!(event.bytes(), expected);
}

#[tokio::test]
async fn link_window_k_rejects_same_window_identity_through_group_peer_atomically() {
    let handler = RequestHandler::new();
    let owner = session_name("link-self-owner");
    let peer = session_name("link-self-peer");
    let external = session_name("link-self-external");
    create_session(&handler, owner.as_str()).await;
    create_grouped_session(&handler, peer.as_str(), &owner).await;
    create_session(&handler, external.as_str()).await;

    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(external.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");

    let (before_sessions, before_targets, stable_window_id) = {
        let state = handler.state.lock().await;
        let before_sessions = [&owner, &peer, &external]
            .into_iter()
            .map(|session_name| {
                state
                    .sessions
                    .session(session_name)
                    .expect("session exists before rejected replacement")
                    .clone()
            })
            .collect::<Vec<_>>();
        let before_targets = state.window_linked_window_targets(&owner, 0);
        let stable_window_id = state
            .sessions
            .session(&owner)
            .and_then(|session| session.window_at(0))
            .expect("runtime owner window exists")
            .id();
        assert_eq!(
            state
                .sessions
                .session(&peer)
                .and_then(|session| session.window_at(0))
                .expect("group peer window exists")
                .id(),
            stable_window_id
        );
        assert_eq!(
            state
                .sessions
                .session(&external)
                .and_then(|session| session.window_at(1))
                .expect("external linked window exists")
                .id(),
            stable_window_id
        );
        (before_sessions, before_targets, stable_window_id)
    };

    for target in [
        PaneTarget::with_window(owner.clone(), 0, 0),
        PaneTarget::with_window(peer.clone(), 0, 0),
        PaneTarget::with_window(external.clone(), 1, 0),
    ] {
        assert_send_keys_succeeds(&handler, target).await;
    }

    for source in [
        WindowTarget::with_window(peer.clone(), 0),
        WindowTarget::with_window(external.clone(), 1),
    ] {
        let response = handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: source.clone(),
                target: WindowTarget::with_window(owner.clone(), 0),
                after: false,
                before: false,
                kill_destination: true,
                detached: true,
            }))
            .await;
        assert!(
            matches!(response, Response::Error(_)),
            "same-WindowId replacement from {source} must fail atomically, got {response:?}"
        );
    }

    {
        let state = handler.state.lock().await;
        let after_sessions = [&owner, &peer, &external]
            .into_iter()
            .map(|session_name| {
                state
                    .sessions
                    .session(session_name)
                    .expect("session survives rejected replacement")
                    .clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(after_sessions, before_sessions);
        assert_eq!(
            state.window_linked_window_targets(&owner, 0),
            before_targets
        );
        assert_eq!(state.window_link_count(&owner, 0), 2);
        for target in [
            WindowTarget::with_window(owner.clone(), 0),
            WindowTarget::with_window(peer.clone(), 0),
            WindowTarget::with_window(external.clone(), 1),
        ] {
            assert_eq!(
                state
                    .sessions
                    .session(target.session_name())
                    .and_then(|session| session.window_at(target.window_index()))
                    .expect("all aliases survive rejected replacement")
                    .id(),
                stable_window_id
            );
        }
    }

    for target in [
        PaneTarget::with_window(owner, 0, 0),
        PaneTarget::with_window(peer, 0, 0),
        PaneTarget::with_window(external, 1, 0),
    ] {
        assert_send_keys_succeeds(&handler, target).await;
    }
}

#[tokio::test]
async fn link_window_k_between_distinct_grouped_window_ids_remains_supported() {
    let handler = RequestHandler::new();
    let owner = session_name("link-distinct-owner");
    let peer = session_name("link-distinct-peer");
    create_session(&handler, owner.as_str()).await;
    create_grouped_session(&handler, peer.as_str(), &owner).await;
    let created = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: owner.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: Some(quiet_window_test_command()),
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(created, Response::NewWindow(_)), "{created:?}");

    let source_window_id = {
        let state = handler.state.lock().await;
        let destination_window_id = state
            .sessions
            .session(&owner)
            .and_then(|session| session.window_at(0))
            .expect("destination window exists")
            .id();
        let source_window_id = state
            .sessions
            .session(&peer)
            .and_then(|session| session.window_at(1))
            .expect("grouped source window exists")
            .id();
        assert_ne!(source_window_id, destination_window_id);
        source_window_id
    };

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(peer.clone(), 1),
            target: WindowTarget::with_window(owner.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(
        matches!(response, Response::LinkWindow(_)),
        "distinct grouped WindowIds must remain replaceable, got {response:?}"
    );

    {
        let state = handler.state.lock().await;
        for target in [
            WindowTarget::with_window(owner.clone(), 0),
            WindowTarget::with_window(peer.clone(), 0),
            WindowTarget::with_window(owner.clone(), 1),
            WindowTarget::with_window(peer.clone(), 1),
        ] {
            assert_eq!(
                state
                    .sessions
                    .session(target.session_name())
                    .and_then(|session| session.window_at(target.window_index()))
                    .expect("linked grouped alias exists")
                    .id(),
                source_window_id
            );
            state
                .pane_profile_in_window(target.session_name(), target.window_index(), 0)
                .expect("linked grouped alias keeps runtime access");
        }
    }
    for target in [
        PaneTarget::with_window(owner.clone(), 0, 0),
        PaneTarget::with_window(peer.clone(), 0, 0),
        PaneTarget::with_window(owner, 1, 0),
        PaneTarget::with_window(peer, 1, 0),
    ] {
        assert_send_keys_succeeds(&handler, target).await;
    }
}

#[tokio::test]
async fn unlink_window_via_group_peer_refreshes_exact_family_and_removes_exact_timers() {
    let handler = RequestHandler::new();
    let monitor = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::MonitorSilence,
            value: "60".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(monitor, Response::SetOption(_)), "{monitor:?}");

    let owner = session_name("unlink-refresh-owner");
    let peer = session_name("unlink-refresh-peer");
    let external = session_name("unlink-refresh-external");
    create_session(&handler, owner.as_str()).await;
    create_grouped_session(&handler, peer.as_str(), &owner).await;
    create_session(&handler, external.as_str()).await;
    let created = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: owner.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: Some(quiet_window_test_command()),
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(created, Response::NewWindow(_)), "{created:?}");
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(external.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler.register_attach(43, owner.clone(), control_tx).await;
    drain_attach_controls(&mut control_rx).await;

    let removed_targets = [
        WindowTarget::with_window(owner.clone(), 0),
        WindowTarget::with_window(peer.clone(), 0),
    ];
    let preserved_targets = [
        WindowTarget::with_window(owner.clone(), 1),
        WindowTarget::with_window(peer.clone(), 1),
        WindowTarget::with_window(external.clone(), 0),
        WindowTarget::with_window(external.clone(), 1),
    ];
    for target in &removed_targets {
        assert!(
            handler.silence_timer_snapshot_for_test(target).is_some(),
            "removed alias starts with a silence timer: {target}"
        );
    }
    let preserved_timer_snapshots = preserved_targets
        .iter()
        .map(|target| handler.silence_timer_snapshot_for_test(target))
        .collect::<Vec<_>>();

    let response = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: WindowTarget::with_window(peer.clone(), 0),
            kill_if_last: false,
        }))
        .await;
    assert!(
        matches!(&response, Response::UnlinkWindow(result) if result.target == WindowTarget::with_window(peer.clone(), 1)),
        "expected grouped peer unlink success, got {response:?}"
    );

    let control = timeout(Duration::from_secs(2), control_rx.recv())
        .await
        .expect("non-syntactic owner attach must be refreshed after unlink-window")
        .expect("owner attach control channel remains open");
    let AttachControl::Switch(mut target) = control else {
        panic!("expected refreshed owner switch, got {control:?}");
    };
    assert_eq!(target.session_name, owner);

    let output = {
        let state = handler.state.lock().await;
        assert!(state
            .sessions
            .session(&owner)
            .and_then(|session| session.window_at(0))
            .is_none());
        assert!(state
            .sessions
            .session(&peer)
            .and_then(|session| session.window_at(0))
            .is_none());
        assert!(
            state
                .sessions
                .session(&external)
                .and_then(|session| session.window_at(1))
                .is_some(),
            "external linked alias survives grouped peer unlink"
        );
        state
            .pane_output_for_target(&owner, 1, 0)
            .expect("owner survivor output exists")
            .clone()
    };
    let expected = b"unlink-peer-live-output".to_vec();
    output.send(expected.clone());
    let received = timeout(Duration::from_secs(2), target.pane_output.recv())
        .await
        .expect("refreshed owner receiver follows the surviving active window");
    let rmux_core::events::OutputCursorItem::Event(event) = received else {
        panic!("expected surviving pane output, got {received:?}");
    };
    assert_eq!(event.bytes(), expected);

    for target in &removed_targets {
        assert_eq!(
            handler.silence_timer_snapshot_for_test(target),
            None,
            "unlink-window removes the vanished alias timer: {target}"
        );
    }
    for (target, snapshot) in preserved_targets.iter().zip(preserved_timer_snapshots) {
        assert_eq!(
            handler.silence_timer_snapshot_for_test(target),
            snapshot,
            "unlink-window must not postpone surviving or unrelated timer {target}"
        );
    }
}

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
            empty_target_context: false,
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
async fn linked_session_formats_include_session_group_peers() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let gamma = session_name("gamma");
    create_session(&handler, "alpha").await;
    create_grouped_session(&handler, "beta", &alpha).await;
    create_session(&handler, "gamma").await;
    create_grouped_session(&handler, "delta", &gamma).await;

    let response = handler
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
        matches!(&response, Response::LinkWindow(r) if r.target == WindowTarget::with_window(gamma.clone(), 1)),
        "expected link-window success, got {response:?}"
    );

    let linked_formats = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Window(WindowTarget::with_window(alpha.clone(), 0))),
            print: true,
            message: Some(
                "#{window_linked}:#{window_linked_sessions}:#{window_linked_sessions_list}"
                    .to_owned(),
            ),
            empty_target_context: false,
        }))
        .await
        .command_output()
        .expect("window linked format output")
        .stdout()
        .to_vec();

    assert_eq!(
        String::from_utf8_lossy(&linked_formats),
        "1:4:alpha,beta,gamma,delta\n"
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
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: beta,
            target_window_index: Some(1),
            format: Some("#{session_name}:#{window_index}:#{pane_index}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await;
    let Response::ListPanes(list) = list else {
        panic!("linked list-panes should survive owner rename, got {list:?}");
    };
    assert_eq!(String::from_utf8_lossy(list.output.stdout()), "beta:1:0\n");
}

#[tokio::test]
async fn link_window_relative_same_destination_slot_makes_room_like_tmux() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;

    let source_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .expect("alpha should exist")
            .pane_id_in_window(1, 0)
            .expect("source pane should exist")
    };

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 1),
            target: WindowTarget::with_window(alpha.clone(), 0),
            after: true,
            before: false,
            kill_destination: false,
            detached: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::LinkWindow(rmux_proto::LinkWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 1),
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(session.pane_id_in_window(1, 0), Some(source_pane_id));
    assert_eq!(session.pane_id_in_window(2, 0), Some(source_pane_id));
    assert_eq!(state.window_link_count(&alpha, 1), 2);
    assert_eq!(state.window_link_count(&alpha, 2), 2);
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
            kill_group: false,
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
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: beta,
            target_window_index: Some(1),
            format: Some("#{session_name}:#{window_index}:#{pane_index}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await;
    let Response::ListPanes(list) = list else {
        panic!("linked list-panes should survive owner removal, got {list:?}");
    };
    assert_eq!(String::from_utf8_lossy(list.output.stdout()), "beta:1:0\n");
}

#[tokio::test]
async fn unlink_window_runtime_owner_transfers_runtime_to_surviving_alias() {
    let handler = RequestHandler::new();
    let owner = session_name("unlink-runtime-owner");
    let external = session_name("unlink-runtime-external");
    create_session(&handler, owner.as_str()).await;
    insert_window(&handler, &owner, 1).await;
    create_session(&handler, external.as_str()).await;

    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(external.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");

    let unlinked = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: WindowTarget::with_window(owner.clone(), 0),
            kill_if_last: false,
        }))
        .await;
    assert!(
        matches!(unlinked, Response::UnlinkWindow(_)),
        "{unlinked:?}"
    );

    {
        let state = handler.state.lock().await;
        assert_eq!(state.window_link_count(&external, 1), 1);
        state
            .pane_profile_in_window(&external, 1, 0)
            .expect("surviving external alias adopts the detached owner's runtime");
        state
            .pane_profile_in_window(&owner, 1, 0)
            .expect("the owner's unrelated window keeps its runtime");
    }
    assert_send_keys_succeeds(&handler, PaneTarget::with_window(external, 1, 0)).await;
}

#[tokio::test]
async fn link_window_k_runtime_owner_transfers_replaced_runtime_to_surviving_alias() {
    let handler = RequestHandler::new();
    let owner = session_name("link-k-runtime-owner");
    let external = session_name("link-k-runtime-external");
    let replacement = session_name("link-k-runtime-replacement");
    create_session(&handler, owner.as_str()).await;
    insert_window(&handler, &owner, 1).await;
    create_session(&handler, external.as_str()).await;
    create_session(&handler, replacement.as_str()).await;

    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(external.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");

    let replaced = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(replacement.clone(), 0),
            target: WindowTarget::with_window(owner.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(replaced, Response::LinkWindow(_)), "{replaced:?}");

    {
        let state = handler.state.lock().await;
        assert_eq!(state.window_link_count(&external, 1), 1);
        state
            .pane_profile_in_window(&external, 1, 0)
            .expect("surviving alias adopts the replaced runtime");
        state
            .pane_profile_in_window(&owner, 0, 0)
            .expect("replacement target resolves its new linked runtime");
    }
    assert_send_keys_succeeds(&handler, PaneTarget::with_window(external, 1, 0)).await;
    assert_send_keys_succeeds(&handler, PaneTarget::with_window(owner, 0, 0)).await;
}

#[tokio::test]
async fn killing_grouped_runtime_owner_preserves_external_linked_alias() {
    let handler = RequestHandler::new();
    let owner = session_name("group-kill-runtime-owner");
    let peer = session_name("group-kill-runtime-peer");
    let external = session_name("group-kill-runtime-external");
    create_session(&handler, owner.as_str()).await;
    create_grouped_session(&handler, peer.as_str(), &owner).await;
    create_session(&handler, external.as_str()).await;

    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(external.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: owner.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");

    {
        let state = handler.state.lock().await;
        assert!(state.sessions.session(&owner).is_none());
        assert_eq!(state.window_link_count(&peer, 0), 2);
        assert_eq!(state.window_link_count(&external, 1), 2);
        assert_eq!(
            state.window_linked_sessions_list(&external, 1),
            vec![peer.clone(), external.clone()],
            "external alias metadata must be rekeyed from the removed owner to its peer"
        );
        state
            .pane_profile_in_window(&peer, 0, 0)
            .expect("group peer keeps the transferred runtime");
        state
            .pane_profile_in_window(&external, 1, 0)
            .expect("external alias follows the transferred group runtime");
    }
    assert_send_keys_succeeds(&handler, PaneTarget::with_window(peer, 0, 0)).await;
    assert_send_keys_succeeds(&handler, PaneTarget::with_window(external, 1, 0)).await;
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
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: beta.clone(),
            target_window_index: Some(1),
            format: Some("#{pane_index}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
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
async fn linked_window_id_resolution_prefers_current_session_slot() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
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
                detached: true,
            }))
            .await,
        Response::LinkWindow(_)
    ));

    let window_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .expect("linked source window exists")
            .id()
            .to_string()
    };

    let resolved = handler
        .handle(Request::ResolveTarget(ResolveTargetRequest {
            target: Some(window_id),
            target_type: ResolveTargetType::Window,
            window_index: false,
            prefer_unattached: false,
        }))
        .await;
    let Response::ResolveTarget(resolved) = resolved else {
        panic!("linked window id should resolve through preferred session, got {resolved:?}");
    };
    assert_eq!(
        resolved.target,
        Target::Window(WindowTarget::with_window(beta, 1))
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
async fn unlink_window_kill_if_last_rekeys_renumbered_silence_timers_without_delay() {
    let handler = RequestHandler::new();
    let alpha = session_name("unlink-renumber-timers");
    let unrelated = session_name("unlink-renumber-unrelated");
    create_session(&handler, alpha.as_str()).await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;
    create_session(&handler, unrelated.as_str()).await;

    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha.clone()),
            option: OptionName::RenumberWindows,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::MonitorSilence,
            value: "60".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");

    let targets = [
        WindowTarget::with_window(alpha.clone(), 0),
        WindowTarget::with_window(alpha.clone(), 1),
        WindowTarget::with_window(alpha.clone(), 2),
    ];
    let snapshots = targets.clone().map(|target| {
        handler
            .silence_timer_snapshot_for_test(&target)
            .expect("each window starts with an armed silence timer")
    });
    let unrelated_target = WindowTarget::with_window(unrelated, 0);
    let unrelated_snapshot = handler
        .silence_timer_snapshot_for_test(&unrelated_target)
        .expect("unrelated session timer starts armed");
    let surviving_window_ids = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("alpha exists");
        [
            session.window_at(1).expect("window one exists").id(),
            session.window_at(2).expect("window two exists").id(),
        ]
    };

    let response = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: targets[0].clone(),
            kill_if_last: true,
        }))
        .await;
    assert!(
        matches!(&response, Response::UnlinkWindow(result) if result.target == WindowTarget::with_window(alpha.clone(), 0)),
        "expected unlink-window -k success with renumbering, got {response:?}"
    );

    {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("alpha survives");
        assert_eq!(
            session.window_at(0).expect("old window one moved").id(),
            surviving_window_ids[0]
        );
        assert_eq!(
            session.window_at(1).expect("old window two moved").id(),
            surviving_window_ids[1]
        );
        assert!(session.window_at(2).is_none());
    }
    assert_eq!(
        handler
            .silence_timer_snapshot_for_test(&targets[0])
            .expect("old window one timer moved to zero")
            .1,
        snapshots[1].1,
        "renumbering must preserve old window one's absolute silence deadline"
    );
    assert_eq!(
        handler
            .silence_timer_snapshot_for_test(&targets[1])
            .expect("old window two timer moved to one")
            .1,
        snapshots[2].1,
        "renumbering must preserve old window two's absolute silence deadline"
    );
    assert_eq!(
        handler.silence_timer_snapshot_for_test(&targets[2]),
        None,
        "the stale pre-renumber timer key must be removed"
    );
    assert_eq!(
        handler.silence_timer_snapshot_for_test(&unrelated_target),
        Some(unrelated_snapshot),
        "unrelated session timer must remain untouched"
    );
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
