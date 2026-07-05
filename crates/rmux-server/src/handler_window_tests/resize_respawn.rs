use super::*;

#[tokio::test]
async fn resize_window_applies_explicit_dimensions() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::ResizeWindow(ResizeWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            width: Some(60),
            height: Some(20),
            adjustment: None,
        }))
        .await;

    assert!(
        matches!(&response, Response::ResizeWindow(r) if r.target == WindowTarget::with_window(alpha.clone(), 0)),
        "expected resize success, got {response:?}"
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    let window = session.window_at(0).expect("window 0 should exist");
    assert_eq!(window.size().cols, 60);
    assert_eq!(window.size().rows, 20);
}

#[tokio::test]
async fn resize_window_applies_relative_adjustment() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    // Session created with cols=120, rows=40. Shrink by 10 cols.
    let response = handler
        .handle(Request::ResizeWindow(ResizeWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            width: None,
            height: None,
            adjustment: Some(ResizeWindowAdjustment::Left(10)),
        }))
        .await;

    assert!(matches!(response, Response::ResizeWindow(_)));

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    let window = session.window_at(0).expect("window 0 should exist");
    assert_eq!(window.size().cols, 110);
    assert_eq!(window.size().rows, 40);
}

#[tokio::test]
async fn resize_window_applies_adjustment_after_explicit_dimensions() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::ResizeWindow(ResizeWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            width: Some(60),
            height: Some(20),
            adjustment: Some(ResizeWindowAdjustment::Down(5)),
        }))
        .await;

    assert!(matches!(response, Response::ResizeWindow(_)));

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    let window = session.window_at(0).expect("window 0 should exist");
    assert_eq!(window.size().cols, 60);
    assert_eq!(window.size().rows, 25);
}

#[tokio::test]
async fn resize_window_largest_smallest_without_attached_clients_use_target_session_size() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session_with_size(
        &handler,
        "alpha",
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    )
    .await;
    create_session_with_size(&handler, "beta", TerminalSize { cols: 80, rows: 24 }).await;

    let link = handler
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
        matches!(link, Response::LinkWindow(_)),
        "expected link-window success, got {link:?}"
    );

    for (target, expected) in [
        (
            WindowTarget::with_window(alpha.clone(), 0),
            TerminalSize {
                cols: 120,
                rows: 40,
            },
        ),
        (
            WindowTarget::with_window(beta.clone(), 1),
            TerminalSize { cols: 80, rows: 24 },
        ),
    ] {
        for adjustment in [
            ResizeWindowAdjustment::LargestLinkedSession,
            ResizeWindowAdjustment::SmallestLinkedSession,
        ] {
            let shrink = handler
                .handle(Request::ResizeWindow(ResizeWindowRequest {
                    target: target.clone(),
                    width: Some(70),
                    height: Some(20),
                    adjustment: None,
                }))
                .await;
            assert!(
                matches!(shrink, Response::ResizeWindow(_)),
                "expected setup resize success, got {shrink:?}"
            );

            let response = handler
                .handle(Request::ResizeWindow(ResizeWindowRequest {
                    target: target.clone(),
                    width: None,
                    height: None,
                    adjustment: Some(adjustment),
                }))
                .await;

            assert!(matches!(response, Response::ResizeWindow(_)));
            let state = handler.state.lock().await;
            let window = state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .expect("window exists");
            assert_eq!(window.size(), expected);
        }
    }
}

#[tokio::test]
async fn resize_window_updates_linked_slots_and_refreshes_linked_sessions() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session_with_size(&handler, "alpha", TerminalSize { cols: 80, rows: 24 }).await;
    create_session_with_size(
        &handler,
        "beta",
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    )
    .await;

    let link = handler
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
        matches!(link, Response::LinkWindow(_)),
        "expected link-window success, got {link:?}"
    );

    let selected = handler
        .handle(Request::SelectWindow(SelectWindowRequest {
            target: WindowTarget::with_window(beta.clone(), 1),
        }))
        .await;
    assert!(
        matches!(selected, Response::SelectWindow(_)),
        "expected select-window success, got {selected:?}"
    );

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler.register_attach(42, beta.clone(), control_tx).await;
    drain_attach_controls(&mut control_rx).await;

    let response = handler
        .handle(Request::ResizeWindow(ResizeWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            width: Some(70),
            height: Some(20),
            adjustment: None,
        }))
        .await;
    assert!(
        matches!(response, Response::ResizeWindow(_)),
        "expected resize-window success, got {response:?}"
    );

    {
        let state = handler.state.lock().await;
        for (session_name, window_index, expected) in [
            (&alpha, 0, TerminalSize { cols: 70, rows: 20 }),
            (
                &beta,
                0,
                TerminalSize {
                    cols: 120,
                    rows: 40,
                },
            ),
            (&beta, 1, TerminalSize { cols: 70, rows: 20 }),
        ] {
            let window = state
                .sessions
                .session(session_name)
                .and_then(|session| session.window_at(window_index))
                .expect("window exists");
            assert_eq!(window.size(), expected);
        }
    }

    let refresh = timeout(Duration::from_secs(2), control_rx.recv())
        .await
        .expect("linked session should receive a refresh")
        .expect("refresh channel should remain open");
    assert_refresh(refresh);
}

