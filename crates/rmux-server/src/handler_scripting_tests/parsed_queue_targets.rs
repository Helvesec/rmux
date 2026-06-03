use super::*;

#[tokio::test]
async fn parsed_queue_uses_current_target_for_rename_window_session_and_last_window() {
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
                name: Some("w1".to_owned()),
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
    assert!(matches!(
        handler
            .handle(Request::SelectWindow(rmux_proto::SelectWindowRequest {
                target: rmux_proto::WindowTarget::with_window(alpha.clone(), 1),
            }))
            .await,
        Response::SelectWindow(_)
    ));

    for command in [
        "rename-window renamed",
        "last-window",
        "rename-session beta",
    ] {
        let parsed = CommandParser::new().parse(command).expect("command parses");
        handler
            .execute_parsed_commands(
                std::process::id(),
                parsed,
                QueueExecutionContext::without_caller_cwd().with_current_target(Some(
                    Target::Pane(PaneTarget::with_window(alpha.clone(), 1, 0)),
                )),
            )
            .await
            .unwrap_or_else(|error| {
                panic!("{command} should succeed with current target: {error}")
            });
    }

    let state = handler.state.lock().await;
    let beta = session_name("beta");
    let session = state
        .sessions
        .session(&beta)
        .expect("renamed session exists");
    assert_eq!(
        session.window_at(1).expect("window 1 exists").name(),
        Some("renamed")
    );
    assert_eq!(
        session.active_window_index(),
        0,
        "last-window should return to window 0"
    );
}

#[tokio::test]
async fn parsed_queue_uses_current_target_for_more_default_targeted_commands() {
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

    let current_pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    let current_window = WindowTarget::with_window(alpha.clone(), 0);
    let context = TargetFindContext::from_target(Target::Pane(current_pane.clone()));
    let state = handler.state.lock().await;

    let cases = [
        (
            "kill-window",
            Vec::new(),
            Request::KillWindow(KillWindowRequest {
                target: current_window.clone(),
                kill_all_others: false,
            }),
        ),
        (
            "rotate-window",
            vec!["-U".to_owned()],
            Request::RotateWindow(RotateWindowRequest {
                target: current_window.clone(),
                direction: RotateWindowDirection::Up,
                restore_zoom: false,
            }),
        ),
        (
            "break-pane",
            vec!["-d".to_owned()],
            Request::BreakPane(BreakPaneRequest {
                source: current_pane.clone(),
                target: None,
                name: None,
                detached: true,
                after: false,
                before: false,
                print_target: false,
                format: None,
            }),
        ),
        (
            "respawn-window",
            vec![
                "-k".to_owned(),
                "--".to_owned(),
                "printf".to_owned(),
                "hello".to_owned(),
            ],
            Request::RespawnWindow(RespawnWindowRequest {
                target: current_window.clone(),
                kill: true,
                environment: None,
                command: Some(vec!["printf".to_owned(), "hello".to_owned()]),
                start_directory: None,
            }),
        ),
        (
            "respawn-pane",
            vec![
                "-k".to_owned(),
                "--".to_owned(),
                "printf".to_owned(),
                "hello".to_owned(),
            ],
            Request::RespawnPane(RespawnPaneRequest {
                target: current_pane.clone(),
                kill: true,
                environment: None,
                command: Some(vec!["printf".to_owned(), "hello".to_owned()]),
                process_command: None,
                start_directory: None,
            }),
        ),
        (
            "swap-pane",
            vec!["-U".to_owned()],
            Request::SwapPane(SwapPaneRequest {
                source: current_pane.clone(),
                target: current_pane.clone(),
                direction: Some(SwapPaneDirection::Up),
                detached: false,
                preserve_zoom: false,
            }),
        ),
    ];

    for (command, arguments, expected) in cases {
        let parsed = crate::handler::scripting_support::parse_request_from_parts(
            command.to_owned(),
            arguments,
            None,
            &state.sessions,
            &context,
        )
        .unwrap_or_else(|error| panic!("{command} should use the current target: {error}"));
        assert_eq!(parsed, expected, "unexpected parsed request for {command}");
    }
}

#[tokio::test]
async fn parsed_queue_resolves_attached_short_target_values_for_select_pane() {
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
        .parse("select-pane -t:.+")
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
        .expect("select-pane binding should resolve");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session
            .window_at(0)
            .expect("window exists")
            .active_pane_index(),
        0,
        "attached short-form -t target should resolve relative pane targets"
    );
}

