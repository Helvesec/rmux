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
        assert_eq!(
            state
                .options
                .resolve_for_window(&alpha, 0, OptionName::WindowSize),
            Some("manual")
        );
        assert_eq!(
            state
                .options
                .resolve_for_window(&beta, 1, OptionName::WindowSize),
            Some("manual")
        );
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
        active.set_declared_client_size(TerminalSize {
            cols: 100,
            rows: 30,
        });
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
        // The client owns an outer 100x30 terminal and `status` defaults to
        // `on`, so the window content is 100x29. tmux 3.7b measured with a real
        // 100x30 PTY client: `resize-window -A` and `-a` both land on 100x29.
        assert_eq!(
            window.size(),
            TerminalSize {
                cols: 100,
                rows: 29
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
async fn resize_window_keeps_multi_pane_geometry_at_the_tmux_viable_minimum() {
    let handler = RequestHandler::new();

    for (name, initial, direction, expected) in [
        (
            "vertical-minimum",
            TerminalSize { cols: 10, rows: 3 },
            SplitDirection::Vertical,
            TerminalSize { cols: 1, rows: 3 },
        ),
        (
            "horizontal-minimum",
            TerminalSize { cols: 3, rows: 10 },
            SplitDirection::Horizontal,
            TerminalSize { cols: 3, rows: 1 },
        ),
    ] {
        let session_name = session_name(name);
        create_session_with_size(&handler, name, initial).await;

        let split = handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session_name.clone()),
                direction,
                before: false,
                environment: None,
            }))
            .await;
        assert!(
            matches!(split, Response::SplitWindow(_)),
            "split must succeed for {name}: {split:?}"
        );

        for _ in 0..2 {
            let response = handler
                .handle(Request::ResizeWindow(ResizeWindowRequest {
                    target: WindowTarget::with_window(session_name.clone(), 0),
                    width: Some(1),
                    height: Some(1),
                    adjustment: None,
                }))
                .await;
            assert!(
                matches!(response, Response::ResizeWindow(_)),
                "resize must succeed for {name}: {response:?}"
            );

            let state = handler.state.lock().await;
            let window = state
                .sessions
                .session(&session_name)
                .and_then(|session| session.window_at(0))
                .expect("window exists");
            assert_eq!(window.size(), expected, "session={name}");
            assert!(
                window.panes().iter().all(|pane| {
                    let geometry = pane.geometry();
                    geometry.cols() >= 1 && geometry.rows() >= 1
                }),
                "session={name} panes={:?}",
                window.panes()
            );
        }
    }
}

