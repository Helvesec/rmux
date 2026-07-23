use super::*;

#[tokio::test]
async fn send_keys_writes_resolved_bytes_to_the_correct_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            keys: vec!["hello".to_owned(), "Enter".to_owned()],
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 2 })
    );
}

#[tokio::test]
async fn send_keys_uses_configured_backspace_byte() {
    let handler = RequestHandler::new();
    let alpha = session_name("send-keys-backspace-option");
    create_send_keys_test_session(&handler, &alpha).await;

    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Backspace,
            value: "C-h".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");

    let capture = RawPaneInputProbe::start(&handler, &alpha, "backspace-option", 3).await;
    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            keys: vec!["BSpace".to_owned(), "M-BSpace".to_owned()],
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 2 })
    );

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"\x08\x1b\x08").await;
}

#[tokio::test]
async fn send_keys_marks_attached_session_input_as_interactive() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler.register_attach(77, alpha.clone(), control_tx).await;

    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(alpha, 0),
            keys: vec!["hello".to_owned()],
        }))
        .await;

    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(control) = control_rx.recv().await {
            if matches!(control, crate::pane_io::AttachControl::InteractiveInput) {
                return;
            }
        }
        panic!("attach control channel should remain open");
    })
    .await
    .expect("interactive input control should arrive");
}

#[tokio::test]
async fn pane_input_ref_marks_attached_session_input_as_interactive() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_send_keys_test_session(&handler, &alpha).await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler.register_attach(77, alpha.clone(), control_tx).await;

    let response = handler
        .handle(Request::PaneInput(rmux_proto::PaneInputRequest {
            target: PaneTargetRef::slot(PaneTarget::new(alpha, 0)),
            keys: vec!["hello".to_owned()],
            literal: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(control) = control_rx.recv().await {
            if matches!(control, crate::pane_io::AttachControl::InteractiveInput) {
                return;
            }
        }
        panic!("attach control channel should remain open");
    })
    .await
    .expect("interactive input control should arrive");
}

#[tokio::test]
async fn send_keys_plain_input_uses_copy_mode_until_copy_mode_exits() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);

    create_send_keys_test_session(&handler, &alpha).await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "send-keys-copy-mode", 1).await;

    let entered = handler
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
        .await;
    assert!(matches!(entered, Response::CopyMode(_)));

    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: target.clone(),
            keys: vec!["q".to_owned(), "X".to_owned()],
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 2 })
    );

    let listed = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: alpha,
            format: Some("#{pane_in_mode}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })))
        .await;
    let Response::ListPanes(response) = listed else {
        panic!("expected list-panes response");
    };
    assert_eq!(response.command_output().stdout(), b"0\n");
    capture.assert_contents(&handler, b"X").await;
}

#[tokio::test]
async fn send_keys_with_empty_keys_returns_zero_count() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(alpha, 0),
            keys: vec![],
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 0 })
    );
}

#[tokio::test]
async fn send_keys_control_question_and_noop_digits_match_tmux_bytes() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    create_send_keys_test_session(&handler, &alpha).await;

    let capture = RawPaneInputProbe::start(&handler, &alpha, "send-keys-control-bytes", 1).await;
    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            keys: vec![
                "C-?".to_owned(),
                "C-3".to_owned(),
                "C-4".to_owned(),
                "C-5".to_owned(),
                "C-7".to_owned(),
                "C-8".to_owned(),
            ],
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 6 })
    );
    capture.assert_contents(&handler, &[0x7f]).await;
}

#[cfg(windows)]
#[tokio::test]
async fn pane_input_ctrl_z_uses_windows_console_key_mapping() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    create_send_keys_test_session(&handler, &alpha).await;

    let capture = RawPaneInputProbe::start(&handler, &alpha, "pane-input-c-z", 1).await;
    let response = handler
        .handle(Request::PaneInput(rmux_proto::PaneInputRequest {
            target: PaneTargetRef::slot(PaneTarget::new(alpha.clone(), 0)),
            keys: vec!["C-z".to_owned()],
            literal: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &[0x1a]).await;
}

#[cfg(windows)]
#[tokio::test]
async fn pane_input_ctrl_c_inside_multi_token_payload_uses_windows_console_sequence() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    create_send_keys_test_session(&handler, &alpha).await;

    let capture = RawPaneInputProbe::start(&handler, &alpha, "pane-input-multi-token-c-c", 1).await;
    let response = handler
        .handle(Request::PaneInput(rmux_proto::PaneInputRequest {
            target: PaneTargetRef::slot(PaneTarget::new(alpha.clone(), 0)),
            keys: vec![
                "Write-Output BEFORE".to_owned(),
                "Enter".to_owned(),
                "C-c".to_owned(),
                "Enter".to_owned(),
            ],
            literal: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 4 })
    );

    let mut expected = b"Write-Output BEFORE".to_vec();
    expected.extend_from_slice(&encoded_windows_key_bytes("Enter"));
    expected.push(0x03);
    expected.extend_from_slice(&encoded_windows_key_bytes("Enter"));
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;
}

