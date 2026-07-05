use super::*;

#[cfg(unix)]
#[tokio::test]
async fn live_attach_direct_fast_path_requires_current_unsynchronized_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let pane_zero = PaneTarget::with_window(alpha.clone(), 0, 0);
    let pty = rmux_pty::PtyPair::open().expect("open pty pair");
    let mut active_emit_cache = None;

    let direct = handler
        .try_forward_plain_attached_bytes_to_current_pane_fast(
            requester_pid,
            &[],
            b"x",
            &pane_zero,
            pty.master(),
            &mut active_emit_cache,
        )
        .await
        .expect("direct fast path should run");
    assert_eq!(direct, Some(true));

    let direct_batch = handler
        .try_forward_plain_attached_bytes_to_current_pane_fast(
            requester_pid,
            &[],
            b"abc",
            &pane_zero,
            pty.master(),
            &mut active_emit_cache,
        )
        .await
        .expect("direct fast path should accept small printable batches");
    assert_eq!(direct_batch, Some(true));

    let stale_target = PaneTarget::with_window(alpha.clone(), 0, 1);
    let stale = handler
        .try_forward_plain_attached_bytes_to_current_pane_fast(
            requester_pid,
            &[],
            b"x",
            &stale_target,
            pty.master(),
            &mut active_emit_cache,
        )
        .await
        .expect("stale target check should run");
    assert_eq!(stale, None);

    let set_sync = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 0)),
            option: OptionName::SynchronizePanes,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_sync, Response::SetOption(_)));

    let synchronized = handler
        .try_forward_plain_attached_bytes_to_current_pane_fast(
            requester_pid,
            &[],
            b"x",
            &pane_zero,
            pty.master(),
            &mut active_emit_cache,
        )
        .await
        .expect("sync panes check should run");
    assert_eq!(synchronized, None);
}

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
    create_send_keys_test_session(handler, alpha).await;
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
}