#[tokio::test]
async fn select_layout_expands_a_minimum_window_to_the_named_tree_minimum() {
    let handler = RequestHandler::new();
    let name = "select-layout-minimum";
    let session_name = session_name(name);
    create_session_with_size(&handler, name, TerminalSize { cols: 3, rows: 10 }).await;

    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session_name.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(
        matches!(split, Response::SplitWindow(_)),
        "split must succeed: {split:?}"
    );

    let resized = handler
        .handle(Request::ResizeWindow(ResizeWindowRequest {
            target: WindowTarget::with_window(session_name.clone(), 0),
            width: Some(3),
            height: Some(1),
            adjustment: None,
        }))
        .await;
    assert!(
        matches!(resized, Response::ResizeWindow(_)),
        "resize must succeed: {resized:?}"
    );

    for _ in 0..2 {
        let selected = handler
            .handle(Request::SelectLayout(SelectLayoutRequest {
                target: SelectLayoutTarget::Window(WindowTarget::with_window(
                    session_name.clone(),
                    0,
                )),
                layout: LayoutName::EvenVertical,
            }))
            .await;
        assert!(
            matches!(
                selected,
                Response::SelectLayout(response) if response.layout == LayoutName::EvenVertical
            ),
            "select-layout must succeed: {selected:?}"
        );

        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&session_name)
            .and_then(|session| session.window_at(0))
            .expect("window exists");
        assert_eq!(window.size(), TerminalSize { cols: 3, rows: 3 });
        assert_eq!(
            window
                .panes()
                .iter()
                .map(|pane| pane.geometry())
                .collect::<Vec<_>>(),
            vec![
                rmux_core::PaneGeometry::new(0, 0, 3, 1),
                rmux_core::PaneGeometry::new(0, 2, 3, 1),
            ]
        );
    }
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
async fn respawn_window_reuses_shell_command_cwd_and_private_environment() {
    let handler = RequestHandler::new();
    let alpha = session_name("respawn-window-provenance");
    let initial_cwd = unique_window_temp_path("respawn-provenance-initial");
    let override_cwd = unique_window_temp_path("respawn-provenance-override");
    let output = unique_window_temp_path("respawn-provenance-output");
    fs::create_dir_all(&initial_cwd).expect("initial respawn-window cwd");
    fs::create_dir_all(&override_cwd).expect("override respawn-window cwd");
    let initial_command = window_respawn_replay_command(&output, "initial-command");
    let initial_process_command = ProcessCommand::Shell(initial_command.clone());
    let initial_environment = "RMUX_RESPAWN=initial".to_owned();

    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(alpha.clone()),
            working_directory: Some(initial_cwd.to_string_lossy().into_owned()),
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),
            environment: Some(vec![initial_environment.clone()]),
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: Some(initial_process_command.clone()),
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&created);

    let initial_cwd_text = expected_window_spawn_cwd(&initial_cwd);
    let initial_line = window_respawn_probe_line(&initial_cwd_text, "initial", "initial-command");
    wait_for_window_file_contents(&output, &initial_line).await;
    let pane_id = {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.pane_id_in_window(0, 0))
            .expect("initial pane exists");
        let lifecycle = state.pane_lifecycle(pane_id).expect("initial lifecycle");
        assert_eq!(lifecycle.process_command(), Some(&initial_process_command));
        assert_eq!(
            lifecycle.respawn_environment(),
            std::slice::from_ref(&initial_environment)
        );
        pane_id
    };

    let target = WindowTarget::with_window(alpha.clone(), 0);
    let inherited = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
        })))
        .await;
    assert!(
        matches!(inherited, Response::RespawnWindow(_)),
        "{inherited:?}"
    );
    wait_for_window_file_contents(&output, &format!("{initial_line}{initial_line}")).await;

    let override_environment = "RMUX_RESPAWN=override".to_owned();
    let override_command = window_respawn_replay_command(&output, "override-command");
    let override_process_command = ProcessCommand::Shell(override_command.clone());
    let explicit = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: target.clone(),
            kill: true,
            start_directory: Some(override_cwd.clone()),
            environment: Some(vec![override_environment.clone()]),
            command: Some(vec![override_command]),
        })))
        .await;
    assert!(
        matches!(explicit, Response::RespawnWindow(_)),
        "{explicit:?}"
    );
    let override_cwd_text = expected_window_spawn_cwd(&override_cwd);
    let override_line =
        window_respawn_probe_line(&override_cwd_text, "override", "override-command");
    wait_for_window_file_contents(
        &output,
        &format!("{initial_line}{initial_line}{override_line}"),
    )
    .await;
    {
        let state = handler.state.lock().await;
        let lifecycle = state.pane_lifecycle(pane_id).expect("override lifecycle");
        assert_eq!(lifecycle.process_command(), Some(&override_process_command));
        assert_eq!(
            lifecycle.private_environment(),
            std::slice::from_ref(&override_environment)
        );
        assert_eq!(
            lifecycle.respawn_environment(),
            std::slice::from_ref(&initial_environment)
        );
    }

    let inherited_after_override = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target,
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
        })))
        .await;
    assert!(
        matches!(inherited_after_override, Response::RespawnWindow(_)),
        "{inherited_after_override:?}"
    );
    let inherited_override_line =
        window_respawn_probe_line(&override_cwd_text, "initial", "override-command");
    wait_for_window_file_contents(
        &output,
        &format!("{initial_line}{initial_line}{override_line}{inherited_override_line}"),
    )
    .await;

    drop(handler);
    let _ = fs::remove_file(output);
    let _ = fs::remove_dir_all(initial_cwd);
    let _ = fs::remove_dir_all(override_cwd);
}

