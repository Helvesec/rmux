use super::*;

pub(super) async fn send_attached_copy_mode_command(
    handler: &RequestHandler,
    target: &PaneTarget,
    tokens: &[&str],
) -> Response {
    handler
        .handle(Request::SendKeysExt(rmux_proto::SendKeysExtRequest {
            target: Some(target.clone()),
            keys: tokens.iter().map(|token| (*token).to_owned()).collect(),
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: false,
            copy_mode_command: true,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await
}

#[tokio::test]
async fn attached_copy_mode_emacs_slash_is_unbound_and_not_forwarded() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 24 },
        b"P0-LINE-12\r\n",
    )
    .await;
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
                page_up: false,
            }))
            .await,
        Response::CopyMode(_)
    ));
    assert_eq!(
        pane_mode_status(&handler, &alpha).await,
        "1:copy-mode:0:0\n"
    );
    let mut pending_input = Vec::new();
    let forwarded_to_pane = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"/")
        .await
        .expect("copy-mode slash key");

    assert_eq!(
        pane_mode_status(&handler, &alpha).await,
        "1:copy-mode:0:0\n",
        "default emacs copy-mode must not treat / as a search prompt"
    );
    assert!(
        !forwarded_to_pane,
        "unbound copy-mode keys must be consumed instead of leaking to the pane"
    );
    assert!(
        pending_input.is_empty(),
        "fully decoded key should not be buffered"
    );
}

#[tokio::test]
async fn attached_copy_mode_emacs_ctrl_s_opens_search_prompt() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 24 },
        b"P0-LINE-12\r\n",
    )
    .await;
    assert!(matches!(
        handler
            .handle(Request::CopyMode(CopyModeRequest {
                target: Some(target),
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
        .handle_attached_live_input_for_test(requester_pid, b"\x13P0-LINE-12\r")
        .await
        .expect("copy-mode C-s search");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    assert_eq!(
        pane_mode_status(&handler, &alpha).await,
        "1:copy-mode:1:0\n"
    );
}

#[tokio::test]
async fn attached_copy_mode_gets_first_refusal_for_search_and_selection_keys() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 24 },
        b"P0-LINE-12\r\n",
    )
    .await;

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 0)),
                option: OptionName::ModeKeys,
                value: "vi".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
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
                page_up: false,
            }))
            .await,
        Response::CopyMode(_)
    ));
    assert!(handler
        .target_is_in_copy_mode(&target)
        .await
        .expect("copy-mode status"));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"/P0-LINE-12\r ")
        .await
        .expect("copy-mode attached keys");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(
        pane_mode_status(&handler, &alpha).await,
        "1:copy-mode:1:1\n"
    );
}

#[tokio::test]
async fn attached_copy_mode_q_exits_and_refreshes_normal_surface() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 24 },
        b"P0-LINE-12\r\n",
    )
    .await;
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 0)),
                option: OptionName::ModeKeys,
                value: "vi".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
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
                page_up: false,
            }))
            .await,
        Response::CopyMode(_)
    ));
    assert_eq!(
        pane_mode_status(&handler, &alpha).await,
        "1:copy-mode:0:0\n"
    );
    handler
        .handle_attached_live_input_for_test(requester_pid, b"/P0-LINE-12\r ")
        .await
        .expect("copy-mode search/select attached keys");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(
        pane_mode_status(&handler, &alpha).await,
        "1:copy-mode:1:1\n"
    );
    drain_attach_controls(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"q\x1b")
        .await
        .expect("q exits copy-mode");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(pane_mode_status(&handler, &alpha).await, "0:::\n");
    let frame = recv_render_frame(&mut control_rx, "exit refresh").await;
    assert!(
        !frame.is_empty(),
        "exit refresh should re-render the attached normal surface"
    );
    assert!(
        !capture_pane_print(&handler, target).await.contains("\nq"),
        "q must be consumed by copy-mode instead of leaking to the pane"
    );
}

#[tokio::test]
async fn attached_copy_mode_copies_selection_to_buffer_and_exits_cleanly() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 24 },
        b"alpha\r\nneedle value\r\nomega\r\n",
    )
    .await;
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
                page_up: false,
            }))
            .await,
        Response::CopyMode(_)
    ));
    assert_eq!(
        pane_mode_status(&handler, &alpha).await,
        "1:copy-mode:0:0\n"
    );

    assert!(matches!(
        send_attached_copy_mode_command(&handler, &target, &["search-backward", "--", "needle"])
            .await,
        Response::SendKeys(rmux_proto::SendKeysResponse { key_count: 3 })
    ));
    assert!(matches!(
        send_attached_copy_mode_command(&handler, &target, &["select-word"]).await,
        Response::SendKeys(rmux_proto::SendKeysResponse { key_count: 1 })
    ));
    assert_eq!(
        pane_mode_status(&handler, &alpha).await,
        "1:copy-mode:1:1\n",
        "search and word selection should be active before copy"
    );
    drain_attach_controls(&mut control_rx);

    assert!(matches!(
        send_attached_copy_mode_command(&handler, &target, &["copy-selection-and-cancel"]).await,
        Response::SendKeys(rmux_proto::SendKeysResponse { key_count: 1 })
    ));

    assert_eq!(pane_mode_status(&handler, &alpha).await, "0:::\n");
    let buffer = handler
        .handle(Request::ShowBuffer(rmux_proto::ShowBufferRequest {
            name: None,
        }))
        .await;
    let output = buffer.command_output().expect("show-buffer returns output");
    assert!(
        String::from_utf8_lossy(output.stdout()).contains("needle"),
        "copy-mode transfer should publish the selected text into the rmux buffer"
    );
    let frame = recv_render_frame(&mut control_rx, "copy-mode exit refresh").await;
    assert!(
        !frame.is_empty(),
        "copy-mode copy-and-cancel should refresh the attached normal surface"
    );
}