#[cfg(windows)]
#[tokio::test]
async fn pane_input_ctrl_a_to_cmd_uses_select_all_sequence() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    set_windows_test_shell(&handler, "cmd.exe").await;
    create_send_keys_test_session(&handler, &alpha).await;

    let capture = RawPaneInputProbe::start(&handler, &alpha, "pane-input-cmd-c-a", 1).await;
    let response = handler
        .handle(Request::PaneInput(rmux_proto::PaneInputRequest {
            target: PaneTargetRef::slot(PaneTarget::new(alpha.clone(), 0)),
            keys: vec!["C-a".to_owned()],
            literal: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );

    capture
        .assert_contents(&handler, &windows_cmd_select_all_bytes())
        .await;
}

#[cfg(windows)]
#[tokio::test]
async fn send_keys_ctrl_a_to_cmd_uses_select_all_sequence() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);

    set_windows_test_shell(&handler, "cmd.exe").await;
    create_send_keys_test_session(&handler, &alpha).await;

    let capture = RawPaneInputProbe::start(&handler, &alpha, "send-keys-cmd-c-a", 1).await;
    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target,
            keys: vec!["C-a".to_owned()],
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );

    capture
        .assert_contents(&handler, &windows_cmd_select_all_bytes())
        .await;
}

#[cfg(windows)]
#[tokio::test]
async fn send_keys_ext_ctrl_a_to_cmd_uses_select_all_sequence() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);

    set_windows_test_shell(&handler, "cmd.exe").await;
    create_send_keys_test_session(&handler, &alpha).await;

    let capture = RawPaneInputProbe::start(&handler, &alpha, "send-keys-ext-cmd-c-a", 1).await;
    let response = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(target),
            keys: vec!["C-a".to_owned()],
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

    capture
        .assert_contents(&handler, &windows_cmd_select_all_bytes())
        .await;
}

#[cfg(windows)]
#[tokio::test]
async fn pane_input_literal_ctrl_c_bytes_remain_literal() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    create_send_keys_test_session(&handler, &alpha).await;

    let capture = RawPaneInputProbe::start(&handler, &alpha, "pane-input-literal-c-c", 1).await;
    let response = handler
        .handle(Request::PaneInput(rmux_proto::PaneInputRequest {
            target: PaneTargetRef::slot(PaneTarget::new(alpha.clone(), 0)),
            keys: vec!["\u{3}".to_owned()],
            literal: true,
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &[0x03]).await;
}

#[tokio::test]
async fn send_keys_reset_terminal_updates_transcript_without_writing_to_child() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&target);
        let transcript = state
            .transcript_handle(&target)
            .expect("test pane transcript must exist");
        let mut transcript = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned");
        transcript.append_bytes(b"reset-marker");
    }

    let response = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(target.clone()),
            keys: vec![],
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: false,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: true,
            repeat_count: None,
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 0 })
    );

    let state = handler.state.lock().await;
    assert_eq!(state.pane_input_capture_for_test(&target), Some(Vec::new()));
    let captured = state
        .capture_transcript(
            &target,
            crate::pane_terminals::PaneCaptureRequest {
                range: Default::default(),
                options: Default::default(),
                alternate: false,
                use_mode_screen: false,
                pending_input: false,
                quiet: false,
                escape_pending: false,
            },
        )
        .expect("capture after reset must succeed");
    assert!(
        !String::from_utf8_lossy(&captured).contains("reset-marker"),
        "terminal reset should clear visible transcript contents"
    );
}

#[tokio::test]
async fn send_keys_to_missing_session_returns_session_not_found() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(session_name("missing"), 0),
            keys: vec!["hello".to_owned()],
        }))
        .await;
    assert_eq!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::SessionNotFound("missing".to_owned()),
        })
    );
}

