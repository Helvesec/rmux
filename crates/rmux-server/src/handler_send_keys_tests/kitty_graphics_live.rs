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
async fn live_attach_meta_underscore_forwards_unchanged_after_escape_timeout() {
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
    assert_eq!(pending_input, b"\x1b_");
    assert!(
        handler
            .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
            .await
            .expect("meta underscore escape timeout"),
        "timed-out meta underscore is forwarded",
    );
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
async fn live_attach_meta_underscore_dispatches_root_binding_after_escape_timeout() {
    let handler = RequestHandler::new();
    let alpha = session_name("meta-underscore-binding");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "M-_".to_owned(),
            note: Some("timed-out meta underscore".to_owned()),
            repeat: false,
            command: Some(vec![
                "send-keys".to_owned(),
                "-l".to_owned(),
                "B".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)), "{rebound:?}");

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-meta-underscore-binding", 1).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b_")
        .await
        .expect("ambiguous meta underscore waits for escape timeout");
    assert_eq!(pending_input, b"\x1b_");
    assert!(
        !handler
            .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
            .await
            .expect("meta underscore escape timeout dispatches binding"),
        "bound meta underscore is consumed instead of forwarded",
    );
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"B").await;
}

#[tokio::test]
async fn meta_underscore_then_typed_g_flushes_binding_and_unblocks_input() {
    // Alt+_ followed by a typed 'G' selects the kitty APC opener \x1b_G,
    // whose payload scan only ends on ST. If its streaming idle budget later
    // expires, the flush still resolves it as keyboard input (M-_ through the
    // key tables, body bytes rerouted) instead of swallowing every subsequent
    // keystroke — the same class as the fixed consumed-OSC M-] 5 2 ; swallow.
    let handler = RequestHandler::new();
    let alpha = session_name("meta-underscore-typed-g");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "M-_".to_owned(),
            note: None,
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "apc-flushed".to_owned(),
                "ok".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)), "{rebound:?}");
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "meta-underscore-typed-g", 0).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b_")
        .await
        .expect("ambiguous meta underscore waits for escape timeout");
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"G")
        .await
        .expect("typed G stays retained as a possible kitty APC");
    assert_eq!(pending_input, b"\x1b_G");
    handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("timed-out M-_ input dispatches the binding");
    assert!(pending_input.is_empty());
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"x")
        .await
        .expect("input after the flush is not swallowed");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"Gx").await;
    let state = handler.state.lock().await;
    let (_, contents) = state
        .buffers
        .show(Some("apc-flushed"))
        .expect("M-_ binding fired from the idle-timeout flush");
    assert_eq!(contents, b"ok");
}

#[tokio::test]
async fn live_attach_meta_underscore_timeout_respects_read_only_transition() {
    let handler = RequestHandler::new();
    let alpha = session_name("meta-underscore-read-only");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "read-only-meta-underscore", 0).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b_")
        .await
        .expect("ambiguous meta underscore waits for escape timeout");
    assert_eq!(pending_input, b"\x1b_");
    {
        let mut active_attach = handler.active_attach.lock().await;
        active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attach is active")
            .flags
            .insert(crate::client_flags::ClientFlags::READONLY);
    }

    assert!(
        !handler
            .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
            .await
            .expect("read-only escape timeout is dropped"),
        "read-only timed-out input is not forwarded",
    );
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
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

    let response = b"\x1b]52;c;AAAA\x07";
    for split in 1..response.len() {
        let mut pending_input = Vec::new();
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, &response[..split])
            .await
            .expect("first fragmented OSC chunk");
        assert_eq!(pending_input, response[..split], "split at byte {split}");

        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, &response[split..])
            .await
            .expect("second fragmented OSC chunk");
        assert!(pending_input.is_empty(), "split at byte {split}");
    }

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn ambiguous_alt_right_bracket_is_forwarded_after_escape_timeout() {
    let handler = RequestHandler::new();
    let alpha = session_name("live-alt-right-bracket");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "live-alt-right-bracket", 0).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b]")
        .await
        .expect("ambiguous OSC prefix waits for escape timeout");
    assert_eq!(pending_input, b"\x1b]");
    handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("timed-out Alt-] input is forwarded");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"\x1b]").await;
}

#[tokio::test]
async fn alt_right_bracket_binding_fires_after_escape_timeout_and_unblocks_input() {
    let handler = RequestHandler::new();
    let alpha = session_name("live-alt-right-bracket-binding");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let bound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "M-]".to_owned(),
            note: None,
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "flushed".to_owned(),
                "ok".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(bound, Response::BindKey(_)), "{bound:?}");
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-alt-right-bracket-binding", 0).await;

    // Alt+] followed by typed body bytes accumulates a full consumed-OSC
    // prefix with no terminator. If its streaming idle budget expires, the
    // flush must resolve it as keyboard input: dispatch M-] through the key
    // tables and reroute the body bytes, instead of opening an unterminated
    // OSC in the pane and swallowing every subsequent keystroke.
    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b]")
        .await
        .expect("ambiguous OSC prefix waits for escape timeout");
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"52;")
        .await
        .expect("typed body bytes stay retained");
    assert_eq!(pending_input, b"\x1b]52;");
    handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("timed-out M-] input dispatches the binding");
    assert!(pending_input.is_empty());
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"x")
        .await
        .expect("input after the flush is not swallowed");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"52;x").await;
    let state = handler.state.lock().await;
    let (_, contents) = state
        .buffers
        .show(Some("flushed"))
        .expect("M-] binding fired from the idle-timeout flush");
    assert_eq!(contents, b"ok");
}

#[tokio::test]
async fn ambiguous_alt_right_bracket_dispatches_root_binding_after_escape_timeout() {
    let handler = RequestHandler::new();
    let alpha = session_name("live-alt-right-bracket-root-binding");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "M-]".to_owned(),
            note: Some("timed-out meta right bracket".to_owned()),
            repeat: false,
            command: Some(vec![
                "send-keys".to_owned(),
                "-l".to_owned(),
                "R".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)), "{rebound:?}");

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-alt-right-bracket-root-binding", 1).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b]")
        .await
        .expect("ambiguous Alt-] waits for escape timeout");
    assert_eq!(pending_input, b"\x1b]");
    assert!(
        !handler
            .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
            .await
            .expect("Alt-] escape timeout dispatches binding"),
        "bound Alt-] is consumed instead of forwarded",
    );
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"R").await;
}
