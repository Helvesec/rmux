use super::*;

#[tokio::test]
async fn attached_copy_mode_u_attach_render_matches_mode_capture_source() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 40, rows: 6 },
        b"mode-render-01\r\nmode-render-02\r\nmode-render-03\r\nmode-render-04\r\nmode-render-05\r\nmode-render-06\r\nmode-render-07\r\nmode-render-08\r\n",
    )
    .await;
    drain_attach_controls(&mut control_rx);

    assert!(matches!(
        handler
            .handle(Request::CopyMode(CopyModeRequest {
                target: Some(target.clone()),
                page_down: false,
                exit_on_scroll: false,
                hide_position: false,
                mouse_drag_start: false,
                cancel_mode: false,
                scrollbar_scroll: false,
                source: None,
                page_up: true,
            }))
            .await,
        Response::CopyMode(_)
    ));

    let frame = recv_render_frame(&mut control_rx, "copy-mode -u refresh").await;
    let mode_capture = {
        let response = handler
            .handle(Request::CapturePane(Box::new(CapturePaneRequest {
                target,
                start: None,
                end: None,
                print: true,
                buffer_name: None,
                alternate: false,
                escape_ansi: false,
                escape_sequences: false,
                include_format: false,
                hyperlinks: false,
                line_numbers: false,
                join_wrapped: false,
                use_mode_screen: true,
                preserve_trailing_spaces: false,
                do_not_trim_spaces: false,
                pending_input: false,
                quiet: false,
                start_is_absolute: false,
                end_is_absolute: false,
            })))
            .await;
        let output = response
            .command_output()
            .expect("capture-pane -p -M should return command output");
        String::from_utf8(output.stdout().to_vec()).expect("mode capture stdout is utf-8")
    };
    assert!(
        frame.contains("mode-render-03"),
        "attached refresh should include the same copy-mode backing content, got {frame:?}"
    );
    assert!(
        mode_capture.contains("mode-render-03"),
        "mode capture should include the copy-mode backing content, got {mode_capture:?}"
    );
}

#[tokio::test]
async fn attached_mouse_drag_copy_mode_refresh_keeps_prompt_visible() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 24 },
        b"\x1b[1m\x1b[32mtester@RMUXHOST\x1b[0m:\x1b[1m\x1b[34m~\x1b[0m$ ",
    )
    .await;
    drain_attach_controls(&mut control_rx);

    let (window_id, pane_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window_at(0).expect("window exists");
        let pane = window.pane(0).expect("pane exists");
        (window.id(), pane.id())
    };

    let mut active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get_mut(&requester_pid)
        .expect("attached client exists");
    active.mouse.current_event = Some(AttachedMouseEvent {
        raw: MouseForwardEvent {
            b: 32,
            lb: 0,
            x: 22,
            y: 17,
            lx: 22,
            ly: 17,
            sgr_b: 32,
            sgr_type: 'M',
            ignore: false,
        },
        session_id: 0,
        window_id: Some(window_id.as_u32()),
        pane_id: Some(pane_id),
        pane_target: Some(target.clone()),
        location: MouseLocation::Pane,
        status_at: None,
        status_lines: 0,
        ignore: false,
    });
    drop(active_attach);

    assert!(matches!(
        handler
            .handle(Request::CopyMode(CopyModeRequest {
                target: Some(target.clone()),
                page_down: false,
                exit_on_scroll: false,
                hide_position: false,
                mouse_drag_start: true,
                cancel_mode: false,
                scrollbar_scroll: false,
                source: None,
                page_up: false,
            }))
            .await,
        Response::CopyMode(_)
    ));

    let frame = recv_render_frame(&mut control_rx, "copy-mode mouse refresh").await;
    let mode_capture = {
        let response = handler
            .handle(Request::CapturePane(Box::new(CapturePaneRequest {
                target: target.clone(),
                start: None,
                end: None,
                print: true,
                buffer_name: None,
                alternate: false,
                escape_ansi: false,
                escape_sequences: false,
                include_format: false,
                hyperlinks: false,
                line_numbers: false,
                join_wrapped: false,
                use_mode_screen: true,
                preserve_trailing_spaces: false,
                do_not_trim_spaces: false,
                pending_input: false,
                quiet: false,
                start_is_absolute: false,
                end_is_absolute: false,
            })))
            .await;
        let output = response
            .command_output()
            .expect("capture-pane -p -M should return command output");
        String::from_utf8(output.stdout().to_vec()).expect("mode capture stdout is utf-8")
    };
    let rendered_screen = {
        let mut screen = Screen::new(TerminalSize { cols: 80, rows: 24 }, 0);
        let mut parser = InputParser::new();
        parser.parse(frame.as_bytes(), &mut screen);
        String::from_utf8(screen.capture_transcript(Default::default(), Default::default()))
            .expect("render frame replay is utf-8")
    };

    assert!(
        mode_capture.contains("tester@RMUXHOST:~$"),
        "mode capture should retain the prompt line, got {mode_capture:?}"
    );
    assert!(
        rendered_screen.contains("tester@RMUXHOST:~$"),
        "attached copy-mode mouse refresh should keep the prompt visible after replay, frame={frame:?} replay={rendered_screen:?}"
    );
    // A drag start has no selected cells yet, so this frame may only carry
    // the position indicator's mode-style. Extend the selection and require
    // a second mode-style span: the indicator alone must not satisfy the
    // selection-paint assertion (the previous form of this test passed even
    // when selections were never painted, issue #90).
    assert!(
        frame.contains("\u{1b}[0;30;43m"),
        "attached copy-mode mouse refresh should style the position indicator, frame={frame:?}"
    );
    assert!(matches!(
        super::copy_mode_keys::send_attached_copy_mode_command(&handler, &target, &["select-line"])
            .await,
        Response::SendKeys(_)
    ));
    let frame = recv_render_frame(&mut control_rx, "copy-mode selection refresh").await;
    assert!(
        frame.contains("\u{1b}[30m\u{1b}[43m"),
        "extending the selection must paint selected cells with tmux mode-style; \
         the position indicator's combined ESC[0;30;43m form alone must not \
         satisfy this, frame={frame:?}"
    );
}

#[tokio::test]
async fn attached_copy_mode_unhandled_key_falls_back_to_prefix_table() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: rmux_proto::SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                target: PaneTarget::new(alpha.clone(), 0),
                title: None,
                style: None,
                input_disabled: None,
                preserve_zoom: false,
            })))
            .await,
        Response::SelectPane(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::CopyMode(CopyModeRequest {
                target: Some(PaneTarget::new(alpha.clone(), 0)),
                page_down: false,
                exit_on_scroll: false,
                hide_position: false,
                mouse_drag_start: false,
                cancel_mode: false,
                scrollbar_scroll: false,
                source: None,
                page_up: false,
            }))
            .await,
        Response::CopyMode(_)
    ));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02o")
        .await
        .expect("copy-mode declined unhandled prefix navigation");

    assert_eq!(active_panes(&handler, &alpha).await, "0:0\n1:1\n");
}