#[cfg(unix)]
#[tokio::test]
async fn respawn_window_keeps_the_original_resolved_shell_after_option_changes() {
    let handler = RequestHandler::new();
    let alpha = session_name("respawn-window-shell-provenance");
    let output = unique_window_temp_path("respawn-window-shell-provenance-output");
    set_window_test_default_shell(&handler, "/bin/sh").await;
    create_session(&handler, alpha.as_str()).await;
    handler.wait_for_initial_panes_for_test().await;
    set_window_test_default_shell(&handler, "/bin/bash").await;

    let target = WindowTarget::with_window(alpha.clone(), 0);
    let shell_command = window_respawn_shell_identity_command(&output, "shell");
    let explicit = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec![shell_command]),
        })))
        .await;
    assert!(
        matches!(explicit, Response::RespawnWindow(_)),
        "{explicit:?}"
    );
    let expected_line = "sh:/bin/sh:shell\n";
    wait_for_window_file_contents(&output, expected_line).await;

    let inherited = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target,
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
        })))
        .await;
    assert!(
        matches!(inherited, Response::RespawnWindow(_)),
        "{inherited:?}"
    );
    wait_for_window_file_contents(&output, &format!("{expected_line}{expected_line}")).await;

    let state = handler.state.lock().await;
    let pane_id = state
        .sessions
        .session(&alpha)
        .and_then(|session| session.pane_id_in_window(0, 0))
        .expect("respawned window pane exists");
    assert_eq!(
        state
            .pane_lifecycle(pane_id)
            .expect("respawned window lifecycle")
            .respawn_shell(),
        Path::new("/bin/sh")
    );
    drop(state);
    drop(handler);
    let _ = fs::remove_file(output);
}

#[tokio::test]
async fn failed_respawn_window_preserves_the_old_layout_terminal_and_lifecycle() {
    let handler = RequestHandler::new();
    let alpha = session_name("respawn-window-rollback");
    create_session(&handler, alpha.as_str()).await;
    handler.wait_for_initial_panes_for_test().await;
    let (session_before, pane_id, lifecycle_before) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let pane_id = session
            .pane_id_in_window(0, 0)
            .expect("initial pane exists");
        (
            session.test_snapshot(),
            pane_id,
            state
                .pane_lifecycle(pane_id)
                .expect("pane lifecycle exists")
                .clone(),
        )
    };

    let response = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 0),
            kill: true,
            start_directory: None,
            environment: Some(vec!["INVALID".to_owned()]),
            command: None,
        })))
        .await;
    assert!(matches!(response, Response::Error(_)), "{response:?}");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .map(|session| session.test_snapshot()),
        Some(session_before)
    );
    state
        .ensure_panes_exist(&alpha, &[pane_id])
        .expect("failed respawn must retain the old terminal");
    assert_eq!(state.pane_lifecycle(pane_id), Some(&lifecycle_before));
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
    crate::session_fixture::assert_new_session(&created);

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

/// A detached 80x40 window split four times, then `resize-window -x 6 -y 4`:
/// five stacked one-row panes hold it at 6x9 (tmux 3.7b, R3510 M9).
async fn manual_window_held_above_its_size(handler: &RequestHandler, name: &str) -> SessionName {
    let session = session_name(name);
    create_session_with_size(handler, name, TerminalSize { cols: 80, rows: 40 }).await;
    for _ in 0..4 {
        let split = handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await;
        assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    }
    let resized = handler
        .handle(Request::ResizeWindow(ResizeWindowRequest {
            target: WindowTarget::with_window(session.clone(), 0),
            width: Some(6),
            height: Some(4),
            adjustment: None,
        }))
        .await;
    assert!(matches!(resized, Response::ResizeWindow(_)), "{resized:?}");
    assert_eq!(window_geometry(handler, &session).await.0, (6, 9));
    session
}

