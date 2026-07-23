use super::*;

#[tokio::test]
async fn live_attach_bracketed_paste_strips_wrappers_when_pane_mode_is_off() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let input = b"\x1b[200~paste\x1b[201~";
    let expected = b"paste";
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-bracketed-paste",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, &input[..4])
        .await
        .expect("first bracketed paste chunk");
    assert_eq!(pending_input, b"\x1b[20");

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, &input[4..])
        .await
        .expect("second bracketed paste chunk");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_bracketed_paste_preserves_wrappers_when_pane_mode_is_enabled() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?2004h")
            .expect("bracketed paste mode transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[200~paste\x1b[201~";
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-bracketed-paste-mode-on",
        expected.len(),
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, expected)
        .await
        .expect("bracketed paste input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_bracketed_paste_is_consumed_without_pane_leak_in_copy_mode() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let entered = handler
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
        .await;
    assert!(matches!(entered, Response::CopyMode(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-bracketed-paste-copy-mode", 0)
            .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[200~secret\x1b[201~")
        .await
        .expect("bracketed paste in copy-mode is consumed");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_chunked_bracketed_paste_is_consumed_without_pane_leak_in_copy_mode() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let entered = handler
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
        .await;
    assert!(matches!(entered, Response::CopyMode(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-chunked-bracketed-paste-copy-mode",
        0,
    )
    .await;

    let mut pending_input = Vec::new();
    for chunk in [b"\x1b[200~sec".as_slice(), b"ret", b"\x1b[201~"] {
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, chunk)
            .await
            .expect("chunked bracketed paste in copy-mode is consumed");
    }
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_bracketed_paste_preserves_multiline_special_payload() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let input = b"\x1b[200~line one\r\nline\ttwo \x02 literal \xe6\x9d\xb1\xe4\xba\xac\x1b[201~";
    let expected = bracketed_paste_body(input);
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-bracketed-paste-special",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    for chunk in [
        &input[..4],
        &input[4..17],
        &input[17..31],
        &input[31..input.len() - 3],
        &input[input.len() - 3..],
    ] {
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, chunk)
            .await
            .expect("bracketed paste chunk");
    }
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_bracketed_paste_forwards_embedded_control_sequences_as_payload() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let chunks: [&[u8]; 7] = [
        b"\x1b[200~literal ",
        b"\x02 prefix ",
        b"\x1b[<64;2",
        b";2M mouse-ish ",
        b"\x1b[9;2u key-ish ",
        b"\x1b[200~ nested-start-ish ",
        b"\x1b[201~",
    ];
    let input = chunks.concat();
    let expected = bracketed_paste_body(&input);
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-bracketed-paste-control-like",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    for chunk in chunks {
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, chunk)
            .await
            .expect("control-like bracketed paste chunk");
    }
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

fn bracketed_paste_body(bytes: &[u8]) -> &[u8] {
    &bytes[b"\x1b[200~".len()..bytes.len() - b"\x1b[201~".len()]
}