#[tokio::test]
async fn send_keys_empty_keys_to_missing_session_returns_error() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(session_name("missing"), 0),
            keys: vec![],
        }))
        .await;
    assert!(matches!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::SessionNotFound(_),
        })
    ));
}

#[cfg(windows)]
async fn set_windows_test_shell(handler: &RequestHandler, shell: &str) {
    let mut state = handler.state.lock().await;
    state
        .options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            shell.to_owned(),
            SetOptionMode::Replace,
        )
        .expect("test default-shell is valid");
}

#[cfg(windows)]
fn windows_cmd_select_all_bytes() -> Vec<u8> {
    ["C-Home", "S-End"]
        .into_iter()
        .flat_map(encoded_windows_key_bytes)
        .collect()
}

#[cfg(windows)]
fn encoded_windows_key_bytes(key_name: &str) -> Vec<u8> {
    let key = key_string_lookup_string(key_name).expect("test key must exist");
    encode_key(0, ExtendedKeyFormat::Xterm, key).expect("test key must encode")
}

#[tokio::test]
async fn send_keys_to_missing_pane_returns_error() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(alpha, 9),
            keys: vec!["hello".to_owned()],
        }))
        .await;
    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn pane_broadcast_input_reports_per_target_successes_and_failures() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let missing_target = PaneTargetRef::by_id(alpha.clone(), PaneId::new(999));
    let response = handler
        .handle(Request::PaneBroadcastInput(PaneBroadcastInputRequest {
            targets: vec![
                PaneTargetRef::slot(PaneTarget::new(alpha.clone(), 0)),
                missing_target.clone(),
            ],
            keys: vec!["hello".to_owned()],
            literal: true,
        }))
        .await;

    let Response::PaneBroadcastInput(response) = response else {
        panic!("expected pane broadcast response, got {response:?}");
    };
    assert_eq!(response.key_count, 1);
    assert_eq!(response.successes.len(), 1);
    assert_eq!(response.successes[0].target_index, 0);
    assert_eq!(
        response.successes[0].target,
        PaneTarget::new(alpha.clone(), 0)
    );
    assert_eq!(response.failures.len(), 1);
    assert_eq!(response.failures[0].target_index, 1);
    assert_eq!(response.failures[0].target, missing_target);
    assert!(matches!(
        response.failures[0].error,
        RmuxError::PaneNotFound {
            ref session_name,
            pane_id,
        } if session_name == &alpha && pane_id == PaneId::new(999)
    ));
}

#[tokio::test]
async fn bind_key_and_list_keys_round_trip_through_the_handler() {
    let handler = RequestHandler::new();

    let bound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "C-a".to_owned(),
            note: Some("test note".to_owned()),
            repeat: true,
            command: Some(vec!["display-message".to_owned(), "hello".to_owned()]),
        })))
        .await;
    assert!(matches!(bound, Response::BindKey(_)));

    let listed = handler
        .handle(Request::ListKeys(Box::new(ListKeysRequest {
            table_name: Some("root".to_owned()),
            first_only: false,
            notes: false,
            include_unnoted: true,
            reversed: false,
            format: None,
            sort_order: None,
            prefix: None,
            key: None,
        })))
        .await;

    let Response::ListKeys(response) = listed else {
        panic!("expected list-keys response");
    };
    let stdout = String::from_utf8(response.command_output().stdout().to_vec()).unwrap();
    assert!(stdout.contains("bind-key -r -T root"));
    assert!(stdout.contains("C-a"));
}

#[tokio::test]
async fn send_keys_k_dispatches_prefix_table_bindings() {
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
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let bound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "x".to_owned(),
            note: Some("prefix-hit".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "prefix-hit".to_owned(),
                "yes".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(bound, Response::BindKey(_)));

    let dispatched = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(PaneTarget::new(alpha.clone(), 0)),
            keys: vec!["C-b".to_owned(), "x".to_owned()],
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: true,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await;
    assert_eq!(
        dispatched,
        Response::SendKeys(SendKeysResponse { key_count: 2 })
    );

    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("prefix-hit".to_owned()),
        }))
        .await;
    let Response::ShowBuffer(response) = shown else {
        panic!("expected show-buffer response");
    };
    assert_eq!(response.command_output().stdout(), b"yes");
}

