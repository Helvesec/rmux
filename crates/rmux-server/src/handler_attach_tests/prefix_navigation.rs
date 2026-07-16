use super::*;

#[tokio::test]
async fn attached_prefix_right_dispatches_select_pane_right() {
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

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02\x1b[C")
        .await
        .expect("prefix right input");

    assert_eq!(active_panes(&handler, &alpha).await, "0:0\n1:1\n");
}

#[tokio::test]
async fn attached_prefix_n_dispatches_next_window() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    assert!(matches!(
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
        Response::NewWindow(_)
    ));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02n")
        .await
        .expect("prefix n input");

    assert_eq!(active_windows(&handler, &alpha).await, "0:0\n1:1\n");
}

#[tokio::test]
async fn attached_prefix_n_without_next_window_reports_status_message() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    drain_attach_controls(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02n")
        .await
        .expect("prefix n should not terminate the attached client");

    let frame = recv_overlay_frame(&mut control_rx, "prefix n status message").await;
    assert!(
        frame.contains("No next window"),
        "prefix n should render tmux's attached status message, got {frame:?}"
    );
    assert!(
        frame.contains("\x1b[0;30;43m") || frame.contains("\x1b[30;43m"),
        "prefix n should render tmux's default message-style, got {frame:?}"
    );
    assert_eq!(active_windows(&handler, &alpha).await, "0:1\n");
}

#[tokio::test]
async fn attached_prefix_p_without_previous_window_reports_status_message() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    drain_attach_controls(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02p")
        .await
        .expect("prefix p should not terminate the attached client");

    let frame = recv_overlay_frame(&mut control_rx, "prefix p status message").await;
    assert!(
        frame.contains("No previous window"),
        "prefix p should render tmux's attached status message, got {frame:?}"
    );
    assert!(
        frame.contains("\x1b[0;30;43m") || frame.contains("\x1b[30;43m"),
        "prefix p should render tmux's default message-style, got {frame:?}"
    );
    assert_eq!(active_windows(&handler, &alpha).await, "0:1\n");
}

#[tokio::test]
async fn attached_prefix_o_cycles_to_the_next_pane() {
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

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02o")
        .await
        .expect("prefix o input");

    assert_eq!(active_panes(&handler, &alpha).await, "0:1\n1:0\n");
}

#[tokio::test]
async fn attached_prefix_meta_digits_select_tmux_layout_presets() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let control_backlog = {
        let active_attach = handler.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client exists")
            .control_backlog
            .clone()
    };
    let control_drain = tokio::spawn(async move {
        while let Some(control) = control_rx.recv().await {
            crate::pane_io::release_attach_control_backlog(
                &control_backlog,
                control.received_backlog_units(),
            );
        }
    });
    for _ in 0..2 {
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
    }

    for (bytes, expected_layout, starting_layout) in [
        (
            b"\x02\x1b1".as_slice(),
            LayoutName::EvenHorizontal,
            LayoutName::Tiled,
        ),
        (
            b"\x02\x1b2".as_slice(),
            LayoutName::EvenVertical,
            LayoutName::Tiled,
        ),
        (
            b"\x02\x1b3".as_slice(),
            LayoutName::MainHorizontal,
            LayoutName::Tiled,
        ),
        (
            b"\x02\x1b4".as_slice(),
            LayoutName::MainVertical,
            LayoutName::Tiled,
        ),
        (
            b"\x02\x1b5".as_slice(),
            LayoutName::Tiled,
            LayoutName::EvenHorizontal,
        ),
    ] {
        select_layout(&handler, &alpha, starting_layout).await;
        assert_eq!(current_layout(&handler, &alpha).await, starting_layout);
        handler
            .handle_attached_live_input_for_test(requester_pid, bytes)
            .await
            .expect("prefix meta digit input");
        assert_eq!(
            current_layout(&handler, &alpha).await,
            expected_layout,
            "prefix meta digit input {bytes:?} should select {expected_layout:?}"
        );
    }
    control_drain.abort();
}

#[tokio::test]
async fn attached_select_layout_main_horizontal_binding_command_executes() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    for _ in 0..2 {
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
    }
    select_layout(&handler, &alpha, LayoutName::Tiled).await;

    let commands = handler
        .parse_command_string_one_group("select-layout main-horizontal")
        .await
        .expect("binding command parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, commands)
        .await
        .expect("binding command executes");

    assert_eq!(
        current_layout(&handler, &alpha).await,
        LayoutName::MainHorizontal
    );
}

#[tokio::test]
async fn attached_prefix_meta_digit_dispatch_survives_escape_split_across_reads() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    for _ in 0..2 {
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
    }

    select_layout(&handler, &alpha, LayoutName::Tiled).await;
    assert_eq!(current_layout(&handler, &alpha).await, LayoutName::Tiled);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02")
        .await
        .expect("prefix input");

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("escape prefix fragment");
    assert_eq!(pending_input, b"\x1b");
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"1")
        .await
        .expect("meta digit fragment");

    assert!(
        pending_input.is_empty(),
        "meta digit should be fully consumed"
    );
    assert_eq!(
        current_layout(&handler, &alpha).await,
        LayoutName::EvenHorizontal
    );
}

#[tokio::test]
async fn attached_prefix_space_cycles_next_layout_using_current_window_target() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    for _ in 0..2 {
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
    }

    select_layout(&handler, &alpha, LayoutName::Tiled).await;
    assert_eq!(current_layout(&handler, &alpha).await, LayoutName::Tiled);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02 ")
        .await
        .expect("prefix space input");

    assert_eq!(
        current_layout(&handler, &alpha).await,
        LayoutName::EvenHorizontal
    );
}

#[tokio::test]
async fn attached_prefix_q_emits_a_display_panes_overlay() {
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
        .handle_attached_live_input_for_test(requester_pid, b"\x02q")
        .await
        .expect("prefix q input");

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
async fn attached_prefix_help_keys_opens_dismissible_surface_without_q_leak() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    drain_attach_controls(&mut control_rx);

    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"\x02?")
        .await
        .expect("prefix help input");

    assert!(
        !forwarded,
        "prefix help should be consumed by the attach UI"
    );
    assert!(
        pending_input.is_empty(),
        "prefix help should not leave pending input"
    );
    let help_frame = recv_overlay_frame(&mut control_rx, "prefix help overlay").await;
    assert!(
        help_frame.contains("list-keys") || help_frame.contains("List key bindings"),
        "prefix help should render list-keys content, got {help_frame:?}"
    );

    let forwarded = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"q")
        .await
        .expect("prefix help dismiss");

    assert!(
        !forwarded,
        "dismiss key should not be forwarded to the pane"
    );
    let clear_frame = recv_overlay_frame(&mut control_rx, "prefix help clear").await;
    assert!(
        clear_frame.is_empty(),
        "dismissing prefix help should clear the overlay, got {clear_frame:?}"
    );
}
