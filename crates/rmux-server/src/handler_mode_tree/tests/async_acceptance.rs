use super::*;

use super::super::mode_tree_model::ChooseTreeTarget;

#[tokio::test]
async fn choose_tree_kill_pane_drains_after_kill_pane_inline_hook() {
    let handler = RequestHandler::new();
    let alpha = SessionName::new("choose-tree-after-kill").expect("valid session");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(rmux_proto::SplitWindowRequest {
                target: rmux_proto::SplitWindowTarget::Session(alpha.clone()),
                direction: rmux_proto::SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetHook(rmux_proto::SetHookRequest {
                scope: rmux_proto::ScopeSelector::Global,
                hook: rmux_proto::HookName::AfterKillPane,
                command: "set-buffer -b tree-after-kill fired".to_owned(),
                lifecycle: rmux_proto::HookLifecycle::Persistent,
            }))
            .await,
        Response::SetHook(_)
    ));
    let (session_id, window_id, window_occurrence_id, pane_id) = {
        let mut state = handler.state.lock().await;
        state.ensure_live_window_link_occurrences();
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window_at(0).expect("window exists");
        let pane = window.pane(1).expect("second pane exists");
        (
            session.id(),
            window.id(),
            state
                .window_link_occurrence_id(&alpha, 0)
                .expect("window occurrence exists"),
            pane.id(),
        )
    };

    handler
        .perform_tree_kill_actions(
            std::process::id(),
            vec![ModeTreeAction::pane_tree_target(
                alpha,
                session_id,
                0,
                window_id,
                window_occurrence_id,
                1,
                pane_id,
            )],
        )
        .await
        .expect("choose-tree pane kill succeeds");

    let state = handler.state.lock().await;
    assert_eq!(state.buffers.get("tree-after-kill"), Some(&b"fired"[..]));
}

#[tokio::test]
async fn choose_tree_tagged_pane_kills_follow_stable_ids_after_renumbering() {
    let handler = RequestHandler::new();
    let alpha = SessionName::new("choose-tree-tagged-stable-panes").expect("valid session");
    create_mode_tree_test_session(&handler, &alpha).await;
    for _ in 0..2 {
        assert!(matches!(
            handler
                .handle(Request::SplitWindow(rmux_proto::SplitWindowRequest {
                    target: rmux_proto::SplitWindowTarget::Session(alpha.clone()),
                    direction: rmux_proto::SplitDirection::Horizontal,
                    before: false,
                    environment: None,
                }))
                .await,
            Response::SplitWindow(_)
        ));
    }

    let (session_id, window_id, window_occurrence_id, pane_targets, surviving_pane_id) = {
        let mut state = handler.state.lock().await;
        state.ensure_live_window_link_occurrences();
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window_at(0).expect("window exists");
        let mut panes = window
            .panes()
            .iter()
            .map(|pane| (pane.index(), pane.id()))
            .collect::<Vec<_>>();
        panes.sort_by_key(|(pane_index, _)| *pane_index);
        assert_eq!(panes.len(), 3);
        (
            session.id(),
            window.id(),
            state
                .window_link_occurrence_id(&alpha, 0)
                .expect("window occurrence exists"),
            panes[..2].to_vec(),
            panes[2].1,
        )
    };
    let actions = pane_targets
        .into_iter()
        .map(|(pane_index, pane_id)| {
            ModeTreeAction::pane_tree_target(
                alpha.clone(),
                session_id,
                0,
                window_id,
                window_occurrence_id,
                pane_index,
                pane_id,
            )
        })
        .collect();

    handler
        .perform_tree_kill_actions(std::process::id(), actions)
        .await
        .expect("tagged pane kills succeed after the first pane renumbers the second");

    let state = handler.state.lock().await;
    let panes = state
        .sessions
        .session(&alpha)
        .expect("session survives")
        .window_at(0)
        .expect("window survives")
        .panes();
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0].id(), surviving_pane_id);
}