#[tokio::test]
async fn resize_window_propagates_linked_slots_to_their_session_group_peers() {
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
        .handle(Request::ResizeWindow(ResizeWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            width: Some(111),
            height: Some(33),
            adjustment: None,
        }))
        .await;
    assert!(
        matches!(response, Response::ResizeWindow(_)),
        "expected resize-window success, got {response:?}"
    );

    let state = handler.state.lock().await;
    for (session_name, window_index) in [(&alpha, 0), (&beta, 0), (&gamma, 1), (&delta, 1)] {
        let window = state
            .sessions
            .session(session_name)
            .and_then(|session| session.window_at(window_index))
            .expect("linked window should exist");
        assert_eq!(
            window.size(),
            TerminalSize {
                cols: 111,
                rows: 33
            },
            "{session_name}:{window_index} should reflect linked resize"
        );
    }
}

async fn create_session_with_size(handler: &RequestHandler, name: &str, size: TerminalSize) {
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name(name),
            detached: true,
            size: Some(size),
            environment: None,
        }))
        .await;
    assert!(
        matches!(created, Response::NewSession(_)),
        "expected new-session success, got {created:?}"
    );
}

#[tokio::test]
async fn resize_window_largest_smallest_with_attached_clients_still_use_client_sizes() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session_with_size(
        &handler,
        "alpha",
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    )
    .await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler.register_attach(42, alpha.clone(), control_tx).await;
    {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&42)
            .expect("registered attach must exist");
        active.client_size = TerminalSize {
            cols: 100,
            rows: 30,
        };
    }

    for adjustment in [
        ResizeWindowAdjustment::LargestLinkedSession,
        ResizeWindowAdjustment::SmallestLinkedSession,
    ] {
        let shrink = handler
            .handle(Request::ResizeWindow(ResizeWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 0),
                width: Some(70),
                height: Some(20),
                adjustment: None,
            }))
            .await;
        assert!(
            matches!(shrink, Response::ResizeWindow(_)),
            "expected setup resize success, got {shrink:?}"
        );

        let response = handler
            .handle(Request::ResizeWindow(ResizeWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 0),
                width: None,
                height: None,
                adjustment: Some(adjustment),
            }))
            .await;

        assert!(matches!(response, Response::ResizeWindow(_)));
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .expect("window exists");
        assert_eq!(
            window.size(),
            TerminalSize {
                cols: 100,
                rows: 30
            }
        );
    }
}

#[tokio::test]
async fn resize_window_clamps_relative_adjustments_to_a_minimum_size_of_one() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::ResizeWindow(ResizeWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            width: Some(2),
            height: Some(3),
            adjustment: Some(ResizeWindowAdjustment::Left(10)),
        }))
        .await;

    assert!(matches!(response, Response::ResizeWindow(_)));

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    let window = session.window_at(0).expect("window 0 should exist");
    assert_eq!(window.size().cols, 1);
    assert_eq!(window.size().rows, 3);
}

#[tokio::test]
async fn resize_window_rejects_nonexistent_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::ResizeWindow(ResizeWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 99),
            width: Some(40),
            height: Some(20),
            adjustment: None,
        }))
        .await;

    assert!(
        matches!(response, Response::Error(_)),
        "expected error for nonexistent window, got {response:?}"
    );
}

#[tokio::test]
async fn respawn_window_rejects_active_window_without_kill_flag() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    // Window 0 has a running pane — respawn without -k should fail.
    let response = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            kill: false,
            start_directory: None,
            environment: None,
            command: None,
        })))
        .await;

    assert!(
        matches!(&response, Response::Error(e) if e.error.to_string().contains("still active")),
        "expected still-active error, got {response:?}"
    );
}

#[tokio::test]
async fn respawn_window_succeeds_with_kill_flag() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
        })))
        .await;

    assert!(
        matches!(&response, Response::RespawnWindow(r) if r.target == WindowTarget::with_window(alpha.clone(), 0)),
        "expected respawn success with -k, got {response:?}"
    );

    // After respawn, window should still exist with exactly one pane.
    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    let window = session
        .window_at(0)
        .expect("window 0 should exist after respawn");
    assert_eq!(window.panes().len(), 1);
}

