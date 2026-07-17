use super::*;

#[tokio::test]
async fn live_attach_synchronize_panes_writes_to_each_live_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(PaneTarget::new(alpha.clone(), 0)),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)));

    let select_first = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(select_first, Response::SelectPane(_)));

    let set_sync = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 0)),
            option: OptionName::SynchronizePanes,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_sync, Response::SetOption(_)));

    let pane_zero = PaneTarget::with_window(alpha.clone(), 0, 0);
    let pane_one = PaneTarget::with_window(alpha.clone(), 0, 1);
    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&pane_zero);
        state.start_pane_input_capture_for_test(&pane_one);
    }

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"sync")
        .await
        .expect("live attach input");

    let state = handler.state.lock().await;
    assert_eq!(
        state.pane_input_capture_for_test(&pane_zero),
        Some(b"sync".to_vec())
    );
    assert_eq!(
        state.pane_input_capture_for_test(&pane_one),
        Some(b"sync".to_vec())
    );
}

#[tokio::test]
async fn live_attach_synchronize_panes_keeps_terminal_responses_on_active_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("sync-terminal-response");
    let (requester_pid, pane_zero, pane_one) = setup_synchronized_attached_mode_captures(
        &handler,
        &alpha,
        0,
        b"\x1b[?1004h",
        b"\x1b[?1004h",
    )
    .await;

    let kitty_response = b"\x1b_Gi=7;OK\x1b\\";
    let mut expected = Vec::new();
    for split in 1..kitty_response.len() {
        let mut pending_input = Vec::new();
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, &kitty_response[..split])
            .await
            .expect("first fragmented Kitty APC chunk succeeds");
        assert_eq!(pending_input, kitty_response[..split], "split at {split}");
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, &kitty_response[split..])
            .await
            .expect("second fragmented Kitty APC chunk succeeds");
        assert!(pending_input.is_empty(), "split at {split}");
        expected.extend_from_slice(kitty_response);
    }

    let mut pending_input = Vec::new();
    for response_fragment in [
        b"\x1b[12;".as_slice(),
        b"34R".as_slice(),
        b"\x1b[".as_slice(),
        b"I".as_slice(),
    ] {
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, response_fragment)
            .await
            .expect("pane-bound terminal response succeeds");
    }
    assert!(pending_input.is_empty());
    expected.extend_from_slice(b"\x1b[12;34R\x1b[I");

    let state = handler.state.lock().await;
    assert_eq!(
        state.pane_input_capture_for_test(&pane_zero),
        Some(expected)
    );
    assert_eq!(
        state.pane_input_capture_for_test(&pane_one),
        Some(Vec::new())
    );
}

