use super::*;

#[tokio::test]
async fn parsed_queue_assignments_apply_before_following_commands() {
    let handler = RequestHandler::new();
    let command = shell_env_or_default_command("FOO", "unset");
    let parsed = CommandParser::new()
        .parse(&format!("FOO=bar ; run-shell {}", command_quote(&command)))
        .expect("commands parse");

    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue succeeds");

    assert_eq!(output.stdout(), b"bar");

    let state = handler.state.lock().await;
    assert_eq!(state.environment.global_value("FOO"), Some("bar"));
}

#[tokio::test]
async fn parsed_queue_lock_client_defaults_to_current_client() {
    let handler = RequestHandler::new();
    let alpha = SessionName::new("alpha").expect("valid session name");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(std::process::id(), alpha, control_tx)
        .await;

    let parsed = CommandParser::new()
        .parse("lock-client")
        .expect("commands parse");

    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue succeeds");

    assert!(output.stdout().is_empty());
}

#[tokio::test]
async fn if_shell_inserted_hidden_assignments_stay_out_of_process_environments() {
    let handler = RequestHandler::new();
    let command = shell_env_or_default_command("SECRET", "unset");
    let parsed = CommandParser::new()
        .parse(&format!(
            "if-shell -F 1 {{ %hidden SECRET=classified }} ; run-shell {}",
            command_quote(&command)
        ))
        .expect("commands parse");

    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue succeeds");

    assert_eq!(output.stdout(), b"unset");

    let state = handler.state.lock().await;
    let entries = state
        .environment
        .show_environment_entries(&ScopeSelector::Global, true, Some("SECRET"))
        .expect("hidden show-environment succeeds");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].value.as_deref(), Some("classified"));
}

#[tokio::test]
async fn queue_error_aborts_later_commands_in_the_same_group_only() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("show-buffer -b missing ; set-buffer -b skipped no\nset-buffer -b kept yes")
        .expect("commands parse");

    let result = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await;

    assert!(result.is_err());
    assert!(matches!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("skipped".to_owned()),
            }))
            .await,
        Response::Error(_)
    ));
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("kept".to_owned()),
            }))
            .await
            .command_output()
            .expect("kept buffer output")
            .stdout(),
        b"yes"
    );
}

#[tokio::test]
async fn if_shell_uses_preparsed_brace_command_lists_at_execution_time() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("if-shell -F 1 { show-buffer -b missing\nset-buffer -b kept yes }")
        .expect("commands parse");

    let result = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await;

    assert!(result.is_err());
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("kept".to_owned()),
            }))
            .await
            .command_output()
            .expect("kept buffer output")
            .stdout(),
        b"yes"
    );
}

#[tokio::test]
async fn if_shell_inserted_brace_errors_do_not_abort_parent_line_tail() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("if-shell -F 1 { show-buffer -b missing } ; set-buffer -b kept yes")
        .expect("commands parse");

    let result = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await;

    assert!(result.is_err());
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("kept".to_owned()),
            }))
            .await
            .command_output()
            .expect("kept buffer output")
            .stdout(),
        b"yes"
    );
}

#[tokio::test]
async fn if_shell_string_mode_newlines_share_one_abort_group() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::IfShell(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "show-buffer -b missing\nset-buffer -b skipped no".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        }))
        .await;

    assert!(matches!(response, Response::Error(_)));
    assert!(matches!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("skipped".to_owned()),
            }))
            .await,
        Response::Error(_)
    ));
}

#[tokio::test]
async fn parsed_queue_resolves_unresolved_window_targets_before_protocol_dispatch() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::NewWindow(NewWindowRequest {
                target: alpha.clone(),
                name: Some("logs".to_owned()),
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                target_window_index: None,
                insert_at_target: false,
            }))
            .await,
        Response::NewWindow(_)
    ));

    let parsed = CommandParser::new()
        .parse("rename-window -t alp:1 renamed")
        .expect("commands parse");

    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue command succeeds");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window_at(1)
            .expect("window exists")
            .name(),
        Some("renamed")
    );
}

