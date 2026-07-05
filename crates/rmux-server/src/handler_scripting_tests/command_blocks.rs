use super::*;

#[test]
fn parsed_list_keys_accepts_attached_sort_order_format_and_reverse() {
    let handler = RequestHandler::new();
    let state = handler.state.blocking_lock();
    let parsed = crate::handler::scripting_support::parse_request_from_parts(
        "list-keys".to_owned(),
        vec![
            "-r".to_owned(),
            "-F#{key_table}".to_owned(),
            "-Okey".to_owned(),
        ],
        None,
        &state.sessions,
        &state.options,
        &TargetFindContext::new(None),
    )
    .expect("list-keys sort flags parse like tmux");

    let Request::ListKeys(request) = parsed else {
        panic!("expected ListKeys request");
    };
    assert!(request.reversed);
    assert_eq!(request.format.as_deref(), Some("#{key_table}"));
    assert_eq!(request.sort_order.as_deref(), Some("key"));
}

#[tokio::test]
async fn parsed_list_panes_accepts_filter_sort_order_and_reverse() {
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
    let state = handler.state.lock().await;
    let parsed = crate::handler::scripting_support::parse_request_from_parts(
        "list-panes".to_owned(),
        vec![
            "-t".to_owned(),
            "alpha:0".to_owned(),
            "-f".to_owned(),
            "#{==:#{pane_index},0}".to_owned(),
            "-O".to_owned(),
            "index".to_owned(),
            "-r".to_owned(),
            "-F".to_owned(),
            "#{pane_index}".to_owned(),
        ],
        None,
        &state.sessions,
        &state.options,
        &TargetFindContext::new(None),
    )
    .expect("list-panes filter/sort flags parse like tmux");

    let Request::ListPanes(request) = parsed else {
        panic!("expected ListPanes request");
    };
    assert_eq!(request.target, session_name("alpha"));
    assert_eq!(request.target_window_index, Some(0));
    assert_eq!(request.filter.as_deref(), Some("#{==:#{pane_index},0}"));
    assert_eq!(request.sort_order.as_deref(), Some("index"));
    assert!(request.reversed);
    assert_eq!(request.format.as_deref(), Some("#{pane_index}"));
}

#[tokio::test]
async fn parsed_list_windows_applies_filter_sort_order_and_reverse() {
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
    handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: None,
            command: None,
            process_command: None,
            detached: false,
            start_directory: None,
            environment: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;

    let filtered = CommandParser::new()
        .parse("list-windows -t alpha -f '#{==:#{window_index},1}' -F '#{window_index}'")
        .expect("list-windows filter parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), filtered)
        .await
        .expect("filtered list-windows executes");
    assert_eq!(output.stdout(), b"1\n");

    let reversed = CommandParser::new()
        .parse("list-windows -t alpha -O index -r -F '#{window_index}'")
        .expect("list-windows sort parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), reversed)
        .await
        .expect("sorted list-windows executes");
    assert_eq!(output.stdout(), b"1\n0\n");
}

#[tokio::test]
async fn parsed_set_option_scope_flags_use_tmux_precedence_and_natural_tables() {
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

    let parsed = CommandParser::new()
        .parse("set-option -s -p @scope server")
        .expect("set-option user scope flags parse");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("set-option user scope executes");
    let shown = CommandParser::new()
        .parse("show-options -gsv @scope")
        .expect("show-options server user parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), shown)
        .await
        .expect("show-options server user executes");
    assert_eq!(output.stdout(), b"server\n");

    let parsed = CommandParser::new()
        .parse("set-option -w -t alpha status off")
        .expect("set-option known option parses");
    {
        let state = handler.state.lock().await;
        let request = crate::handler::scripting_support::parse_request_from_parts(
            "set-option".to_owned(),
            vec![
                "-w".to_owned(),
                "-t".to_owned(),
                "alpha:0".to_owned(),
                "status".to_owned(),
                "off".to_owned(),
            ],
            None,
            &state.sessions,
            &state.options,
            &TargetFindContext::new(None),
        )
        .expect("resolved set-option parses");
        let Request::SetOptionByName(request) = request else {
            panic!("expected SetOptionByName request");
        };
        assert_eq!(
            request.scope,
            OptionScopeSelector::Session(session_name("alpha"))
        );
    }
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("set-option known option executes");
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .options
                .resolve(Some(&session_name("alpha")), OptionName::Status),
            Some("off")
        );
    }
    let shown = CommandParser::new()
        .parse("show-options -v -t alpha status")
        .expect("show-options session status parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), shown)
        .await
        .expect("show-options session status executes");
    assert_eq!(output.stdout(), b"off\n");

    let parsed = CommandParser::new()
        .parse("set-option -wg status on")
        .expect("set-option -wg known option parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("set-option -wg known option executes");
    let shown = CommandParser::new()
        .parse("show-options -gv status")
        .expect("show-options global status parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), shown)
        .await
        .expect("show-options global status executes");
    assert_eq!(output.stdout(), b"on\n");

    let parsed = CommandParser::new()
        .parse("set-option -pg status off")
        .expect("set-option -pg known option parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("set-option -pg known option executes");
    let output = handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            CommandParser::new()
                .parse("show-options -gv status")
                .expect("show-options global status parses"),
        )
        .await
        .expect("show-options global status executes");
    assert_eq!(output.stdout(), b"off\n");
}

#[tokio::test]
async fn parsed_queue_bind_key_accepts_command_blocks() {
    let handler = RequestHandler::new();
    let bind = CommandParser::new()
        .parse("bind-key x { display-message -p -- from-block }")
        .expect("bind-key block parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), bind)
        .await
        .expect("bind-key block executes");

    let list = CommandParser::new()
        .parse("list-keys -T prefix")
        .expect("list-keys parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), list)
        .await
        .expect("list-keys executes");
    let stdout = std::str::from_utf8(output.stdout()).expect("list-keys utf8");

    assert!(
        stdout.contains("display-message -p -- from-block"),
        "{stdout}"
    );
}

#[tokio::test]
async fn parsed_queue_set_hook_accepts_command_blocks() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("set-hook -g after-new-window { display-message -p -- hook-block }")
        .expect("set-hook block parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("set-hook block executes");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .hooks
            .global_command(rmux_proto::HookName::AfterNewWindow),
        Some("display-message -p -- hook-block")
    );
}

#[tokio::test]
async fn parsed_queue_set_hook_resolves_relative_targets_before_block_parse() {
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
        .parse("set-hook -t . after-new-window { display-message -p -- hook-block }")
        .expect("set-hook block parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Pane(PaneTarget::with_window(alpha, 0, 0)))),
        )
        .await
        .expect("set-hook -t . should execute");
}

#[tokio::test]
async fn parsed_queue_set_hook_session_target_uses_hook_natural_window_scope() {
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
        .parse("set-hook -t alpha window-renamed { display-message -p -- renamed }")
        .expect("set-hook parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("set-hook executes");

    let state = handler.state.lock().await;
    assert_eq!(
        state.hooks.session_command(&alpha, HookName::WindowRenamed),
        None
    );
    assert_eq!(
        state.hooks.window_command(
            &WindowTarget::with_window(alpha, 0),
            HookName::WindowRenamed
        ),
        Some("display-message -p -- renamed")
    );
}
