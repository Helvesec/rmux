use super::*;

const KITTY_SET: &[u8] = b"\x1b[=8u";
const KITTY_PUSH: &[u8] = b"\x1b[>1u";
const KITTY_POP: &[u8] = b"\x1b[<u";

async fn append_pane_output(
    handler: &RequestHandler,
    session: &rmux_proto::SessionName,
    bytes: &[u8],
) {
    let mut state = handler.state.lock().await;
    state
        .append_bytes_to_pane_transcript_for_test(session, 0, 0, bytes)
        .expect("pane transcript update");
}

async fn assert_activation_request_keeps_standard_encoding(request: &[u8], session_label: &str) {
    let handler = RequestHandler::new();
    let session = session_name(session_label);
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &session).await;
    append_pane_output(&handler, &session, request).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;

    let csi_u_shift_enter = b"\x1b[13;2u";
    let expected = b"\n";
    let capture = RawPaneInputProbe::start(
        &handler,
        &session,
        "ignored-kitty-activation",
        expected.len(),
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, csi_u_shift_enter)
        .await
        .expect("live attach Shift+Enter input");

    capture.finish(&handler, &session).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_kitty_set_and_push_do_not_enable_partial_key_encoding() {
    assert_activation_request_keeps_standard_encoding(KITTY_SET, "kitty-set-noop").await;
    assert_activation_request_keeps_standard_encoding(KITTY_PUSH, "kitty-push-noop").await;
}

#[tokio::test]
async fn live_attach_kitty_pop_does_not_clear_xterm_extended_key_mode() {
    let handler = RequestHandler::new();
    let session = session_name("kitty-pop-noop");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &session).await;
    append_pane_output(&handler, &session, b"\x1b[>4;2m").await;
    append_pane_output(&handler, &session, KITTY_POP).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;

    let csi_u_shift_enter = b"\x1b[13;2u";
    let expected = b"\x1b[27;2;13~";
    let capture =
        RawPaneInputProbe::start(&handler, &session, "ignored-kitty-pop", expected.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, csi_u_shift_enter)
        .await
        .expect("live attach Shift+Enter input");

    capture.finish(&handler, &session).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_ignored_kitty_request_preserves_legacy_escape_and_backspace() {
    let handler = RequestHandler::new();
    let session = session_name("kitty-legacy-keys");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &session).await;
    append_pane_output(&handler, &session, KITTY_SET).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;

    let expected = b"\x1b\x7f";
    let capture = RawPaneInputProbe::start(
        &handler,
        &session,
        "ignored-kitty-legacy-keys",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("standalone escape input");
    assert_eq!(pending_input, b"\x1b");
    assert!(handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("standalone escape flush"));
    assert!(pending_input.is_empty());

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x7f")
        .await
        .expect("legacy backspace input");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &session).await;
    capture.assert_contents(&handler, expected).await;
}
