use super::*;

#[tokio::test]
async fn send_keys_uses_runtime_extended_key_format_for_mode_two() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    create_send_keys_test_session(&handler, &alpha).await;

    let set_format = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::ExtendedKeysFormat,
            value: "csi-u".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_format, Response::SetOption(_)));

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[>4;2m")
            .expect("mode 2 transcript update");
    }

    let expected = encode_key(
        mode::MODE_KEYS_EXTENDED_2,
        ExtendedKeyFormat::CsiU,
        key_string_lookup_string("M-C-a").expect("key parses"),
    )
    .expect("extended key encodes");
    let capture = RawPaneInputProbe::start(&handler, &alpha, "extended-key", expected.len()).await;

    let response = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(PaneTarget::new(alpha.clone(), 0)),
            keys: vec!["M-C-a".to_owned()],
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: false,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;
}

#[tokio::test]
async fn send_keys_sends_modified_cursor_keys_without_extended_mode() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    create_send_keys_test_session(&handler, &alpha).await;

    let expected = b"\x1b[1;5A";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "send-keys-c-up", expected.len()).await;
    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            keys: vec!["C-Up".to_owned()],
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn send_keys_m_forwards_the_current_mouse_event_to_the_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1000h")
            .expect("mouse mode transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let (window_id, pane_id, pane_target) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window_at(0).expect("window exists");
        let pane = window.pane(0).expect("pane exists");
        (window.id(), pane.id(), PaneTarget::new(alpha.clone(), 0))
    };

    let raw = MouseForwardEvent {
        b: 0,
        lb: 0,
        x: 1,
        y: 1,
        lx: 1,
        ly: 1,
        sgr_b: 0,
        sgr_type: ' ',
        ignore: false,
    };
    {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attached client exists");
        active.mouse.current_event = Some(AttachedMouseEvent {
            raw,
            session_id: 0,
            window_id: Some(window_id.as_u32()),
            pane_id: Some(pane_id),
            pane_target: Some(pane_target.clone()),
            location: MouseLocation::Pane,
            status_at: None,
            status_lines: 0,
            ignore: false,
        });
    }

    let expected =
        encode_mouse_event(mode::MODE_MOUSE_STANDARD, &raw, raw.x, raw.y).expect("mouse encodes");
    let capture = RawPaneInputProbe::start(&handler, &alpha, "mouse-forward", expected.len()).await;

    let response = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(pane_target),
            keys: Vec::new(),
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: false,
            copy_mode_command: false,
            forward_mouse_event: true,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 0 })
    );

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;
}

#[tokio::test]
async fn live_attach_extended_keys_are_reencoded_for_the_target_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[Z";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-extended-key", expected.len())
            .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[9;2u")
        .await
        .expect("live attach input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_shift_enter_csi_u_survives_extended_key_mode() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let set_format = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::ExtendedKeysFormat,
            value: "csi-u".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_format, Response::SetOption(_)));

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[>4;2m")
            .expect("extended key mode transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[13;2u";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-shift-enter", expected.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, expected)
        .await
        .expect("live attach S-Enter input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_shift_enter_uses_csi_u_after_kitty_keyboard_request() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[>1u")
            .expect("kitty keyboard request transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[13;2u";
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-kitty-shift-enter",
        expected.len(),
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, expected)
        .await
        .expect("live attach Kitty S-Enter input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_standalone_escape_flushes_when_timeout_expires() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-escape-time", expected.len()).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, expected)
        .await
        .expect("standalone escape fragment");
    assert_eq!(pending_input, expected);

    let flushed = handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("pending escape flush");

    assert!(flushed);
    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_fragmented_arrow_consumes_pending_escape_before_timeout() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = encode_key(
        0,
        ExtendedKeyFormat::Xterm,
        key_string_lookup_string("Up").expect("Up parses"),
    )
    .expect("Up encodes");
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-fragmented-up",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("arrow escape prefix");
    assert_eq!(pending_input, b"\x1b");
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"[A")
        .await
        .expect("arrow suffix");

    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;
}

