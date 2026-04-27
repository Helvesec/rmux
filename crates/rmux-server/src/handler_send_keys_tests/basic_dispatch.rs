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
async fn bind_key_and_list_keys_round_trip_through_the_handler() {
    let handler = RequestHandler::new();

    let bound = handler
        .handle(Request::BindKey(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "C-a".to_owned(),
            note: Some("test note".to_owned()),
            repeat: true,
            command: Some(vec!["display-message".to_owned(), "hello".to_owned()]),
        }))
        .await;
    assert!(matches!(bound, Response::BindKey(_)));

    let listed = handler
        .handle(Request::ListKeys(ListKeysRequest {
            table_name: Some("root".to_owned()),
            first_only: false,
            notes: false,
            include_unnoted: true,
            reversed: false,
            format: None,
            sort_order: None,
            prefix: None,
            key: Some("C-a".to_owned()),
        }))
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
        .handle(Request::BindKey(BindKeyRequest {
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
        }))
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
        .handle(Request::BindKey(BindKeyRequest {
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
        }))
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

    let bound = handler
        .handle(Request::BindKey(BindKeyRequest {
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
        }))
        .await;
    assert!(matches!(bound, Response::BindKey(_)));

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

    let dispatched = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
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
        }))
        .await;
    assert_eq!(
        dispatched,
        Response::SendKeys(SendKeysResponse { key_count: 2 })
    );

    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("copy-mode-hit".to_owned()),
        }))
        .await;
    let Response::ShowBuffer(response) = shown else {
        panic!("expected show-buffer response");
    };
    assert_eq!(response.command_output().stdout(), b"ok");

    let listed = handler
        .handle(Request::ListPanes(ListPanesRequest {
            target: alpha,
            format: Some("#{pane_in_mode}".to_owned()),
            target_window_index: None,
        }))
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
        Request::BindKey(BindKeyRequest {
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
        }),
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
        Request::ListPanes(ListPanesRequest {
            target: alpha,
            format: Some("#{pane_in_mode}".to_owned()),
            target_window_index: None,
        }),
    )
    .await;
    let Response::ListPanes(response) = listed else {
        panic!("expected list-panes response");
    };
    assert_eq!(response.command_output().stdout(), b"0\n");
}
