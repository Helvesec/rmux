use super::*;

#[tokio::test]
async fn send_prefix_reports_the_configured_prefix_key() {
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
        .handle(Request::SendPrefix(SendPrefixRequest {
            target: Some(PaneTarget::new(alpha, 0)),
            secondary: false,
        }))
        .await;
    assert!(matches!(
        response,
        Response::SendPrefix(ref success) if success.key == "C-b"
    ));
}

#[tokio::test]
async fn bind_key_without_a_command_requires_an_existing_binding() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "User1000".to_owned(),
            note: Some("missing".to_owned()),
            repeat: true,
            command: None,
        })))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn bind_key_without_a_command_updates_note_and_repeat_in_place() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "C-b".to_owned(),
            note: Some("updated note".to_owned()),
            repeat: true,
            command: None,
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)));

    let listed = handler
        .handle(Request::ListKeys(Box::new(ListKeysRequest {
            table_name: Some("prefix".to_owned()),
            first_only: false,
            notes: false,
            include_unnoted: true,
            reversed: false,
            format: Some("#{key_note}|#{key_repeat}|#{key_command}".to_owned()),
            sort_order: None,
            prefix: None,
            key: None,
        })))
        .await;
    let Response::ListKeys(response) = listed else {
        panic!("expected list-keys response");
    };
    let stdout = String::from_utf8(response.command_output().stdout().to_vec()).unwrap();
    assert!(
        stdout
            .lines()
            .any(|line| line == "updated note|1|send-prefix"),
        "{stdout:?}"
    );
}

#[tokio::test]
async fn list_keys_notes_render_effective_prefix_column() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Prefix,
            value: "C-a".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)));

    for request in [
        BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "X".to_owned(),
            note: Some("note text".to_owned()),
            repeat: false,
            command: Some(vec!["display-message".to_owned(), "hi".to_owned()]),
        },
        BindKeyRequest {
            table_name: "root".to_owned(),
            key: "F12".to_owned(),
            note: Some("root note".to_owned()),
            repeat: false,
            command: Some(vec!["display-message".to_owned(), "root".to_owned()]),
        },
    ] {
        let response = handler.handle(Request::BindKey(Box::new(request))).await;
        assert!(matches!(response, Response::BindKey(_)));
    }

    let listed = handler
        .handle(Request::ListKeys(Box::new(ListKeysRequest {
            table_name: None,
            first_only: false,
            notes: true,
            include_unnoted: false,
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
    assert!(stdout.contains("C-a X       note text\n"), "{stdout:?}");
    assert!(stdout.contains("    F12     root note\n"), "{stdout:?}");

    let listed = handler
        .handle(Request::ListKeys(Box::new(ListKeysRequest {
            table_name: None,
            first_only: false,
            notes: true,
            include_unnoted: false,
            reversed: false,
            format: None,
            sort_order: None,
            prefix: Some("PFX".to_owned()),
            key: None,
        })))
        .await;
    let Response::ListKeys(response) = listed else {
        panic!("expected list-keys response");
    };
    let stdout = String::from_utf8(response.command_output().stdout().to_vec()).unwrap();
    assert!(stdout.contains("PFXX       note text\n"), "{stdout:?}");
    assert!(stdout.contains("   F12     root note\n"), "{stdout:?}");
}

#[tokio::test]
async fn list_keys_single_key_filter_is_silent_for_explicit_table() {
    let handler = RequestHandler::new();

    let listed = handler
        .handle(Request::ListKeys(Box::new(ListKeysRequest {
            table_name: Some("prefix".to_owned()),
            first_only: false,
            notes: false,
            include_unnoted: true,
            reversed: false,
            format: None,
            sort_order: None,
            prefix: None,
            key: Some("C-b".to_owned()),
        })))
        .await;
    let Response::ListKeys(response) = listed else {
        panic!("expected list-keys response");
    };

    let stdout = String::from_utf8(response.command_output().stdout().to_vec()).unwrap();
    assert_eq!(stdout, "");
    assert_eq!(response.match_count, 0);
}

#[tokio::test]
async fn list_keys_single_key_filter_matches_valid_key_across_tables() {
    let handler = RequestHandler::new();

    let listed = handler
        .handle(Request::ListKeys(Box::new(ListKeysRequest {
            table_name: None,
            first_only: false,
            notes: false,
            include_unnoted: true,
            reversed: false,
            format: None,
            sort_order: None,
            prefix: None,
            key: Some("C-b".to_owned()),
        })))
        .await;
    let Response::ListKeys(response) = listed else {
        panic!("expected list-keys response");
    };

    let stdout = String::from_utf8(response.command_output().stdout().to_vec()).unwrap();
    assert!(
        stdout.contains("-T copy-mode    C-b send-keys -X cursor-left"),
        "{stdout:?}"
    );
    assert!(
        stdout.contains("-T copy-mode-vi C-b send-keys -X page-up"),
        "{stdout:?}"
    );
    assert!(
        stdout.contains("-T prefix       C-b send-prefix"),
        "{stdout:?}"
    );
    assert_eq!(response.match_count, 3);
}

#[tokio::test]
async fn list_keys_single_key_filter_errors_when_key_syntax_is_invalid() {
    let handler = RequestHandler::new();

    let listed = handler
        .handle(Request::ListKeys(Box::new(ListKeysRequest {
            table_name: Some("prefix".to_owned()),
            first_only: false,
            notes: false,
            include_unnoted: true,
            reversed: false,
            format: None,
            sort_order: None,
            prefix: None,
            key: Some("NotAKey".to_owned()),
        })))
        .await;

    assert_eq!(
        listed,
        Response::Error(ErrorResponse {
            error: RmuxError::Server("invalid key: NotAKey".to_owned())
        })
    );
}

#[tokio::test]
async fn list_keys_rejects_unknown_sort_orders() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::ListKeys(Box::new(ListKeysRequest {
            table_name: None,
            first_only: false,
            notes: false,
            include_unnoted: true,
            reversed: false,
            format: None,
            sort_order: Some("bogus".to_owned()),
            prefix: None,
            key: None,
        })))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn repeating_non_repeat_lookup_restarts_in_the_default_table() {
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

    for request in [
        BindKeyRequest {
            table_name: "root".to_owned(),
            key: "x".to_owned(),
            note: Some("root".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "dispatch-source".to_owned(),
                "root".to_owned(),
            ]),
        },
        BindKeyRequest {
            table_name: "my-table".to_owned(),
            key: "r".to_owned(),
            note: Some("repeat".to_owned()),
            repeat: true,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "repeat-hit".to_owned(),
                "yes".to_owned(),
            ]),
        },
        BindKeyRequest {
            table_name: "my-table".to_owned(),
            key: "x".to_owned(),
            note: Some("custom".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "dispatch-source".to_owned(),
                "custom".to_owned(),
            ]),
        },
    ] {
        let response = handler.handle(Request::BindKey(Box::new(request))).await;
        assert!(matches!(response, Response::BindKey(_)));
    }

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
            keys: vec!["r".to_owned(), "x".to_owned()],
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
            name: Some("dispatch-source".to_owned()),
        }))
        .await;
    let Response::ShowBuffer(response) = shown else {
        panic!("expected show-buffer response");
    };
    assert_eq!(response.command_output().stdout(), b"root");
}