#[tokio::test]
async fn live_attach_fragmented_arrow_survives_target_extended_key_mode() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[>4;2m")
            .expect("extended key mode transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[A";
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-extended-mode-fragmented-up",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("arrow escape prefix");
    assert_eq!(pending_input, b"\x1b");
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"[A")
        .await
        .expect("arrow suffix");

    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_ambiguous_escape_prefixes_wait_for_suffix() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[>4;2m")
            .expect("extended key mode transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    for (label, chunks, expected) in [
        (
            "ss3-up",
            [b"\x1bO".as_slice(), b"A".as_slice()],
            b"\x1b[A".as_slice(),
        ),
        (
            "csi-home",
            [b"\x1b[".as_slice(), b"H".as_slice()],
            b"\x1b[1~".as_slice(),
        ),
        (
            "csi-home-7",
            [b"\x1b[7".as_slice(), b"~".as_slice()],
            b"\x1b[1~".as_slice(),
        ),
        (
            "csi-end-8",
            [b"\x1b[8".as_slice(), b"~".as_slice()],
            b"\x1b[4~".as_slice(),
        ),
        (
            "ss3-f1",
            [b"\x1bO".as_slice(), b"P".as_slice()],
            b"\x1bOP".as_slice(),
        ),
        (
            "csi-f9",
            [b"\x1b[20".as_slice(), b"~".as_slice()],
            b"\x1b[20~".as_slice(),
        ),
    ] {
        let capture = RawPaneInputProbe::start(&handler, &alpha, label, expected.len()).await;
        let mut pending_input = Vec::new();
        for chunk in chunks {
            handler
                .handle_attached_live_input(requester_pid, &mut pending_input, chunk)
                .await
                .expect("fragmented escape sequence");
        }
        assert!(
            pending_input.is_empty(),
            "{label} should not leave pending input"
        );
        capture.finish(&handler, &alpha).await;
        capture.assert_contents(&handler, expected).await;
    }
}

#[tokio::test]
async fn live_attach_fragmented_meta_key_consumes_pending_escape_before_timeout() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = encode_key(
        0,
        ExtendedKeyFormat::Xterm,
        key_string_lookup_string("M-1").expect("M-1 parses"),
    )
    .expect("M-1 encodes");
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-fragmented-meta",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("meta escape prefix");
    assert_eq!(pending_input, b"\x1b");
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"1")
        .await
        .expect("meta suffix");

    assert!(pending_input.is_empty());
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;
}

#[tokio::test]
async fn live_attach_control_bytes_dispatch_tmux_distinct_bindings() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    for (key, literal) in [
        ("C-h", "H"),
        ("BSpace", "B"),
        ("C-j", "J"),
        ("Enter", "E"),
        ("C-Space", "S"),
        ("C-\\", "L"),
        ("C-]", "R"),
        ("C-^", "C"),
        ("C-_", "U"),
    ] {
        let rebound = handler
            .handle(Request::BindKey(BindKeyRequest {
                table_name: "root".to_owned(),
                key: key.to_owned(),
                note: Some("live-attach-control-byte".to_owned()),
                repeat: false,
                command: Some(vec![
                    "send-keys".to_owned(),
                    "-l".to_owned(),
                    literal.to_owned(),
                ]),
            }))
            .await;
        assert!(matches!(rebound, Response::BindKey(_)), "{key} should bind");
    }

    let input = b"\x08\x7f\x0a\x0d\x00\x1c\x1d\x1e\x1f";
    let expected = b"HBJESLRCU";
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-control-bindings",
        expected.len(),
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, input)
        .await
        .expect("live attach control binding input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_nul_dispatches_c_at_alias_binding() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let rebound = handler
        .handle(Request::BindKey(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "C-@".to_owned(),
            note: Some("live-attach-control-byte-alias".to_owned()),
            repeat: false,
            command: Some(vec![
                "send-keys".to_owned(),
                "-l".to_owned(),
                "A".to_owned(),
            ]),
        }))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));

    let capture = RawPaneInputProbe::start(&handler, &alpha, "live-attach-c-at-alias", 1).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x00")
        .await
        .expect("live attach C-@ alias input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"A").await;
}

