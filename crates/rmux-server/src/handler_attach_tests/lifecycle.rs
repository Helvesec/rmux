use super::*;

#[tokio::test]
async fn attached_remain_on_exit_strips_the_submitted_exit_line_from_dead_pane_capture() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Pane(target.clone()),
                option: OptionName::RemainOnExit,
                value: "on".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SendKeys(SendKeysRequest {
                target: target.clone(),
                keys: vec!["export PS1='PROMPT> '".to_owned(), "Enter".to_owned()],
            }))
            .await,
        Response::SendKeys(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SendKeys(SendKeysRequest {
                target: target.clone(),
                keys: vec!["clear".to_owned(), "Enter".to_owned()],
            }))
            .await,
        Response::SendKeys(_)
    ));
    sleep(Duration::from_millis(150)).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"exit\r")
        .await
        .expect("attached exit input");
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;
    sleep(Duration::from_millis(150)).await;

    let capture = capture_pane_print(&handler, target).await;
    assert!(
        !capture.contains("PROMPT> exit"),
        "attached remain-on-exit capture must not keep the submitted exit line, got {capture:?}"
    );
    if default_shell_window_name() == "bash" {
        assert!(
            capture.contains("logout"),
            "dead pane capture should preserve bash post-exit output, got {capture:?}"
        );
    }
    assert!(
        capture.contains("Pane is dead"),
        "dead pane capture should include remain-on-exit status, got {capture:?}"
    );
}

#[tokio::test]
async fn attached_display_message_print_reports_client_size_and_cursor_position() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);

    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 23 },
        b"PROMPT> ",
    )
    .await;

    let response = handler
        .handle(Request::DisplayMessage(rmux_proto::DisplayMessageRequest {
            target: None,
            print: true,
            message: Some(
                "#{client_width}x#{client_height}|#{cursor_x}|#{cursor_y}|#{session_width}x#{session_height}|#{pane_width}x#{pane_height}"
                    .to_owned(),
            ),
            }))
        .await;
    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    assert_eq!(output.stdout(), b"80x24|8|0|80x23|80x23\n");
}

#[tokio::test]
async fn attached_exit_on_last_pane_closes_the_session_and_client() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);

    assert!(matches!(
        handler
            .handle(Request::SendKeys(SendKeysRequest {
                target: target.clone(),
                keys: vec!["export PS1='PROMPT> '".to_owned(), "Enter".to_owned()],
            }))
            .await,
        Response::SendKeys(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SendKeys(SendKeysRequest {
                target,
                keys: vec!["clear".to_owned(), "Enter".to_owned()],
            }))
            .await,
        Response::SendKeys(_)
    ));
    sleep(Duration::from_millis(150)).await;
    drain_attach_controls(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"exit\r")
        .await
        .expect("attached exit input");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match control_rx.recv().await {
                Some(AttachControl::Exited) => break,
                Some(_) => {}
                None => panic!("attach control channel closed before exit notification"),
            }
        }
    })
    .await
    .expect("timed out waiting for attach exit notification");
    wait_for_session_removed(&handler, &alpha).await;
}

#[tokio::test]
async fn attached_keystroke_stub_returns_key_dispatched_ack() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session_name("alpha"), control_tx)
        .await;

    let response = handler
        .handle_attached_keystroke(
            requester_pid,
            &AttachedKeystroke::new(b"\x1b[A".to_vec()),
            true,
        )
        .await
        .expect("typed keystroke should reach test_double handler");

    assert_eq!(response, KeyDispatched::new(3));
}

#[tokio::test]
async fn attached_keystroke_forwarded_ack_reports_not_consumed() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session_name("alpha"), control_tx)
        .await;

    let response = handler
        .handle_attached_keystroke(requester_pid, &AttachedKeystroke::new(b"a".to_vec()), false)
        .await
        .expect("forwarded keystroke should acknowledge");

    assert_eq!(response, KeyDispatched::forwarded(1));
    assert!(!response.consumed());
}

#[tokio::test]
async fn attached_prefix_key_activates_prefix_table() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02")
        .await
        .expect("prefix key input");

    let active_attach = handler.active_attach.lock().await;
    assert_eq!(
        active_attach
            .by_pid
            .get(&requester_pid)
            .and_then(|active| active.key_table_name.as_deref()),
        Some("prefix")
    );
}

#[tokio::test]
async fn attached_prefix_prefix_dispatches_send_prefix_once_and_returns_to_root() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    assert!(matches!(
        handler
            .handle(Request::SendKeys(SendKeysRequest {
                target: target.clone(),
                keys: vec!["cat -v".to_owned(), "Enter".to_owned()],
            }))
            .await,
        Response::SendKeys(_)
    ));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02\x02x")
        .await
        .expect("prefix send-prefix input");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let capture = capture_pane_print(&handler, target).await;
    assert_eq!(
        capture.matches("^B").count(),
        1,
        "send-prefix should emit exactly one default prefix byte, got {capture:?}"
    );
    assert!(
        capture.contains("^Bx"),
        "literal input after send-prefix should return to root flow, got {capture:?}"
    );
    let active_attach = handler.active_attach.lock().await;
    assert_eq!(
        active_attach
            .by_pid
            .get(&requester_pid)
            .and_then(|active| active.key_table_name.as_deref()),
        None
    );
}

#[tokio::test]
async fn attached_send_prefix_emits_the_configured_prefix_byte() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                option: OptionName::Prefix,
                value: "C-a".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SendKeys(SendKeysRequest {
                target: target.clone(),
                keys: vec!["cat -v".to_owned(), "Enter".to_owned()],
            }))
            .await,
        Response::SendKeys(_)
    ));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x01\x02")
        .await
        .expect("configured prefix send-prefix input");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let capture = capture_pane_print(&handler, target).await;
    assert_eq!(
        capture.matches("^A").count(),
        1,
        "send-prefix should emit exactly one configured prefix byte, got {capture:?}"
    );
    assert!(
        !capture.contains("^B"),
        "send-prefix must not hard-code the default prefix byte, got {capture:?}"
    );
}

#[tokio::test]
async fn attached_live_input_preserves_split_utf8_sequences() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    assert!(matches!(
        handler
            .handle(Request::SendKeys(SendKeysRequest {
                target: target.clone(),
                keys: vec!["export PS1='PROMPT> '".to_owned(), "Enter".to_owned()],
            }))
            .await,
        Response::SendKeys(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SendKeys(SendKeysRequest {
                target: target.clone(),
                keys: vec!["clear".to_owned(), "Enter".to_owned()],
            }))
            .await,
        Response::SendKeys(_)
    ));
    sleep(Duration::from_millis(100)).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"printf 'cafe \xe6")
        .await
        .expect("first utf-8 fragment");
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x96")
        .await
        .expect("second utf-8 fragment");
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x87\\n'\r")
        .await
        .expect("final utf-8 fragment");
    sleep(Duration::from_millis(100)).await;

    let capture = capture_pane_print(&handler, target).await;
    assert!(
        capture.contains("PROMPT> printf 'cafe 文\\n'"),
        "attached input must preserve the split utf-8 command text, got {capture:?}"
    );
    assert!(
        capture.contains("cafe 文"),
        "attached input must preserve the split utf-8 output, got {capture:?}"
    );
}