#[tokio::test]
async fn choose_tree_tagged_link_aliases_do_not_abort_later_distinct_kills() {
    let handler = RequestHandler::new();
    let alpha = SessionName::new("choose-tree-link-alpha").expect("valid session");
    let beta = SessionName::new("choose-tree-link-beta").expect("valid session");
    let gamma = SessionName::new("choose-tree-link-gamma").expect("valid session");
    for session in [&alpha, &beta, &gamma] {
        create_mode_tree_test_session(&handler, session).await;
    }
    for session in [&alpha, &gamma] {
        assert!(matches!(
            handler
                .handle(Request::NewWindow(Box::new(rmux_proto::NewWindowRequest {
                    target: session.clone(),
                    name: None,
                    detached: true,
                    environment: None,
                    command: None,
                    process_command: None,
                    start_directory: None,
                    target_window_index: Some(1),
                    insert_at_target: false,
                })))
                .await,
            Response::NewWindow(_)
        ));
    }
    assert!(matches!(
        handler
            .handle(Request::LinkWindow(rmux_proto::LinkWindowRequest {
                source: rmux_proto::WindowTarget::with_window(alpha.clone(), 0),
                target: rmux_proto::WindowTarget::with_window(beta.clone(), 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await,
        Response::LinkWindow(_)
    ));

    let actions = {
        let mut state = handler.state.lock().await;
        state.ensure_live_window_link_occurrences();
        [&alpha, &beta, &gamma]
            .into_iter()
            .zip([0_u32, 1, 0])
            .map(|(session_name, window_index)| {
                let session = state
                    .sessions
                    .session(session_name)
                    .expect("tagged session exists");
                let window = session
                    .window_at(window_index)
                    .expect("tagged window exists");
                ModeTreeAction::window_tree_target(
                    session_name.clone(),
                    session.id(),
                    window_index,
                    window.id(),
                    state
                        .window_link_occurrence_id(session_name, window_index)
                        .expect("tagged window occurrence exists"),
                )
            })
            .collect::<Vec<_>>()
    };

    handler
        .perform_tree_kill_actions(std::process::id(), actions)
        .await
        .expect("duplicate linked aliases are one stable kill target");

    let state = handler.state.lock().await;
    let alpha_session = state
        .sessions
        .session(&alpha)
        .expect("alpha keeps window one");
    assert!(alpha_session.window_at(0).is_none());
    assert!(alpha_session.window_at(1).is_some());
    let gamma_session = state
        .sessions
        .session(&gamma)
        .expect("gamma keeps window one");
    assert!(gamma_session.window_at(0).is_none());
    assert!(gamma_session.window_at(1).is_some());
    let beta_session = state
        .sessions
        .session(&beta)
        .expect("beta keeps window zero");
    assert!(beta_session.window_at(0).is_some());
    assert!(beta_session.window_at(1).is_none());
}

#[tokio::test]
async fn choose_tree_stale_session_action_fails_closed_after_name_reuse() {
    let handler = RequestHandler::new();
    let alpha = SessionName::new("choose-tree-session-aba").expect("valid session");
    create_mode_tree_test_session(&handler, &alpha).await;
    let old_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("old session exists")
        .id();
    let stale_action = ModeTreeAction::session_tree_target(alpha.clone(), old_session_id);

    assert!(matches!(
        handler
            .handle(Request::KillSession(rmux_proto::KillSessionRequest {
                target: alpha.clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await,
        Response::KillSession(_)
    ));
    create_mode_tree_test_session(&handler, &alpha).await;
    let replacement_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("replacement session exists")
        .id();
    assert_ne!(replacement_session_id, old_session_id);

    let error = handler
        .perform_tree_kill_actions(std::process::id(), vec![stale_action])
        .await
        .expect_err("stale session tree action must fail closed");
    assert!(matches!(error, RmuxError::SessionNotFound(_)), "{error:?}");
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&alpha)
            .map(rmux_core::Session::id),
        Some(replacement_session_id)
    );
}

#[tokio::test]
async fn choose_tree_kill_current_does_not_fall_back_after_session_aba() {
    use super::super::mode_tree_order::session_item_id;

    let handler = RequestHandler::new();
    let host = SessionName::new("choose-tree-aba-host").expect("valid session");
    let alpha = SessionName::new("choose-tree-aba-target").expect("valid session");
    create_mode_tree_test_session(&handler, &host).await;
    create_mode_tree_test_session(&handler, &alpha).await;
    let old_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("old target exists")
        .id();

    let attach_pid = std::process::id().saturating_add(41);
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler.register_attach(attach_pid, host, control_tx).await;
    let parsed = CommandParser::new()
        .parse_arguments(["choose-tree", "-s"])
        .expect("choose-tree parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("mode-tree command parses")
        .expect("mode-tree command recognized");
    handler
        .execute_queued_mode_tree(
            attach_pid,
            command,
            &QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("mode-tree opens");
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get_mut(&attach_pid)
        .and_then(|active| active.mode_tree.as_mut())
        .expect("mode-tree remains active")
        .selected_id = Some(session_item_id(old_session_id));

    assert!(matches!(
        handler
            .handle(Request::KillSession(rmux_proto::KillSessionRequest {
                target: alpha.clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await,
        Response::KillSession(_)
    ));
    create_mode_tree_test_session(&handler, &alpha).await;
    let replacement_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("replacement target exists")
        .id();
    assert_ne!(replacement_session_id, old_session_id);

    handler
        .perform_tree_kill_current(attach_pid)
        .await
        .expect("stale current selection is a no-op");
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&alpha)
            .map(rmux_core::Session::id),
        Some(replacement_session_id)
    );
}

#[tokio::test]
async fn choose_tree_stale_window_action_fails_closed_after_index_reuse() {
    let handler = RequestHandler::new();
    let alpha = SessionName::new("choose-tree-window-aba").expect("valid session");
    create_mode_tree_test_session(&handler, &alpha).await;
    create_mode_tree_test_window(&handler, &alpha, 1).await;
    let (session_id, old_window_id, old_window_occurrence_id) = {
        let mut state = handler.state.lock().await;
        state.ensure_live_window_link_occurrences();
        let session = state.sessions.session(&alpha).expect("session exists");
        (
            session.id(),
            session.window_at(1).expect("old window exists").id(),
            state
                .window_link_occurrence_id(&alpha, 1)
                .expect("old window occurrence exists"),
        )
    };
    let stale_action = ModeTreeAction::window_tree_target(
        alpha.clone(),
        session_id,
        1,
        old_window_id,
        old_window_occurrence_id,
    );

    assert!(matches!(
        handler
            .handle(Request::KillWindow(rmux_proto::KillWindowRequest {
                target: rmux_proto::WindowTarget::with_window(alpha.clone(), 1),
                kill_all_others: false,
            }))
            .await,
        Response::KillWindow(_)
    ));
    create_mode_tree_test_window(&handler, &alpha, 1).await;
    let replacement_window_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .and_then(|session| session.window_at(1))
        .expect("replacement window exists")
        .id();
    assert_ne!(replacement_window_id, old_window_id);

    let error = handler
        .perform_tree_kill_actions(std::process::id(), vec![stale_action])
        .await
        .expect_err("stale window tree action must fail closed");
    assert!(
        matches!(error, RmuxError::InvalidTarget { .. }),
        "{error:?}"
    );
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(1))
            .map(rmux_core::Window::id),
        Some(replacement_window_id)
    );
}

async fn create_mode_tree_test_session(handler: &RequestHandler, session_name: &SessionName) {
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
}

async fn create_mode_tree_test_window(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
) {
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(rmux_proto::NewWindowRequest {
                target: session_name.clone(),
                name: None,
                detached: true,
                environment: None,
                command: None,
                process_command: None,
                start_directory: None,
                target_window_index: Some(window_index),
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));
}

#[tokio::test]
async fn choose_tree_default_switch_rejects_a_reconnected_host_at_the_send_lock() {
    let handler = RequestHandler::new();
    let host = SessionName::new("choose-tree-host-generation-alpha").expect("valid session");
    let target = SessionName::new("choose-tree-host-generation-beta").expect("valid session");
    create_mode_tree_test_session(&handler, &host).await;
    create_mode_tree_test_session(&handler, &target).await;
    let target_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&target)
        .expect("target session exists")
        .id();

    let attach_pid = std::process::id().saturating_add(651);
    let (old_tx, _old_rx) = mpsc::unbounded_channel();
    let old_attach_id = handler
        .register_attach(attach_pid, host.clone(), old_tx)
        .await;
    let parsed = CommandParser::new()
        .parse_arguments(["choose-tree", "-s"])
        .expect("choose-tree parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("mode-tree command parses")
        .expect("mode-tree command recognized");
    handler
        .execute_queued_mode_tree(
            attach_pid,
            command,
            &QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("choose-tree opens");

    let pause =
        super::super::super::attach_support::install_attach_control_identity_pause(attach_pid);
    let switch_handler = handler.clone();
    let switch_target = target.clone();
    let switch = tokio::spawn(async move {
        switch_handler
            .apply_choose_tree_default_target(
                attach_pid,
                old_attach_id,
                ChooseTreeTarget {
                    session_name: switch_target,
                    session_id: target_session_id,
                    window_index: None,
                    window_id: None,
                    window_occurrence_id: None,
                    pane_index: None,
                    pane_id: None,
                },
            )
            .await
    });

    pause.reached.notified().await;
    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    let replacement_attach_id = handler
        .register_attach(attach_pid, host.clone(), replacement_tx)
        .await;
    assert_ne!(replacement_attach_id, old_attach_id);
    pause.release.notify_one();

    assert!(
        switch.await.expect("switch task joins").is_err(),
        "stale host generation must fail closed"
    );
    let active_attach = handler.active_attach.lock().await;
    let replacement = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("replacement host survives");
    assert_eq!(replacement.id, replacement_attach_id);
    assert_eq!(replacement.session_name, host);
    drop(active_attach);
    while let Ok(control) = replacement_rx.try_recv() {
        assert!(
            !matches!(control, crate::pane_io::AttachControl::Switch(_)),
            "stale choose-tree switch reached the replacement host"
        );
    }
}

#[tokio::test]
async fn choose_tree_zw_runs_direct_command_only_on_accept() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = SessionName::new("alpha").expect("valid session");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _ = handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;

    let parsed = CommandParser::new()
        .parse_arguments(["choose-tree", "-Zw", "set-buffer", "-b", "chosen", "%%"])
        .expect("choose-tree parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("mode-tree command parses")
        .expect("mode-tree command recognized");

    handler
        .execute_queued_mode_tree(
            attach_pid,
            command,
            &QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("overlay opens");

    {
        let state = handler.state.lock().await;
        assert!(
            state.buffers.get("chosen").is_none(),
            "direct command must not run before accept"
        );
    }

    handler
        .accept_mode_tree_selection(attach_pid)
        .await
        .expect("selection accept succeeds");

    let state = handler.state.lock().await;
    let chosen = state
        .buffers
        .get("chosen")
        .expect("buffer created on accept");
    assert_eq!(String::from_utf8_lossy(chosen), "=alpha:0.");
}

#[tokio::test]
async fn choose_client_from_unattached_request_activates_mode_tree_on_all_attaches() {
    let handler = RequestHandler::new();
    let alpha = SessionName::new("alpha").expect("valid session");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();
    let first_pid = std::process::id();
    let second_pid = first_pid.saturating_add(1);
    let _ = handler
        .register_attach(first_pid, alpha.clone(), first_tx)
        .await;
    let _ = handler.register_attach(second_pid, alpha, second_tx).await;

    let parsed = CommandParser::new()
        .parse_arguments(["choose-client"])
        .expect("choose-client parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("mode-tree command parses")
        .expect("mode-tree command recognized");

    handler
        .execute_queued_mode_tree(
            first_pid.saturating_add(10),
            command,
            &QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("overlay opens");

    let active_attach = handler.active_attach.lock().await;
    assert!(active_attach
        .by_pid
        .get(&first_pid)
        .and_then(|active| active.mode_tree.as_ref())
        .is_some());
    assert!(active_attach
        .by_pid
        .get(&second_pid)
        .and_then(|active| active.mode_tree.as_ref())
        .is_some());
    drop(active_attach);

    let first_overlay = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let Some(control) = first_rx.recv().await else {
                panic!("first attach control channel closed before choose-client overlay");
            };
            if matches!(control, crate::pane_io::AttachControl::Overlay(_)) {
                break;
            }
        }
    })
    .await;
    assert!(
        first_overlay.is_ok(),
        "first attach did not receive choose-client overlay"
    );

    let second_overlay = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let Some(control) = second_rx.recv().await else {
                panic!("second attach control channel closed before choose-client overlay");
            };
            if matches!(control, crate::pane_io::AttachControl::Overlay(_)) {
                break;
            }
        }
    })
    .await;
    assert!(
        second_overlay.is_ok(),
        "second attach did not receive choose-client overlay"
    );
}

#[tokio::test]
async fn mode_tree_commands_without_attached_client_mark_target_pane_mode() {
    for (command, expected_mode, needs_buffer) in [
        ("choose-tree -t alpha:0.0", "tree-mode", false),
        ("find-window -t alpha:0.0 sleep", "tree-mode", false),
        ("find-window -talpha:0.0 sleep", "tree-mode", false),
        ("customize-mode -t alpha:0.0", "options-mode", false),
        ("choose-buffer -t alpha:0.0", "buffer-mode", true),
    ] {
        let handler = RequestHandler::new();
        let alpha = SessionName::new("alpha").expect("valid session");
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: alpha.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));

        if needs_buffer {
            assert!(matches!(
                handler
                    .handle(Request::SetBuffer(Box::new(rmux_proto::SetBufferRequest {
                        name: Some("buf".to_owned()),
                        content: b"hello".to_vec(),
                        append: false,
                        new_name: None,
                        set_clipboard: false,
                        target_client: None,
                    })))
                    .await,
                Response::SetBuffer(_)
            ));
        }

        let parsed = CommandParser::new()
            .parse_arguments(command.split_whitespace())
            .expect("mode command parses");
        let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
            .expect("mode-tree command parses")
            .expect("mode-tree command recognized");

        handler
            .execute_queued_mode_tree(
                std::process::id().saturating_add(10),
                command,
                &QueueExecutionContext::without_caller_cwd(),
            )
            .await
            .expect("detached mode command succeeds");

        let target = rmux_proto::PaneTarget::with_window(alpha, 0, 0);
        let state = handler.state.lock().await;
        let transcript = state.transcript_handle(&target).expect("target transcript");
        let mode = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .pane_mode_name();
        assert_eq!(mode, Some(expected_mode), "command should enter mode");
    }
}

#[tokio::test]
async fn choose_tree_zw_defers_parse_errors_until_accept() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = SessionName::new("alpha").expect("valid session");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _ = handler.register_attach(attach_pid, alpha, control_tx).await;

    let parsed = CommandParser::new()
        .parse_arguments(["choose-tree", "-Zw", "{"])
        .expect("choose-tree parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("mode-tree command parses")
        .expect("mode-tree command recognized");

    handler
        .execute_queued_mode_tree(
            attach_pid,
            command,
            &QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("overlay opens despite invalid direct command");

    let err = handler
        .accept_mode_tree_selection(attach_pid)
        .await
        .expect_err("parse error should surface when accepting");
    let RmuxError::Server(message) = err else {
        panic!("expected server parse error");
    };
    assert!(message.starts_with("mode-tree command parse failed:"));
}