/// The window size and every pane's `(x, y, cols, rows)` in pane order.
async fn window_geometry(
    handler: &RequestHandler,
    session: &SessionName,
) -> ((u16, u16), Vec<(u16, u16, u16, u16)>) {
    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(session)
        .and_then(|session| session.window_at(0))
        .expect("window 0 should exist");
    let panes = window
        .panes()
        .iter()
        .map(|pane| {
            let geometry = pane.geometry();
            (geometry.x(), geometry.y(), geometry.cols(), geometry.rows())
        })
        .collect();
    ((window.size().cols, window.size().rows), panes)
}

async fn kill_pane(handler: &RequestHandler, session: &SessionName, pane: u32, all_except: bool) {
    let killed = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(session.clone(), 0, pane),
            kill_all_except: all_except,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)), "{killed:?}");
}

#[tokio::test]
async fn kill_pane_returns_manual_window_to_its_layout_minimum() {
    let handler = RequestHandler::new();
    let session = manual_window_held_above_its_size(&handler, "m9-kill").await;

    kill_pane(&handler, &session, 4, false).await;

    // tmux 3.7b: 6x7, four one-row panes; H58 kept 6x9 with a 6x3 last pane.
    assert_eq!(
        window_geometry(&handler, &session).await,
        (
            (6, 7),
            vec![(0, 0, 6, 1), (0, 2, 6, 1), (0, 4, 6, 1), (0, 6, 6, 1)]
        )
    );
}

#[tokio::test]
async fn kill_other_panes_returns_manual_window_to_its_explicit_size() {
    let handler = RequestHandler::new();
    let session = manual_window_held_above_its_size(&handler, "m9-kill-all").await;

    kill_pane(&handler, &session, 2, true).await;

    // tmux 3.7b `kill-pane -a`: the lone pane fits the explicit 6x4.
    assert_eq!(
        window_geometry(&handler, &session).await,
        ((6, 4), vec![(0, 0, 6, 4)])
    );
}

#[tokio::test]
async fn kill_pane_keeps_a_non_manual_window_at_its_size() {
    let handler = RequestHandler::new();
    let session = manual_window_held_above_its_size(&handler, "m9-latest").await;
    let latest = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
            option: OptionName::WindowSize,
            value: "latest".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(latest, Response::SetOption(_)), "{latest:?}");

    kill_pane(&handler, &session, 4, false).await;

    // tmux 3.7b with no client under `window-size latest`: no new size, 6x9 stays.
    assert_eq!(
        window_geometry(&handler, &session).await,
        (
            (6, 9),
            vec![(0, 0, 6, 1), (0, 2, 6, 1), (0, 4, 6, 1), (0, 6, 6, 3)]
        )
    );
}

#[tokio::test]
async fn select_layout_returns_manual_window_to_its_layout_minimum() {
    let handler = RequestHandler::new();
    let session = manual_window_held_above_its_size(&handler, "m9-layout").await;
    kill_pane(&handler, &session, 4, false).await;

    let selected = handler
        .handle(Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Window(WindowTarget::with_window(session.clone(), 0)),
            layout: LayoutName::MainVertical,
        }))
        .await;
    assert!(
        matches!(selected, Response::SelectLayout(_)),
        "{selected:?}"
    );

    // tmux 3.7b: `select-layout main-vertical` built at 6x7, then
    // `recalculate_sizes()` takes it back towards 6x4: three stacked
    // one-row panes beside the main pane need five rows.
    assert_eq!(
        window_geometry(&handler, &session).await,
        (
            (6, 5),
            vec![(0, 0, 4, 5), (5, 0, 1, 1), (5, 2, 1, 1), (5, 4, 1, 1)]
        )
    );
}
