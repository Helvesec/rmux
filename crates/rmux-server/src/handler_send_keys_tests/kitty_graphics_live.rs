use super::*;

#[tokio::test]
async fn live_attach_kitty_graphics_apc_passes_through_unchanged_when_chunked() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b_Gi=7;OK\x1b\\";
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-kitty-graphics-apc",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b_Gi=7")
        .await
        .expect("first kitty graphics APC chunk");
    assert_eq!(pending_input, b"\x1b_Gi=7");

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b";OK")
        .await
        .expect("second kitty graphics APC chunk");
    assert_eq!(pending_input, b"\x1b_Gi=7;OK");

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b\\")
        .await
        .expect("closing kitty graphics APC chunk");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_kitty_graphics_apc_does_not_capture_meta_underscore() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b_x";
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-meta-underscore",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b_")
        .await
        .expect("meta underscore input");
    assert!(pending_input.is_empty());

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"x")
        .await
        .expect("following literal input");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_terminal_response_is_consumed_when_chunked() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-terminal-response", 0).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b[?62")
        .await
        .expect("first terminal response chunk");
    assert_eq!(pending_input, b"\x1b[?62");

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b";52;c")
        .await
        .expect("second terminal response chunk");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_cursor_position_response_is_forwarded_when_chunked() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[12;34R";
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-cursor-position-response",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b[12;34")
        .await
        .expect("first CPR chunk");
    assert_eq!(pending_input, b"\x1b[12;34");

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"R")
        .await
        .expect("second CPR chunk");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_decrpm_response_is_consumed() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let capture = RawPaneInputProbe::start(&handler, &alpha, "live-attach-decrpm", 0).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[?2004;1$y")
        .await
        .expect("DECRPM response");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_osc_sequences_are_consumed_at_attach_boundary() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let capture = RawPaneInputProbe::start(&handler, &alpha, "live-attach-osc-response", 0).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b]52;c;AA")
        .await
        .expect("first OSC chunk");
    assert_eq!(pending_input, b"\x1b]52;c;AA");

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"AA\x07")
        .await
        .expect("second OSC chunk");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}