#[tokio::test]
async fn parsed_queue_resolves_session_only_new_window_targets_at_protocol_boundary() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    let parsed = CommandParser::new()
        .parse("new-window -t alp -d -n logs")
        .expect("commands parse");

    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue command succeeds");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window_at(1)
            .expect("window exists")
            .name(),
        Some("logs")
    );
}

#[tokio::test]
async fn parsed_queue_uses_current_target_for_new_window_split_and_zoom() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    let current_target = Target::Pane(PaneTarget::with_window(alpha.clone(), 0, 0));

    for command in ["new-window -d -n logs", "split-window -h", "resize-pane -Z"] {
        let parsed = CommandParser::new().parse(command).expect("command parses");
        handler
            .execute_parsed_commands(
                std::process::id(),
                parsed,
                QueueExecutionContext::without_caller_cwd()
                    .with_current_target(Some(current_target.clone())),
            )
            .await
            .unwrap_or_else(|error| {
                panic!("{command} should succeed with current target: {error}")
            });
    }

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session.windows().len(),
        2,
        "new-window and split-window should both apply"
    );
    assert!(
        session.window_at(0).expect("window 0 exists").is_zoomed(),
        "resize-pane -Z should zoom the current pane"
    );
    assert_eq!(
        session.window_at(0).expect("window 0 exists").pane_count(),
        2,
        "split-window should split the current window without -t"
    );
}

#[tokio::test]
async fn parsed_queue_reports_missing_target_client_before_input() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    for command in [
        "send-keys -c 999999 -t alpha:0.0 echo SHOULD_NOT_TYPE",
        "display-message -c 999999 hello",
    ] {
        let parsed = CommandParser::new()
            .parse(command)
            .expect("command parses at queue layer");
        let output = handler
            .execute_parsed_commands_for_test(std::process::id(), parsed)
            .await
            .expect("missing target-client is a tmux-compatible noop");

        assert!(output.stdout().is_empty());
    }

    let parsed = CommandParser::new()
        .parse("display-message -p -c 999999 hello")
        .expect("command parses at queue layer");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("print with missing target-client still succeeds");

    assert_eq!(output.stdout(), b"hello\n");
}

#[tokio::test]
async fn parsed_queue_uses_current_target_for_display_panes_without_t() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = 52_u32;
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let parsed = CommandParser::new()
        .parse("display-panes")
        .expect("command parses");
    handler
        .execute_parsed_commands(
            requester_pid,
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Pane(PaneTarget::with_window(alpha, 0, 0)))),
        )
        .await
        .expect("display-panes should use the current target");

    let _overlay = control_rx.recv().await.expect("display-panes overlay");
}

#[tokio::test]
async fn parsed_queue_display_panes_t_reports_target_client_errors_like_cli() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    for (command, expected) in [
        ("display-panes -t 999999", "can't find client: 999999"),
        ("display-panes -t alpha", "can't find client: alpha"),
        ("display-panes -t alpha:0", "can't find client: alpha:0"),
        ("display-panes -t alpha:", "can't find client: alpha"),
    ] {
        let parsed = CommandParser::new()
            .parse(command)
            .expect("display-panes command parses");
        let error = handler
            .execute_parsed_commands_for_test(std::process::id(), parsed)
            .await
            .expect_err("missing target-client should fail");

        assert_eq!(error, rmux_proto::RmuxError::Message(expected.to_owned()));
    }
}

#[tokio::test]
async fn parsed_queue_uses_current_target_for_kill_pane_without_t() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(alpha.clone(), 0, 0)),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));

    let parsed = CommandParser::new()
        .parse("kill-pane")
        .expect("command parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd().with_current_target(Some(Target::Pane(
                PaneTarget::with_window(alpha.clone(), 0, 1),
            ))),
        )
        .await
        .expect("kill-pane should use the current pane target");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session.window_at(0).expect("window exists").pane_count(),
        1,
        "kill-pane without -t should remove the current pane"
    );
}