#[tokio::test]
async fn parsed_queue_resolves_select_pane_mark_against_the_current_pane() {
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
        .parse("select-pane -m")
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
        .expect("select-pane -m should resolve against the current pane");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .marked_pane_target()
            .expect("marked pane target exists")
            .to_string(),
        "alpha:0.1"
    );
}

#[tokio::test]
async fn parsed_queue_uses_marked_pane_as_default_swap_source() {
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

    let (pane_zero_id, pane_one_id) = {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .expect("window exists");
        (
            window.pane(0).expect("pane 0 exists").id(),
            window.pane(1).expect("pane 1 exists").id(),
        )
    };

    let mark = CommandParser::new()
        .parse("select-pane -m")
        .expect("mark command parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            mark,
            QueueExecutionContext::without_caller_cwd().with_current_target(Some(Target::Pane(
                PaneTarget::with_window(alpha.clone(), 0, 1),
            ))),
        )
        .await
        .expect("mark pane 1");

    let swap = CommandParser::new()
        .parse("swap-pane -t alpha:0.0")
        .expect("swap command parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            swap,
            QueueExecutionContext::without_caller_cwd().with_current_target(Some(Target::Pane(
                PaneTarget::with_window(alpha.clone(), 0, 0),
            ))),
        )
        .await
        .expect("swap-pane should use the marked pane as source");

    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&alpha)
        .and_then(|session| session.window_at(0))
        .expect("window exists");
    assert_eq!(window.pane(0).expect("pane 0 exists").id(), pane_one_id);
    assert_eq!(window.pane(1).expect("pane 1 exists").id(), pane_zero_id);
}

#[tokio::test]
async fn parsed_queue_uses_marked_pane_window_as_default_swap_window_source() {
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
                name: Some("marked-window".to_owned()),
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                target_window_index: Some(1),
                insert_at_target: false,
            }))
            .await,
        Response::NewWindow(_)
    ));

    let mark = CommandParser::new()
        .parse("select-pane -m")
        .expect("mark command parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            mark,
            QueueExecutionContext::without_caller_cwd().with_current_target(Some(Target::Pane(
                PaneTarget::with_window(alpha.clone(), 1, 0),
            ))),
        )
        .await
        .expect("mark pane in window 1");

    let swap = CommandParser::new()
        .parse("swap-window -t alpha:0")
        .expect("swap-window command parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            swap,
            QueueExecutionContext::without_caller_cwd().with_current_target(Some(Target::Pane(
                PaneTarget::with_window(alpha.clone(), 0, 0),
            ))),
        )
        .await
        .expect("swap-window should use the marked pane window as source");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session.window_at(0).expect("window 0 exists").name(),
        Some("marked-window")
    );
}

#[tokio::test]
async fn parsed_queue_resolves_explicit_marked_pane_targets() {
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

    let mark = CommandParser::new()
        .parse("select-pane -m")
        .expect("mark command parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            mark,
            QueueExecutionContext::without_caller_cwd().with_current_target(Some(Target::Pane(
                PaneTarget::with_window(alpha.clone(), 0, 1),
            ))),
        )
        .await
        .expect("mark pane 1");

    let command = CommandParser::new()
        .parse("display-message -p -t ~ '#{pane_index}'")
        .expect("display-message command parses");
    let output = handler
        .execute_parsed_commands(
            std::process::id(),
            command,
            QueueExecutionContext::without_caller_cwd().with_current_target(Some(Target::Pane(
                PaneTarget::with_window(alpha.clone(), 0, 0),
            ))),
        )
        .await
        .expect("display-message should resolve marked pane");

    assert_eq!(String::from_utf8_lossy(&output.stdout), "1\n");
}

#[tokio::test]
async fn marked_pane_prefers_original_linked_window_slot() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    for session_name in [&alpha, &beta] {
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session_name.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
    }

    assert!(matches!(
        handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(alpha.clone(), 0),
                target: WindowTarget::with_window(beta.clone(), 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: false,
            }))
            .await,
        Response::LinkWindow(_)
    ));

    let mark = CommandParser::new()
        .parse("select-pane -t alpha:0.0 -m")
        .expect("mark command parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), mark)
        .await
        .expect("mark linked pane through alpha");

    let command = CommandParser::new()
        .parse("display-message -p -t '{marked}' '#{session_name}:#{window_index}.#{pane_index}'")
        .expect("display-message command parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), command)
        .await
        .expect("marked pane should resolve through the marked slot");

    assert_eq!(String::from_utf8_lossy(&output.stdout), "alpha:0.0\n");
}