#[tokio::test]
async fn send_keys_k_binding_task_preserves_disabled_hook_context() {
    let handler = RequestHandler::new();
    let alpha = session_name("hook-binding-alpha");
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
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let hook = handler
        .handle(Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Global,
            hook: HookName::AfterNewWindow,
            command: "set-buffer -b nested-hook fired".to_owned(),
            lifecycle: HookLifecycle::Persistent,
        }))
        .await;
    assert!(matches!(hook, Response::SetHook(_)));
    let bound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "x".to_owned(),
            note: Some("create-with-hooks-disabled".to_owned()),
            repeat: false,
            command: Some(vec!["new-window".to_owned(), "-d".to_owned()]),
        })))
        .await;
    assert!(matches!(bound, Response::BindKey(_)));

    let dispatched = crate::hook_runtime::with_hook_execution(
        crate::hook_runtime::HookExecutionContext::command(HookName::AfterNewWindow),
        Vec::new(),
        async {
            handler
                .handle(Request::SendKeysExt(SendKeysExtRequest {
                    target: Some(PaneTarget::new(alpha.clone(), 0)),
                    keys: vec!["C-b".to_owned(), "x".to_owned()],
                    expand_formats: false,
                    hex: false,
                    literal: false,
                    dispatch_key_table: true,
                    copy_mode_command: false,
                    forward_mouse_event: false,
                    reset_terminal: false,
                    repeat_count: None,
                }))
                .await
        },
    )
    .await;
    assert_eq!(
        dispatched,
        Response::SendKeys(SendKeysResponse { key_count: 2 })
    );

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("session survives")
            .windows()
            .len(),
        2,
        "the attached binding still creates its requested window"
    );
    drop(state);
    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("nested-hook".to_owned()),
        }))
        .await;
    assert!(
        matches!(shown, Response::Error(_)),
        "hook command must stay disabled across the Tokio task boundary"
    );
}

#[tokio::test]
async fn switch_client_t_sets_custom_key_table_for_next_k_dispatch() {
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
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let bound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "my-table".to_owned(),
            key: "j".to_owned(),
            note: Some("custom".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "custom-hit".to_owned(),
                "ok".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(bound, Response::BindKey(_)));

    let switched = handler
        .handle(Request::SwitchClientExt(SwitchClientExtRequest {
            target: None,
            key_table: Some("my-table".to_owned()),
        }))
        .await;
    assert!(matches!(switched, Response::SwitchClient(_)));

    let dispatched = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(PaneTarget::new(alpha, 0)),
            keys: vec!["j".to_owned()],
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: true,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await;
    assert_eq!(
        dispatched,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );

    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("custom-hit".to_owned()),
        }))
        .await;
    let Response::ShowBuffer(response) = shown else {
        panic!("expected show-buffer response");
    };
    assert_eq!(response.command_output().stdout(), b"ok");
}

#[tokio::test]
async fn send_keys_k_prefix_precedes_a_transient_key_table() {
    let handler = RequestHandler::new();
    let alpha = session_name("prefix-precedes-transient-k");
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
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let bound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "my-table".to_owned(),
            key: "C-b".to_owned(),
            note: Some("must lose to the prefix key".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "wrong-table-hit".to_owned(),
                "yes".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(bound, Response::BindKey(_)));

    let switched = handler
        .handle(Request::SwitchClientExt(SwitchClientExtRequest {
            target: None,
            key_table: Some("my-table".to_owned()),
        }))
        .await;
    assert!(matches!(switched, Response::SwitchClient(_)));

    let dispatched = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(PaneTarget::new(alpha, 0)),
            keys: vec!["C-b".to_owned()],
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: true,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await;
    assert_eq!(
        dispatched,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );

    // tmux 3.7b reports `prefix|1` here and does not execute a C-b binding
    // from the transient table: the configured prefix key takes precedence.
    let clients = handler
        .handle(Request::ListClients(Box::new(
            rmux_proto::ListClientsRequest {
                format: Some("#{client_key_table}|#{client_prefix}".to_owned()),
                target_session: None,
                filter: None,
                sort_order: None,
                reversed: false,
            },
        )))
        .await;
    let Response::ListClients(clients) = clients else {
        panic!("expected list-clients response");
    };
    assert_eq!(clients.output.stdout(), b"prefix|1\n");

    let wrong_table_hit = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("wrong-table-hit".to_owned()),
        }))
        .await;
    assert!(
        matches!(wrong_table_hit, Response::Error(_)),
        "the transient table's prefix-key binding must not execute"
    );
}

