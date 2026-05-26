use super::*;

#[tokio::test]
async fn attached_prefix_q_repaints_status_line_after_status_message() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 30, rows: 6 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    drain_attach_controls(&mut control_rx);

    let mut status_frame = String::new();
    for _ in 0..16 {
        handler
            .handle_attached_live_input_for_test(requester_pid, b"\x02\"")
            .await
            .expect("prefix quote input");
        while let Ok(control) = control_rx.try_recv() {
            if let AttachControl::Overlay(overlay) = control {
                let frame = String::from_utf8_lossy(&overlay.frame).into_owned();
                if frame.contains("No space for new pane") {
                    status_frame = frame;
                    break;
                }
            }
        }
        if !status_frame.is_empty() {
            break;
        }
    }
    assert!(
        status_frame.contains("No space for new pane"),
        "precondition should render the attached status message, got {status_frame:?}"
    );

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02q")
        .await
        .expect("prefix q input");

    let overlay_frame = recv_overlay_frame(&mut control_rx, "display-panes overlay").await;
    assert!(
        overlay_frame.contains("[alpha]"),
        "display-panes should repaint the normal status line before drawing labels, got {overlay_frame:?}"
    );
    assert!(
        !overlay_frame.contains("No space for new pane"),
        "display-panes should clear the previous message band, got {overlay_frame:?}"
    );
}

#[tokio::test]
async fn attached_prefix_x_during_display_panes_opens_kill_pane_prompt() {
    if skip_if_passthrough("display-panes overlay rendering differs under passthrough alt-screen wrap") { return; }
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
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
            .handle(Request::SelectPane(SelectPaneRequest {
                target: PaneTarget::new(alpha.clone(), 1),
                title: None,
            }))
            .await,
        Response::SelectPane(_)
    ));
    drain_attach_controls(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02q")
        .await
        .expect("prefix q input");
    let display_panes_frame = recv_overlay_frame(&mut control_rx, "display-panes overlay").await;
    assert!(
        display_panes_frame.contains("\x1b[?25l"),
        "prefix q should enter display-panes, got {display_panes_frame:?}"
    );

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02x")
        .await
        .expect("prefix x input during display-panes");
    let prompt = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if let Some(prompt) = handler.attached_prompt_render(requester_pid).await {
                break prompt;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("kill-pane prompt after display-panes timeout");
    assert!(
        prompt.prompt.contains("kill-pane"),
        "prefix x during display-panes should open the normal kill-pane prompt, got {prompt:?}"
    );
}

#[tokio::test]
async fn attached_prefix_q_emits_a_display_panes_overlay_when_prefix_and_q_arrive_separately() {
    if skip_if_passthrough("display-panes overlay rendering differs under passthrough alt-screen wrap") { return; }
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
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
    drain_attach_controls(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02")
        .await
        .expect("prefix input");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"q")
        .await
        .expect("q input");

    let overlay = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let next = control_rx
                .recv()
                .await
                .expect("display-panes overlay control");
            if matches!(next, AttachControl::Overlay(_)) {
                break next;
            }
        }
    })
    .await
    .expect("display-panes overlay should arrive");
    assert!(
        matches!(overlay, AttachControl::Overlay(_)),
        "expected display-panes overlay, got {overlay:?}"
    );
}

#[tokio::test]
async fn display_panes_bounds_unterminated_sgr_mouse_without_pane_leak() {
    if skip_if_passthrough("display-panes overlay rendering differs under passthrough alt-screen wrap") { return; }
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    drain_attach_controls(&mut control_rx);
    let before_capture = capture_pane_print(&handler, target.clone()).await;

    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"\x02q")
        .await
        .expect("prefix q input");
    assert!(
        !forwarded,
        "display-panes prefix should be consumed by the attach UI"
    );
    let overlay = recv_overlay_frame(&mut control_rx, "display-panes overlay").await;
    assert!(
        overlay.contains("\x1b[?25l"),
        "prefix q should enter display-panes, got {overlay:?}"
    );

    let partial = oversized_unterminated_sgr_mouse_input();
    let result = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, &partial)
        .await;
    assert_partial_control_bound(result, "display-panes prompt input");
    assert!(
        pending_input.is_empty(),
        "overflowing display-panes partial input should be cleared after rejection"
    );
    assert_eq!(
        capture_pane_print(&handler, target.clone()).await,
        before_capture,
        "unterminated display-panes control input must not mutate the pane screen"
    );

    let recovered = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("escape should still close display-panes after partial-input rejection");
    assert!(
        !recovered,
        "display-panes escape must not be forwarded to pane IO"
    );
    let clear = recv_overlay_frame(&mut control_rx, "display-panes clear").await;
    assert!(
        !clear.is_empty(),
        "display-panes should repaint or clear after recovery escape"
    );
}