#[tokio::test]
async fn attached_copy_mode_updates_automatic_window_name_on_entry_and_exit() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);

    let normal_status = display_target_format(
        &handler,
        target.clone(),
        "#{window_name}|#{pane_in_mode}|#{pane_mode}",
    );
    let normal_status = normal_status.await;
    assert!(
        normal_status.ends_with("|0|\n"),
        "normal pane status should report no active mode, got {normal_status:?}"
    );
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
                page_up: false,
            }))
            .await,
        Response::CopyMode(_)
    ));
    assert_eq!(
        display_target_format(
            &handler,
            target.clone(),
            "#{window_name}|#{pane_in_mode}|#{pane_mode}"
        )
        .await,
        "[tmux]|1|copy-mode\n"
    );

    handler
        .handle_attached_live_input_for_test(requester_pid, b"q")
        .await
        .expect("q exits copy-mode");
    let restored_status = display_target_format(
        &handler,
        target,
        "#{window_name}|#{pane_in_mode}|#{pane_mode}",
    )
    .await;
    assert!(
        restored_status.ends_with("|0|\n"),
        "copy-mode exit should restore normal pane mode, got {restored_status:?}"
    );
    assert!(
        !restored_status.starts_with("[tmux]|"),
        "copy-mode exit should restore a process-derived automatic window name, got {restored_status:?}"
    );
}

#[tokio::test]
async fn attached_copy_mode_escape_exits_and_clears_mode_state() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
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
                page_up: false,
            }))
            .await,
        Response::CopyMode(_)
    ));
    assert_eq!(
        pane_mode_status(&handler, &alpha).await,
        "1:copy-mode:0:0\n"
    );
    drain_attach_controls(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b")
        .await
        .expect("Escape exits copy-mode");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(pane_mode_status(&handler, &alpha).await, "0:::\n");
    let _ = recv_matching_attach_control(&mut control_rx, "Escape exit refresh", |control| {
        matches!(control, AttachControl::Switch(_))
    })
    .await;
}

#[tokio::test]
async fn attached_copy_mode_u_refresh_renders_history_backing() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 24 },
        b"copy-u-line-01\r\ncopy-u-line-02\r\ncopy-u-line-03\r\ncopy-u-line-04\r\ncopy-u-line-05\r\ncopy-u-line-06\r\ncopy-u-line-07\r\ncopy-u-line-08\r\ncopy-u-line-09\r\ncopy-u-line-10\r\ncopy-u-line-11\r\ncopy-u-line-12\r\ncopy-u-line-13\r\ncopy-u-line-14\r\ncopy-u-line-15\r\ncopy-u-line-16\r\ncopy-u-line-17\r\ncopy-u-line-18\r\ncopy-u-line-19\r\ncopy-u-line-20\r\ncopy-u-line-21\r\ncopy-u-line-22\r\ncopy-u-line-23\r\ncopy-u-line-24\r\ncopy-u-line-25\r\ncopy-u-line-26\r\ncopy-u-line-27\r\ncopy-u-line-28\r\ncopy-u-line-29\r\ncopy-u-line-30\r\n",
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
    assert!(
        frame.contains("copy-u-line-12"),
        "copy-mode -u attached refresh should render history-backed copy-mode content, got {frame:?}"
    );
    assert_eq!(
        pane_mode_status(&handler, &alpha).await,
        "1:copy-mode:0:0\n"
    );
}

#[tokio::test]
async fn attached_copy_mode_refresh_renders_tmux_position_indicator() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 24 },
        b"copy-position-line\r\n",
    )
    .await;
    drain_attach_controls(&mut control_rx);

    assert!(matches!(
        handler
            .handle(Request::CopyMode(CopyModeRequest {
                target: Some(target),
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

    let frame = recv_render_frame(&mut control_rx, "copy-mode refresh").await;
    assert!(
        frame.contains("[0/0]"),
        "copy-mode attached refresh should render tmux position indicator, got {frame:?}"
    );
}