#[tokio::test]
async fn prefix_timeout_clears_the_prefix_table_without_waiting_for_the_next_key() {
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

    let configured = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::PrefixTimeout,
            value: "25".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(configured, Response::SetOption(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

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

    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client should remain registered");
        assert_eq!(active.key_table_name.as_deref(), Some("prefix"));
    }

    sleep(Duration::from_millis(100)).await;

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client should remain registered");
    assert_eq!(active.key_table_name, None);
    assert_eq!(active.key_table_set_at, None);
    assert!(!active.repeat_active);
    assert_eq!(active.repeat_deadline, None);
}

#[tokio::test]
async fn repeat_timeout_clears_custom_key_tables_without_waiting_for_the_next_key() {
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

    let configured = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha.clone()),
            option: OptionName::RepeatTime,
            value: "25".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(configured, Response::SetOption(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let bound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "my-table".to_owned(),
            key: "r".to_owned(),
            note: Some("repeat".to_owned()),
            repeat: true,
            command: Some(vec![
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "repeat-hit".to_owned(),
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
            keys: vec!["r".to_owned()],
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

    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client should remain registered");
        assert_eq!(active.key_table_name.as_deref(), Some("my-table"));
        assert!(active.repeat_active);
        assert!(active.repeat_deadline.is_some());
    }

    sleep(Duration::from_millis(100)).await;

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client should remain registered");
    assert_eq!(active.key_table_name, None);
    assert_eq!(active.key_table_set_at, None);
    assert!(!active.repeat_active);
    assert_eq!(active.repeat_deadline, None);
}

#[tokio::test]
async fn unbind_key_all_removes_active_bindings_without_dropping_default_tables() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::UnbindKey(UnbindKeyRequest {
            table_name: "prefix".to_owned(),
            all: true,
            key: None,
            quiet: false,
        }))
        .await;
    assert!(matches!(
        response,
        Response::UnbindKey(ref success) if success.removed && success.all
    ));

    let listed = handler
        .handle(Request::ListKeys(Box::new(ListKeysRequest {
            table_name: Some("prefix".to_owned()),
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
    assert_eq!(response.match_count, 0);

    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "User1000".to_owned(),
            note: Some("user".to_owned()),
            repeat: false,
            command: Some(vec!["send-prefix".to_owned()]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));
}