#[tokio::test]
async fn respawn_window_retains_surviving_pane_lifecycle_counters_and_redacts_env() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let initial_secret = "RMUX_WINDOW_INITIAL=alpha-secret".to_owned();
    let split_secret = "RMUX_WINDOW_SPLIT=beta-secret".to_owned();
    let respawn_secret = "RMUX_WINDOW_RESPAWN=gamma-secret".to_owned();
    let respawn_command = crate::test_shell::stdin_discard_command();

    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(alpha.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),
            environment: Some(vec![initial_secret.clone()]),
            group_target: None,
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
    assert!(matches!(created, Response::NewSession(_)));

    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: Some(vec![split_secret.clone()]),
        }))
        .await;
    let split_target = match split {
        Response::SplitWindow(response) => response.pane,
        response => panic!("expected split-window success, got {response:?}"),
    };

    let (surviving_pane_id, split_pane_id, previous_generation, previous_revision, previous_output) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window_at(0).expect("window exists");
        let surviving_pane = window.pane(0).expect("surviving pane exists");
        let split_pane = window
            .pane(split_target.pane_index())
            .expect("split pane exists");
        let lifecycle = state
            .pane_lifecycle(surviving_pane.id())
            .expect("surviving lifecycle exists");
        assert_eq!(
            lifecycle.private_environment(),
            std::slice::from_ref(&initial_secret)
        );
        assert_eq!(
            state
                .pane_lifecycle(split_pane.id())
                .expect("split lifecycle exists")
                .private_environment(),
            std::slice::from_ref(&split_secret)
        );
        (
            surviving_pane.id(),
            split_pane.id(),
            lifecycle.generation,
            lifecycle.revision,
            lifecycle.output_sequence,
        )
    };

    let response = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            kill: true,
            start_directory: None,
            environment: Some(vec![respawn_secret.clone()]),
            command: Some(vec![respawn_command.clone()]),
        })))
        .await;

    assert!(
        matches!(&response, Response::RespawnWindow(r) if r.target == WindowTarget::with_window(alpha.clone(), 0)),
        "expected respawn-window success, got {response:?}"
    );

    let (generation, revision, output_sequence) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window_at(0).expect("window exists");
        let pane = window.pane(0).expect("respawned pane exists");
        assert_eq!(window.panes().len(), 1);
        assert_eq!(pane.id(), surviving_pane_id);
        assert!(
            state.pane_lifecycle(split_pane_id).is_none(),
            "respawn-window must remove lifecycle state for panes it destroys"
        );

        let lifecycle = state
            .pane_lifecycle(surviving_pane_id)
            .expect("respawned lifecycle exists");
        assert_eq!(
            lifecycle.command(),
            Some(std::slice::from_ref(&respawn_command))
        );
        assert_eq!(
            lifecycle.private_environment(),
            std::slice::from_ref(&respawn_secret)
        );
        assert!(!lifecycle.private_environment().contains(&initial_secret));
        assert!(!lifecycle.private_environment().contains(&split_secret));
        assert!(lifecycle.generation > previous_generation);
        assert!(lifecycle.revision > previous_revision);
        assert!(lifecycle.output_sequence > previous_output);
        (
            lifecycle.generation,
            lifecycle.revision,
            lifecycle.output_sequence,
        )
    };

    let listed = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: alpha.clone(),
            target_window_index: Some(0),
            format: Some(
                "#{pane_id}\t#{pane_lifecycle_generation}\t#{pane_lifecycle_revision}\t#{pane_output_sequence}\t#{pane_start_command}".to_owned(),
            ),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await;
    let list_stdout = match listed {
        Response::ListPanes(response) => {
            String::from_utf8(response.output.stdout).expect("list-panes utf8")
        }
        response => panic!("expected list-panes response, got {response:?}"),
    };
    assert!(list_stdout.contains(&surviving_pane_id.to_string()));
    assert!(list_stdout.contains(&generation.to_string()));
    assert!(list_stdout.contains(&revision.to_string()));
    assert!(list_stdout.contains(&output_sequence.to_string()));
    assert!(!list_stdout.contains(&initial_secret));
    assert!(!list_stdout.contains(&split_secret));
    assert!(!list_stdout.contains(&respawn_secret));

    let windows = handler
        .handle(Request::ListWindows(Box::new(ListWindowsRequest {
            target: alpha,
            format: Some(
                "#{window_id}\t#{pane_id}\t#{pane_lifecycle_generation}\t#{pane_output_sequence}"
                    .to_owned(),
            ),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await;
    let windows_stdout = match windows {
        Response::ListWindows(response) => {
            assert_eq!(response.windows.len(), 1);
            String::from_utf8(response.output.stdout).expect("list-windows utf8")
        }
        response => panic!("expected list-windows response, got {response:?}"),
    };
    assert!(!windows_stdout.contains(&initial_secret));
    assert!(!windows_stdout.contains(&split_secret));
    assert!(!windows_stdout.contains(&respawn_secret));
}

#[tokio::test]
async fn respawn_window_selects_target_window_like_tmux() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;

    assert!(matches!(
        handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 1),
            }))
            .await,
        Response::SelectWindow(_)
    ));

    let response = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
        })))
        .await;

    assert!(
        matches!(response, Response::RespawnWindow(_)),
        "respawn-window should succeed, got {response:?}"
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("alpha should exist");
    assert_eq!(session.active_window_index(), 0);
}