#[tokio::test]
async fn live_attach_meta_control_bytes_do_not_wait_for_following_input() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b\x01\x1b\x7f";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-meta-control", expected.len())
            .await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b\x01")
        .await
        .expect("meta control input");
    assert!(pending_input.is_empty());
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b\x7f")
        .await
        .expect("meta backspace input");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_committed_utf8_text_preserves_latin_and_ime_payload_chunks() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = "Latin ABC 123 | 日本語かな | 한글 | cafe\u{0301}".as_bytes();
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-committed-utf8-text",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    for chunk in [&expected[..17], &expected[17..35], &expected[35..]] {
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, chunk)
            .await
            .expect("committed utf8 text chunk");
    }
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_preserves_c1_and_malformed_utf8_bytes() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let input = b"\x9bA\xc3(\xe2(\xa1";
    let expected = input;
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-invalid-bytes",
        expected.len(),
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, input)
        .await
        .expect("invalid byte input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_focus_sequences_are_consumed_at_attach_boundary() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let capture = RawPaneInputProbe::start(&handler, &alpha, "live-attach-focus", 0).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[I\x1b[O")
        .await
        .expect("live attach focus input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn live_attach_focus_sequences_forward_when_pane_focus_mode_is_enabled() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1004h")
            .expect("focus mode transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[I\x1b[O";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-focus-mode", expected.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, expected)
        .await
        .expect("live attach focus mode input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_mouse_sequences_dispatch_default_mouse_bindings() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1002h")
            .expect("mouse motion mode transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let rebound = handler
        .handle(Request::BindKey(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "MouseDrag1Pane".to_owned(),
            note: Some("live-attach-mouse".to_owned()),
            repeat: false,
            command: Some(vec!["send-keys".to_owned(), "-M".to_owned()]),
        }))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));

    let expected = encode_mouse_event(
        mode::MODE_MOUSE_BUTTON,
        &MouseForwardEvent {
            b: 32,
            lb: 0,
            x: 1,
            y: 1,
            lx: 0,
            ly: 0,
            sgr_b: 32,
            sgr_type: 'M',
            ignore: false,
        },
        1,
        1,
    )
    .expect("mouse encodes");
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-mouse", expected.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<32;2;2M")
        .await
        .expect("live attach mouse input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;

    let active_attach = handler.active_attach.lock().await;
    let event = active_attach
        .by_pid
        .get(&requester_pid)
        .and_then(|active| active.mouse.current_event.as_ref())
        .expect("current mouse event");
    assert_eq!(event.location, MouseLocation::Pane);
}

#[tokio::test]
async fn live_attach_mouse_down_selects_the_clicked_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)));

    let selected = handler
        .handle(Request::SelectPane(SelectPaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)));

    let mouse_enabled = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Mouse,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(mouse_enabled, Response::SetOption(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let (click_x, click_y) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window();
        assert_eq!(window.active_pane_index(), 0);
        let pane = window.pane(1).expect("pane 1 exists");
        (
            pane.geometry().x().saturating_add(1),
            pane.geometry().y().saturating_add(1),
        )
    };
    let mouse_down = format!("\x1b[<0;{};{}M", click_x + 1, click_y + 1);

    handler
        .handle_attached_live_input_for_test(requester_pid, mouse_down.as_bytes())
        .await
        .expect("live attach mouse down input");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(session.window().active_pane_index(), 1);
}

#[tokio::test]
async fn live_attach_sgr_wheel_forwards_when_pane_mouse_any_is_enabled() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1003h\x1b[?1006h")
            .expect("mouse any and sgr transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = encode_mouse_event(
        mode::MODE_MOUSE_ALL | mode::MODE_MOUSE_SGR,
        &MouseForwardEvent {
            b: 64,
            lb: 0,
            x: 1,
            y: 1,
            lx: 0,
            ly: 0,
            sgr_b: 64,
            sgr_type: 'M',
            ignore: false,
        },
        1,
        1,
    )
    .expect("sgr wheel encodes");
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-sgr-wheel", expected.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[<64;2;2M")
        .await
        .expect("live attach wheel input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;

    let active_attach = handler.active_attach.lock().await;
    let event = active_attach
        .by_pid
        .get(&requester_pid)
        .and_then(|active| active.mouse.current_event.as_ref())
        .expect("current wheel event");
    assert_eq!(event.location, MouseLocation::Pane);
    assert_eq!(event.raw.b, 64);
}

#[tokio::test]
async fn live_attach_sgr_motion_forwards_without_explicit_binding_when_mouse_all_is_enabled() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1003h\x1b[?1006h")
            .expect("mouse all and sgr transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[<35;2;2M";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-sgr-motion", expected.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, expected)
        .await
        .expect("live attach motion input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_sgr_release_forwards_without_explicit_binding_when_mouse_all_is_enabled() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1003h\x1b[?1006h")
            .expect("mouse all and sgr transcript update");
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let expected = b"\x1b[<0;2;2m";
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "live-attach-sgr-release", expected.len()).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, expected)
        .await
        .expect("live attach release input");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_manual_prompt_drag_sequence_does_not_error() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha, control_tx)
        .await;

    let result = handler
        .handle_attached_live_input_for_test(
            requester_pid,
            b"\x1b[<0;7;1M\x1b[<32;9;1M\x1b[<32;10;1M",
        )
        .await;
    assert!(result.is_ok(), "{result:?}");
}