#[tokio::test]
async fn send_keys_k_uses_copy_mode_bindings_until_copy_mode_exits() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let target = PaneTarget::new(alpha.clone(), 0);

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
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let bound = handle_boxed(
        &handler,
        Request::BindKey(Box::new(BindKeyRequest {
            table_name: "copy-mode".to_owned(),
            key: "j".to_owned(),
            note: Some("copy-mode-hit".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "copy-mode-hit".to_owned(),
                "ok".to_owned(),
            ]),
        })),
    )
    .await;
    assert!(matches!(bound, Response::BindKey(_)));

    let entered = handle_boxed(
        &handler,
        Request::CopyMode(CopyModeRequest {
            target: Some(target.clone()),
            page_down: false,
            exit_on_scroll: false,
            hide_position: false,
            mouse_drag_start: false,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: None,
            page_up: false,
        }),
    )
    .await;
    assert!(matches!(entered, Response::CopyMode(_)));

    let dispatched = handle_boxed(
        &handler,
        Request::SendKeysExt(SendKeysExtRequest {
            target: Some(target),
            keys: vec!["j".to_owned(), "q".to_owned()],
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: true,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }),
    )
    .await;
    assert_eq!(
        dispatched,
        Response::SendKeys(SendKeysResponse { key_count: 2 })
    );

    let shown = handle_boxed(
        &handler,
        Request::ShowBuffer(ShowBufferRequest {
            name: Some("copy-mode-hit".to_owned()),
        }),
    )
    .await;
    let Response::ShowBuffer(response) = shown else {
        panic!("expected show-buffer response");
    };
    assert_eq!(response.command_output().stdout(), b"ok");

    let listed = handle_boxed(
        &handler,
        Request::ListPanes(Box::new(ListPanesRequest {
            target: alpha,
            format: Some("#{pane_in_mode}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })),
    )
    .await;
    let Response::ListPanes(response) = listed else {
        panic!("expected list-panes response");
    };
    assert_eq!(response.command_output().stdout(), b"0\n");
}

#[tokio::test]
async fn send_keys_k_uses_copy_mode_vi_bindings_when_mode_keys_is_vi() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let target = PaneTarget::new(alpha.clone(), 0);

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
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let configured = handle_boxed(
        &handler,
        Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::new(alpha.clone())),
            option: OptionName::ModeKeys,
            value: "vi".to_owned(),
            mode: SetOptionMode::Replace,
        }),
    )
    .await;
    assert!(matches!(configured, Response::SetOption(_)));

    let bound = handle_boxed(
        &handler,
        Request::BindKey(Box::new(BindKeyRequest {
            table_name: "copy-mode-vi".to_owned(),
            key: "v".to_owned(),
            note: Some("copy-mode-vi-hit".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "copy-mode-vi-hit".to_owned(),
                "ok".to_owned(),
            ]),
        })),
    )
    .await;
    assert!(matches!(bound, Response::BindKey(_)));

    let entered = handle_boxed(
        &handler,
        Request::CopyMode(CopyModeRequest {
            target: Some(target.clone()),
            page_down: false,
            exit_on_scroll: false,
            hide_position: false,
            mouse_drag_start: false,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: None,
            page_up: false,
        }),
    )
    .await;
    assert!(matches!(entered, Response::CopyMode(_)));

    let dispatched = handle_boxed(
        &handler,
        Request::SendKeysExt(SendKeysExtRequest {
            target: Some(target),
            keys: vec!["v".to_owned(), "q".to_owned()],
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: true,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }),
    )
    .await;
    assert_eq!(
        dispatched,
        Response::SendKeys(SendKeysResponse { key_count: 2 })
    );

    let shown = handle_boxed(
        &handler,
        Request::ShowBuffer(ShowBufferRequest {
            name: Some("copy-mode-vi-hit".to_owned()),
        }),
    )
    .await;
    let Response::ShowBuffer(response) = shown else {
        panic!("expected show-buffer response");
    };
    assert_eq!(response.command_output().stdout(), b"ok");

    let listed = handle_boxed(
        &handler,
        Request::ListPanes(Box::new(ListPanesRequest {
            target: alpha,
            format: Some("#{pane_in_mode}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })),
    )
    .await;
    let Response::ListPanes(response) = listed else {
        panic!("expected list-panes response");
    };
    assert_eq!(response.command_output().stdout(), b"0\n");
}
