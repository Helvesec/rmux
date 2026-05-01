use super::*;

async fn enter_copy_mode_with_motion_seed(handler: &RequestHandler, target: &PaneTarget) -> String {
    replace_transcript_contents(
        handler,
        target,
        TerminalSize { cols: 80, rows: 24 },
        b"alpha beta gamma\r\nsecond beta line\r\nthird alpha marker\r\nfourth delta marker\r\nfifth beta tail\x1b[2;6H",
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
    copy_motion_status(handler, target.clone()).await
}

async fn copy_motion_status(handler: &RequestHandler, target: PaneTarget) -> String {
    display_target_format(
        handler,
        target,
        "#{pane_in_mode}:#{copy_cursor_x},#{copy_cursor_y}",
    )
    .await
}

async fn send_copy_motion_key(
    handler: &RequestHandler,
    requester_pid: u32,
    pending_input: &mut Vec<u8>,
    bytes: &[u8],
) {
    let forwarded_to_pane = handler
        .handle_attached_live_input_inner(requester_pid, pending_input, bytes)
        .await
        .expect("copy-mode motion input");
    assert!(
        !forwarded_to_pane,
        "copy-mode motion keys must be consumed instead of forwarded to pane IO"
    );
    assert!(
        pending_input.is_empty(),
        "copy-mode motion input should fully decode and leave no pending bytes"
    );
}

#[tokio::test]
async fn attached_copy_mode_arrow_motion_routes_to_copy_handler_not_pane() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);

    assert_eq!(
        enter_copy_mode_with_motion_seed(&handler, &target).await,
        "1:5,1\n"
    );

    let before_capture = capture_pane_print(&handler, target.clone()).await;
    let mut pending_input = Vec::new();

    send_copy_motion_key(&handler, requester_pid, &mut pending_input, b"\x1b[C").await;
    assert_eq!(
        copy_motion_status(&handler, target.clone()).await,
        "1:6,1\n"
    );

    send_copy_motion_key(&handler, requester_pid, &mut pending_input, b"\x1b[D").await;
    assert_eq!(
        copy_motion_status(&handler, target.clone()).await,
        "1:5,1\n"
    );

    send_copy_motion_key(&handler, requester_pid, &mut pending_input, b"\x1b[B").await;
    assert_eq!(
        copy_motion_status(&handler, target.clone()).await,
        "1:5,2\n"
    );

    send_copy_motion_key(&handler, requester_pid, &mut pending_input, b"\x1b[A").await;
    assert_eq!(
        copy_motion_status(&handler, target.clone()).await,
        "1:5,1\n"
    );

    assert_eq!(
        capture_pane_print(&handler, target.clone()).await,
        before_capture,
        "copy-mode motion keys must not mutate the pane screen"
    );
}

#[tokio::test]
async fn attached_copy_mode_arrow_motion_exits_without_q_leak_and_resumes_pane_input() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_quiet_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    let _ = enter_copy_mode_with_motion_seed(&handler, &target).await;

    let mut pending_input = Vec::new();
    send_copy_motion_key(&handler, requester_pid, &mut pending_input, b"\x1b[C").await;

    let forwarded_to_pane = handler
        .handle_attached_live_input_inner(requester_pid, &mut pending_input, b"q")
        .await
        .expect("q exits copy-mode after motion");
    assert!(
        !forwarded_to_pane,
        "q must be consumed by copy-mode instead of forwarded to pane IO"
    );
    assert_eq!(pane_mode_status(&handler, &alpha).await, "0:::\n");
    assert!(
        !capture_pane_print(&handler, target.clone())
            .await
            .contains("\nq"),
        "q must not appear in the pane capture after copy-mode dismiss"
    );

    let forwarded_to_pane = handler
        .handle_attached_live_input_inner(
            requester_pid,
            &mut pending_input,
            b"RMUX_AFTER_COPY_MOTION",
        )
        .await
        .expect("normal input resumes after copy-mode");
    assert!(
        forwarded_to_pane,
        "normal pane input should resume after copy-mode dismiss"
    );
}