#[tokio::test]
async fn live_attach_synchronize_panes_wraps_paste_per_pane_when_active_is_plain() {
    let handler = RequestHandler::new();
    let alpha = session_name("sync-paste-active-plain");
    let (requester_pid, pane_zero, pane_one) = setup_synchronized_attached_mode_captures(
        &handler,
        &alpha,
        0,
        b"\x1b[?2004l",
        b"\x1b[?2004h",
    )
    .await;

    let body = b"A\nB";
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[200~A\nB\x1b[201~")
        .await
        .expect("synchronized bracketed paste succeeds");

    let state = handler.state.lock().await;
    assert_eq!(
        state.pane_input_capture_for_test(&pane_zero),
        Some(body.to_vec())
    );
    assert_eq!(
        state.pane_input_capture_for_test(&pane_one),
        Some(b"\x1b[200~A\nB\x1b[201~".to_vec())
    );
}

#[tokio::test]
async fn live_attach_synchronize_panes_strips_paste_per_pane_when_active_is_bracketed() {
    let handler = RequestHandler::new();
    let alpha = session_name("sync-paste-active-bracketed");
    let (requester_pid, pane_zero, pane_one) = setup_synchronized_attached_mode_captures(
        &handler,
        &alpha,
        1,
        b"\x1b[?2004l",
        b"\x1b[?2004h",
    )
    .await;

    let wrapped = b"\x1b[200~A\nB\x1b[201~";
    handler
        .handle_attached_live_input_for_test(requester_pid, wrapped)
        .await
        .expect("synchronized bracketed paste succeeds");

    let state = handler.state.lock().await;
    assert_eq!(
        state.pane_input_capture_for_test(&pane_zero),
        Some(b"A\nB".to_vec())
    );
    assert_eq!(
        state.pane_input_capture_for_test(&pane_one),
        Some(wrapped.to_vec())
    );
}

#[tokio::test]
async fn live_attach_synchronize_panes_encodes_decckm_per_pane_when_active_is_normal() {
    let handler = RequestHandler::new();
    let alpha = session_name("sync-cursor-active-normal");
    let (requester_pid, pane_zero, pane_one) =
        setup_synchronized_attached_mode_captures(&handler, &alpha, 0, b"\x1b[?1l", b"\x1b[?1h")
            .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[A")
        .await
        .expect("synchronized cursor key succeeds");

    assert_per_pane_up_encoding(&handler, &pane_zero, &pane_one).await;
}

#[tokio::test]
async fn live_attach_synchronize_panes_encodes_decckm_per_pane_when_active_is_application() {
    let handler = RequestHandler::new();
    let alpha = session_name("sync-cursor-active-application");
    let (requester_pid, pane_zero, pane_one) =
        setup_synchronized_attached_mode_captures(&handler, &alpha, 1, b"\x1b[?1l", b"\x1b[?1h")
            .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[A")
        .await
        .expect("synchronized cursor key succeeds");

    assert_per_pane_up_encoding(&handler, &pane_zero, &pane_one).await;
}

#[tokio::test]
async fn send_keys_synchronize_panes_writes_to_each_live_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    create_synchronized_two_pane_session(&handler, &alpha).await;

    let pane_zero = PaneTarget::with_window(alpha.clone(), 0, 0);
    let pane_one = PaneTarget::with_window(alpha.clone(), 0, 1);
    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&pane_zero);
        state.start_pane_input_capture_for_test(&pane_one);
    }

    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: pane_zero.clone(),
            keys: vec!["sync".to_owned()],
        }))
        .await;
    assert!(matches!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    ));

    let state = handler.state.lock().await;
    assert_eq!(
        state.pane_input_capture_for_test(&pane_zero),
        Some(b"sync".to_vec())
    );
    assert_eq!(
        state.pane_input_capture_for_test(&pane_one),
        Some(b"sync".to_vec())
    );
}

#[cfg(windows)]
#[tokio::test]
async fn pane_input_ref_multi_token_ctrl_c_stays_on_referenced_pane_when_synchronized() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    create_synchronized_two_pane_session(&handler, &alpha).await;

    let pane_zero = PaneTarget::with_window(alpha.clone(), 0, 0);
    let pane_one = PaneTarget::with_window(alpha.clone(), 0, 1);
    let pane_zero_id = {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&pane_zero);
        state.start_pane_input_capture_for_test(&pane_one);
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("test pane exists")
    };

    let response = handler
        .handle(Request::PaneInput(rmux_proto::PaneInputRequest {
            target: PaneTargetRef::by_id(alpha.clone(), pane_zero_id),
            keys: vec!["C-c".to_owned(), "Enter".to_owned()],
            literal: false,
        }))
        .await;
    assert!(matches!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 2 })
    ));

    let mut expected = vec![0x03];
    let enter = key_string_lookup_string("Enter").expect("Enter key exists");
    expected.extend_from_slice(&encode_key(0, ExtendedKeyFormat::Xterm, enter).unwrap());

    let state = handler.state.lock().await;
    assert_eq!(
        state.pane_input_capture_for_test(&pane_zero),
        Some(expected)
    );
    assert_eq!(
        state.pane_input_capture_for_test(&pane_one),
        Some(Vec::new())
    );
}