#[tokio::test]
async fn attached_prefix_q_emits_a_display_panes_clear_after_the_timeout() {
    if skip_if_passthrough("display-panes overlay rendering differs under passthrough alt-screen wrap") { return; }
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Session(alpha.clone()),
                OptionName::DisplayPanesTime,
                "25".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("set display-panes-time");
    }
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
    drain_attach_controls(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02q")
        .await
        .expect("prefix q input");

    let overlay = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let next = control_rx
                .recv()
                .await
                .expect("display-panes overlay control");
            if let AttachControl::Overlay(overlay) = next {
                break overlay;
            }
        }
    })
    .await
    .expect("display-panes overlay should arrive");
    assert!(
        !overlay.frame.is_empty(),
        "display-panes overlay should render a non-empty frame"
    );

    let mut saw = Vec::new();
    let clear = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let next = control_rx.recv().await.expect("follow-up control");
            match next {
                AttachControl::Overlay(clear) => break clear,
                other => saw.push(format!("{other:?}")),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("clear overlay should arrive; saw {saw:?}"));
    assert!(
        !clear.frame.is_empty(),
        "display-panes clear overlay should repaint the client"
    );
}

#[tokio::test]
async fn attached_prefix_q_inside_choose_tree_restores_the_tree_overlay_without_base_clear() {
    if skip_if_passthrough("choose-tree + display-panes overlay differs under passthrough alt-screen wrap") { return; }
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Session(alpha.clone()),
                OptionName::DisplayPanesTime,
                "25".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("set display-panes-time");
    }
    assert!(matches!(
        handler
            .handle(Request::NewWindow(NewWindowRequest {
                target: alpha.clone(),
                name: Some("w1".to_owned()),
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                target_window_index: None,
                insert_at_target: false,
            }))
            .await,
        Response::NewWindow(_)
    ));
    let commands = handler
        .parse_control_commands("choose-tree -Zw")
        .await
        .expect("choose-tree parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, commands)
        .await
        .expect("choose-tree activates");
    let initial_tree_overlay =
        recv_overlay_frame(&mut control_rx, "initial choose-tree overlay").await;
    assert!(
        initial_tree_overlay.contains("sort: index"),
        "choose-tree precondition should render the tree overlay, got: {initial_tree_overlay:?}"
    );
    drain_attach_controls(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02q")
        .await
        .expect("prefix q input inside choose-tree");

    let _display_panes_overlay = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let next = control_rx
                .recv()
                .await
                .expect("display-panes overlay control");
            if let AttachControl::Overlay(overlay) = next {
                break overlay;
            }
        }
    })
    .await
    .expect("display-panes overlay should arrive");

    let restored = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let next = control_rx.recv().await.expect("follow-up control");
            match next {
                AttachControl::Overlay(overlay) => {
                    if String::from_utf8_lossy(&overlay.frame).contains("sort: index") {
                        break overlay;
                    }
                }
                AttachControl::AdvancePersistentOverlayState(_) => {}
                AttachControl::Switch(_) => {}
                AttachControl::Write(_) => {}
                AttachControl::LockShellCommand(_) => {}
                AttachControl::Detach => panic!("unexpected detach"),
                AttachControl::Exited => panic!("unexpected exited"),
                AttachControl::DetachKill => panic!("unexpected detach kill"),
                AttachControl::DetachExecShellCommand(_) => panic!("unexpected detach exec"),
                AttachControl::Suspend => panic!("unexpected suspend"),
            }
        }
    })
    .await
    .expect("choose-tree overlay should be restored after timeout");
    let restored_frame =
        String::from_utf8(restored.frame).expect("restored overlay frame must be utf-8");
    assert!(
        restored_frame.contains("sort: index"),
        "display-panes timeout inside choose-tree should restore the tree overlay directly, got: {restored_frame:?}"
    );
}
