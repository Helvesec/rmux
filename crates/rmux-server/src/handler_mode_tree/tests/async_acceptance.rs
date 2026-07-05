use super::*;

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
                    .handle(Request::SetBuffer(rmux_proto::SetBufferRequest {
                        name: Some("buf".to_owned()),
                        content: b"hello".to_vec(),
                        append: false,
                        new_name: None,
                        set_clipboard: false,
                    }))
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