#[tokio::test]
async fn send_prefix_synchronize_panes_writes_to_each_live_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    create_synchronized_two_pane_session(&handler, &alpha).await;

    let pane_zero = PaneTarget::with_window(alpha.clone(), 0, 0);
    let pane_one = PaneTarget::with_window(alpha.clone(), 0, 1);
    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&pane_zero);
        state.start_pane_input_capture_for_test(&pane_one);
    }

    let response = handler
        .handle(Request::SendPrefix(SendPrefixRequest {
            target: Some(pane_zero.clone()),
            secondary: false,
        }))
        .await;
    assert!(matches!(
        response,
        Response::SendPrefix(SendPrefixResponse { key_count: 1, .. })
    ));

    let state = handler.state.lock().await;
    assert_eq!(
        state.pane_input_capture_for_test(&pane_zero),
        Some(b"\x02".to_vec())
    );
    assert_eq!(
        state.pane_input_capture_for_test(&pane_one),
        Some(b"\x02".to_vec())
    );
}

async fn create_synchronized_two_pane_session(
    handler: &RequestHandler,
    alpha: &rmux_proto::SessionName,
) {
    // Inert, silent panes: these tests stamp DEC private modes onto the
    // transcripts and assert per-pane input encoding, so a real login shell's
    // startup output must not race the stamped modes (the same class as the
    // double-click content race fixed in live_attach.rs).
    create_quiet_mouse_session(handler, alpha).await;
    let split = handler
        .handle(Request::SplitWindowExt(Box::new(
            rmux_proto::SplitWindowExtRequest {
                target: SplitWindowTarget::Pane(PaneTarget::new(alpha.clone(), 0)),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
                command: Some(quiet_pane_command()),
                process_command: None,
                start_directory: None,
                keep_alive_on_exit: None,
                detached: false,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            },
        )))
        .await;
    let Response::SplitWindow(split) = split else {
        panic!("expected split-window response: {split:?}");
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&split.pane)
        .await;

    let select_first = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(select_first, Response::SelectPane(_)));

    let set_sync = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 0)),
            option: OptionName::SynchronizePanes,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_sync, Response::SetOption(_)));
}

async fn setup_synchronized_attached_mode_captures(
    handler: &RequestHandler,
    session_name: &rmux_proto::SessionName,
    active_pane: u32,
    pane_zero_mode: &[u8],
    pane_one_mode: &[u8],
) -> (u32, PaneTarget, PaneTarget) {
    create_synchronized_two_pane_session(handler, session_name).await;
    let pane_zero = PaneTarget::with_window(session_name.clone(), 0, 0);
    let pane_one = PaneTarget::with_window(session_name.clone(), 0, 1);
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(session_name, 0, 0, pane_zero_mode)
            .expect("first pane mode update succeeds");
        state
            .append_bytes_to_pane_transcript_for_test(session_name, 0, 1, pane_one_mode)
            .expect("second pane mode update succeeds");
        state.start_pane_input_capture_for_test(&pane_zero);
        state.start_pane_input_capture_for_test(&pane_one);
    }

    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::with_window(session_name.clone(), 0, active_pane),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)));

    let requester_pid = std::process::id();
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, session_name.clone(), control_tx)
        .await;
    (requester_pid, pane_zero, pane_one)
}

async fn assert_per_pane_up_encoding(
    handler: &RequestHandler,
    normal_target: &PaneTarget,
    application_target: &PaneTarget,
) {
    let up = key_string_lookup_string("Up").expect("Up key exists");
    let normal = encode_key(0, ExtendedKeyFormat::Xterm, up).expect("normal Up encodes");
    let application = encode_key(mode::MODE_KCURSOR, ExtendedKeyFormat::Xterm, up)
        .expect("application Up encodes");
    let state = handler.state.lock().await;
    assert_eq!(
        state.pane_input_capture_for_test(normal_target),
        Some(normal)
    );
    assert_eq!(
        state.pane_input_capture_for_test(application_target),
        Some(application)
    );
}
