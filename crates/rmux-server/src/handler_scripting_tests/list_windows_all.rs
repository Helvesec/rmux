use super::*;

async fn create_window_listing_fixture(handler: &RequestHandler) {
    for (session, size, first_name, second_name) in [
        (
            "beta",
            TerminalSize { cols: 80, rows: 30 },
            "zebra",
            "apple",
        ),
        (
            "alpha",
            TerminalSize {
                cols: 100,
                rows: 20,
            },
            "mango",
            "berry",
        ),
    ] {
        let session = session_name(session);
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(size),
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::NewSession(_)), "{response:?}");
        let response = handler
            .handle(Request::RenameWindow(rmux_proto::RenameWindowRequest {
                target: WindowTarget::with_window(session.clone(), 0),
                name: first_name.to_owned(),
            }))
            .await;
        assert!(
            matches!(response, Response::RenameWindow(_)),
            "{response:?}"
        );
        let response = handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: session,
                name: Some(second_name.to_owned()),
                detached: true,
                environment: None,
                command: None,
                start_directory: None,
                target_window_index: None,
                insert_at_target: false,
                process_command: None,
            })))
            .await;
        assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    }
    handler.wait_for_initial_panes_for_test().await;
}

async fn execute_list_windows_all(handler: &RequestHandler, command: &str) -> String {
    let parsed = CommandParser::new()
        .parse(command)
        .expect("list-windows -a parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("list-windows -a executes");
    String::from_utf8(output.stdout().to_vec()).expect("list-windows output is utf-8")
}

#[tokio::test]
async fn parsed_queue_list_windows_all_formats_filters_and_ignores_target() {
    let handler = RequestHandler::new();
    create_window_listing_fixture(&handler).await;

    let stdout = execute_list_windows_all(
        &handler,
        "list-windows -a -t beta -f '#{==:#{session_name},alpha}' -F '#{session_name}:#{window_index}:#{window_name}'",
    )
    .await;
    assert_eq!(stdout, "alpha:0:mango\nalpha:1:berry\n");
}

#[tokio::test]
async fn parsed_queue_list_windows_all_applies_global_sort_and_reverse() {
    let handler = RequestHandler::new();
    create_window_listing_fixture(&handler).await;

    let by_name = execute_list_windows_all(
        &handler,
        "list-windows -a -O name -F '#{session_name}:#{window_index}:#{window_name}'",
    )
    .await;
    assert_eq!(
        by_name,
        "beta:1:apple\nalpha:1:berry\nalpha:0:mango\nbeta:0:zebra\n"
    );

    let reversed = execute_list_windows_all(
        &handler,
        "list-windows -arOname -F '#{session_name}:#{window_index}:#{window_name}'",
    )
    .await;
    assert_eq!(
        reversed,
        "beta:0:zebra\nalpha:0:mango\nalpha:1:berry\nbeta:1:apple\n"
    );

    let by_size = execute_list_windows_all(
        &handler,
        "list-windows -a -O size -F '#{session_name}:#{window_index}:#{window_width}x#{window_height}'",
    )
    .await;
    assert_eq!(
        by_size,
        "alpha:0:100x20\nalpha:1:100x20\nbeta:0:80x30\nbeta:1:80x30\n"
    );
}

#[tokio::test]
async fn parsed_queue_list_windows_all_uses_tmux_default_format() {
    let handler = RequestHandler::new();
    create_window_listing_fixture(&handler).await;

    let stdout = execute_list_windows_all(&handler, "list-windows -a").await;
    assert!(stdout.contains("alpha:0: mango"), "{stdout:?}");
    assert!(stdout.contains("beta:1: apple"), "{stdout:?}");
    assert!(
        stdout.find("alpha:0:") < stdout.find("beta:0:"),
        "default all-session order should be session then window: {stdout:?}"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn queued_list_windows_all_waits_for_deferred_windows_pane_pids() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse(
            "new-session -d -s deferred-pid-list ; \
             list-windows -a -f '#{pane_pid}' -F '#{pane_pid}'",
        )
        .expect("deferred pane-pid queue parses");

    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("deferred pane-pid queue executes");
    let pane_pid = String::from_utf8(output.stdout().to_vec()).expect("pane pid is utf-8");
    let pane_pid = pane_pid.trim().parse::<u32>().expect("pane pid is numeric");
    assert_ne!(pane_pid, 0);
}

#[tokio::test]
async fn source_file_and_control_queue_share_list_windows_all_execution() {
    let handler = RequestHandler::new();
    create_window_listing_fixture(&handler).await;
    let root = temp_root("list-windows-all");
    fs::create_dir_all(&root).expect("create source root");
    fs::write(
        root.join("all.conf"),
        "list-windows -a -F '#{session_name}:#{window_index}'\n",
    )
    .expect("write source file");

    let response = handler
        .handle(source_file_request(
            vec!["all.conf".to_owned()],
            Some(root.clone()),
        ))
        .await;
    let Response::SourceFile(response) = response else {
        panic!("expected source-file response: {response:?}");
    };
    let source_stdout = String::from_utf8(
        response
            .command_output()
            .expect("source-file carries list output")
            .stdout()
            .to_vec(),
    )
    .expect("source output is utf-8");
    assert_eq!(source_stdout, "alpha:0\nalpha:1\nbeta:0\nbeta:1\n");

    let requester_pid = 49_102;
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: rmux_proto::ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(requester_pid, Some(session_name("alpha")))
        .await
        .expect("control session set succeeds");
    let commands = handler
        .parse_control_commands(
            "list-windows -a -f '#{==:#{session_name},beta}' -F '#{session_name}:#{window_index}'",
        )
        .await
        .expect("control list-windows parses");
    let result = handler
        .execute_control_commands(requester_pid, commands)
        .await;
    assert_eq!(result.error, None, "{:?}", result.error);
    assert_eq!(result.stdout, b"beta:0\nbeta:1\n");

    fs::remove_dir_all(root).expect("remove source root");
}

#[tokio::test]
async fn queued_list_windows_all_runs_its_tmux_after_hook() {
    let handler = RequestHandler::new();
    create_window_listing_fixture(&handler).await;
    let parsed = CommandParser::new()
        .parse("set-hook -g after-list-windows { set-buffer -b list-windows-hook fired }")
        .expect("after-list-windows hook parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("after-list-windows hook installs");

    let _ = execute_list_windows_all(&handler, "list-windows -a -F '#{session_name}'").await;
    wait_for_named_buffer(&handler, "list-windows-hook", b"fired").await;
}

#[tokio::test]
async fn queued_list_windows_all_validates_target_and_scopes_success_and_error_hooks() {
    let handler = RequestHandler::new();
    create_window_listing_fixture(&handler).await;
    let hooks = CommandParser::new()
        .parse(
            "set-hook -t alpha after-list-windows 'set-buffer -b alpha-list-success yes' ; \
             set-hook -t beta after-list-windows 'set-buffer -b beta-list-success yes' ; \
             set-hook -t alpha command-error 'set-buffer -b alpha-list-error yes' ; \
             set-hook -t beta command-error 'set-buffer -b beta-list-error yes'",
        )
        .expect("scoped list hooks parse");
    handler
        .execute_parsed_commands_for_test(std::process::id(), hooks)
        .await
        .expect("scoped list hooks install");

    let stdout = execute_list_windows_all(
        &handler,
        "list-windows -a -t beta -F '#{session_name}:#{window_index}'",
    )
    .await;
    assert!(stdout.starts_with("alpha:0\n"), "{stdout:?}");
    wait_for_named_buffer(&handler, "beta-list-success", b"yes").await;
    assert!(handler
        .state
        .lock()
        .await
        .buffers
        .get("alpha-list-success")
        .is_none());

    let invalid_sort = CommandParser::new()
        .parse("list-windows -a -t beta -O definitely-invalid")
        .expect("invalid sort command parses structurally");
    let error = handler
        .execute_parsed_commands_for_test(std::process::id(), invalid_sort)
        .await
        .expect_err("invalid sort should fail");
    assert!(error.to_string().contains(rmux_core::INVALID_SORT_ORDER));
    wait_for_named_buffer(&handler, "beta-list-error", b"yes").await;
    assert!(handler
        .state
        .lock()
        .await
        .buffers
        .get("alpha-list-error")
        .is_none());

    let missing_target = CommandParser::new()
        .parse("list-windows -a -t definitely-missing")
        .expect("missing target command parses structurally");
    let error = handler
        .execute_parsed_commands_for_test(std::process::id(), missing_target)
        .await
        .expect_err("missing target should fail before listing");
    assert!(error.to_string().contains("can't find session"), "{error}");
}

#[tokio::test]
async fn hook_binding_and_alias_entries_share_list_windows_all_execution() {
    let handler = RequestHandler::new();
    create_window_listing_fixture(&handler).await;

    let hook = CommandParser::new()
        .parse(
            r#"set-hook -g after-new-window "run-shell -C 'list-windows -a ; set-buffer -b list-windows-hook-entry ok'""#,
        )
        .expect("hook body parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), hook)
        .await
        .expect("hook installs");
    let new_window = CommandParser::new()
        .parse("new-window -d -t alpha")
        .expect("new-window parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), new_window)
        .await
        .expect("new-window and hook execute");
    wait_for_named_buffer(&handler, "list-windows-hook-entry", b"ok").await;

    let requester_pid = u32::MAX - 91_102;
    let (attach_tx, _attach_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session_name("alpha"), attach_tx)
        .await;
    let binding = CommandParser::new()
        .parse(
            r#"bind-key X "run-shell -C 'list-windows -a ; set-buffer -b list-windows-binding-entry ok'""#,
        )
        .expect("binding body parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, binding)
        .await
        .expect("binding installs");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02X")
        .await
        .expect("binding executes");
    wait_for_named_buffer(&handler, "list-windows-binding-entry", b"ok").await;

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::CommandAlias,
                value: "lwa=list-windows -a -F '#{session_name}:#{window_index}'".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    let alias = handler
        .parse_command_string_one_group("lwa")
        .await
        .expect("runtime alias parses");
    let output = handler
        .execute_parsed_commands_for_test(requester_pid, alias)
        .await
        .expect("runtime alias executes");
    assert_eq!(
        output.stdout(),
        b"alpha:0\nalpha:1\nalpha:2\nbeta:0\nbeta:1\n"
    );
}

#[tokio::test]
async fn startup_config_entry_executes_list_windows_all_and_continues() {
    let handler = RequestHandler::new();
    create_window_listing_fixture(&handler).await;
    let root = temp_root("list-windows-all-startup");
    fs::create_dir_all(&root).expect("create startup root");
    fs::write(
        root.join("startup.conf"),
        "list-windows -a -F '#{session_name}:#{window_index}'\nset-option -g @list-windows-startup ok\n",
    )
    .expect("write startup config");
    let config = crate::DaemonConfig::new(root.join("rmux.sock")).with_config_files(
        vec![std::path::PathBuf::from("startup.conf")],
        false,
        Some(root.clone()),
    );

    handler
        .load_startup_config(config.config_load().clone())
        .await;
    assert!(
        handler.startup_config_errors.lock().await.is_empty(),
        "startup list-windows -a should not produce a config error"
    );
    let show = CommandParser::new()
        .parse("show-options -gqv @list-windows-startup")
        .expect("show startup marker parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), show)
        .await
        .expect("startup marker is readable");
    assert_eq!(output.stdout(), b"ok\n");

    fs::remove_dir_all(root).expect("remove startup root");
}
