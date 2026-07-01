use super::*;
#[tokio::test]
async fn run_shell_foreground_suppresses_stdout_like_tmux() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;

    let response = handler
        .handle(run_shell(&shell_print_command("hello"), false))
        .await;

    assert_eq!(
        response,
        Response::RunShell(RunShellResponse::from_exit_status(0))
    );
}

#[tokio::test]
async fn run_shell_nonzero_returns_exact_exit_status_without_stdout() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;

    let response = handler
        .handle(run_shell(
            &shell_print_then_exit_command("hidden", 7),
            false,
        ))
        .await;

    assert_eq!(
        response,
        Response::RunShell(RunShellResponse::from_exit_status(7))
    );
}

#[tokio::test]
async fn run_shell_background_returns_immediately_without_output() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(run_shell(&shell_success_command(), true))
        .await;

    assert_eq!(response, Response::RunShell(RunShellResponse::background()));
}

#[tokio::test]
async fn background_run_shell_is_tracked_as_detached_request_until_finished() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::RunShell(Box::new(RunShellRequest {
            command: String::new(),
            background: true,
            as_commands: false,
            show_stderr: false,
            delay_seconds: Some(RunShellDelaySeconds(0.2)),
            start_directory: None,
            target: None,
            source_depth: None,
        })))
        .await;

    assert_eq!(response, Response::RunShell(RunShellResponse::background()));
    wait_for_detached_request_count(&handler, 1).await;
    wait_for_detached_request_count(&handler, 0).await;
}

#[tokio::test]
async fn background_run_shell_commands_keep_detached_write_access_after_response() {
    let handler = RequestHandler::new();
    let requester_pid = 424_005;
    let parsed = CommandParser::new()
        .parse("run-shell -b -d 0.05 -C 'set-buffer -b bg-run-shell ok'")
        .expect("background run-shell command parses");

    {
        let _access = handler.begin_detached_requester_access(requester_pid, true);
        let output = handler
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await
            .expect("background run-shell dispatch succeeds");
        assert!(output.stdout().is_empty());
    }

    wait_for_named_buffer(&handler, "bg-run-shell", b"ok").await;
}

#[tokio::test]
async fn background_run_shell_commands_still_emit_after_hooks_outside_hook_context() {
    let handler = RequestHandler::new();
    create_named_session(&handler, "run-shell-after-hooks").await;
    execute_test_command(
        &handler,
        "set-hook -g after-new-window 'set-buffer -b after-run-shell yes'",
    )
    .await;

    execute_test_command(&handler, "run-shell -b -C 'new-window -d -n bg'").await;

    wait_for_named_buffer(&handler, "after-run-shell", b"yes").await;
}

#[tokio::test]
async fn background_if_shell_still_emits_after_hooks_outside_hook_context() {
    let handler = RequestHandler::new();
    create_named_session(&handler, "if-shell-after-hooks").await;
    execute_test_command(
        &handler,
        "set-hook -g after-new-window 'set-buffer -b after-if-shell yes'",
    )
    .await;

    execute_test_command(&handler, "if-shell -b -F '1' 'new-window -d -n ifbg'").await;

    wait_for_named_buffer(&handler, "after-if-shell", b"yes").await;
}

#[tokio::test]
async fn background_run_shell_preserves_hook_formats_after_hook_scope_exits() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;
    let root = temp_root("run-shell-background-hook-formats");
    std::fs::create_dir_all(&root).expect("temp output root");
    let output_path = root.join("hook-pane.txt");
    let command = write_literal_format_command(&output_path, "#{hook_pane}");

    let response = crate::hook_runtime::with_hook_execution(
        vec![("hook_pane".to_owned(), "%1".to_owned())],
        async {
            handler
                .handle(Request::RunShell(Box::new(RunShellRequest {
                    command,
                    background: true,
                    as_commands: false,
                    show_stderr: true,
                    delay_seconds: None,
                    start_directory: None,
                    target: None,
                    source_depth: None,
                })))
                .await
        },
    )
    .await;

    assert_eq!(response, Response::RunShell(RunShellResponse::background()));
    wait_for_file_text(&output_path, "%1").await;
}

#[tokio::test]
async fn run_shell_expands_socket_path_without_target() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;
    handler.set_socket_path("/tmp/rmux-test.sock");
    let root = temp_root("run-shell-socket-path");
    std::fs::create_dir_all(&root).expect("temp output root");
    let output_path = root.join("socket-path.txt");
    let command = write_text_command(&output_path, "#{socket_path}");

    let response = handler
        .handle(Request::RunShell(Box::new(RunShellRequest {
            command,
            background: false,
            as_commands: false,
            show_stderr: true,
            delay_seconds: None,
            start_directory: None,
            target: None,
            source_depth: None,
        })))
        .await;

    assert_eq!(
        response,
        Response::RunShell(RunShellResponse::from_exit_status(0))
    );
    assert_eq!(
        std::fs::read_to_string(output_path).expect("socket path output"),
        "/tmp/rmux-test.sock"
    );
}

async fn create_named_session(handler: &RequestHandler, name: &str) {
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name(name),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
}

async fn execute_test_command(handler: &RequestHandler, command: &str) {
    let parsed = CommandParser::new()
        .parse(command)
        .unwrap_or_else(|error| panic!("{command:?} should parse: {error}"));
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .unwrap_or_else(|error| panic!("{command:?} should execute: {error}"));
}

async fn wait_for_file_text(path: &std::path::Path, expected: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                if text == expected {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("file {path:?} did not become {expected:?}"));
}

fn write_literal_format_command(path: &std::path::Path, text: &str) -> String {
    #[cfg(unix)]
    {
        format!(
            "printf '%s' {} > {}",
            command_quote(text),
            shell_quote(path)
        )
    }
    #[cfg(windows)]
    {
        format!(
            "[IO.File]::WriteAllText({}, {})",
            crate::test_shell::powershell_quote_path(path),
            crate::test_shell::powershell_quote(text)
        )
    }
}

fn write_text_command(path: &std::path::Path, text: &str) -> String {
    #[cfg(unix)]
    {
        format!("printf {} > {}", command_quote(text), shell_quote(path))
    }
    #[cfg(windows)]
    {
        format!(
            "[IO.File]::WriteAllText({}, {})",
            crate::test_shell::powershell_quote_path(path),
            crate::test_shell::powershell_quote(text)
        )
    }
}

#[tokio::test]
async fn queue_parsed_run_shell_accepts_tmux_compact_delay_flag_without_running_a_shell_command() {
    let handler = RequestHandler::new();

    let parsed = handler
        .parse_command_string_one_group("run-shell -d0.01")
        .await
        .expect("compact tmux delay syntax parses");

    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("delay-only run-shell executes");

    assert!(
        output.stdout().is_empty(),
        "delay-only run-shell should not emit stdout, got: {:?}",
        String::from_utf8_lossy(output.stdout())
    );
}

#[tokio::test]
async fn run_shell_rejects_invalid_delay_without_closing_connection() {
    let handler = RequestHandler::new();

    for delay in [-1.0, f64::NAN, f64::INFINITY] {
        let response = handler
            .handle(Request::RunShell(Box::new(RunShellRequest {
                command: shell_success_command(),
                background: false,
                as_commands: false,
                show_stderr: false,
                delay_seconds: Some(RunShellDelaySeconds(delay)),
                start_directory: None,
                target: None,
                source_depth: None,
            })))
            .await;

        assert!(
            matches!(&response, Response::Error(error) if error.error.to_string().contains("non-negative finite delay")),
            "expected invalid delay error for {delay:?}, got {response:?}"
        );
    }
}

#[tokio::test]
async fn run_shell_background_rejects_invalid_delay_before_reporting_success() {
    let handler = RequestHandler::new();

    for delay in [-1.0, f64::NAN, f64::INFINITY] {
        let response = handler
            .handle(Request::RunShell(Box::new(RunShellRequest {
                command: shell_success_command(),
                background: true,
                as_commands: false,
                show_stderr: false,
                delay_seconds: Some(RunShellDelaySeconds(delay)),
                start_directory: None,
                target: None,
                source_depth: None,
            })))
            .await;

        assert!(
            matches!(&response, Response::Error(error) if error.error.to_string().contains("non-negative finite delay")),
            "expected invalid background delay error for {delay:?}, got {response:?}"
        );
    }
}

#[tokio::test]
async fn queue_parsed_run_shell_rejects_invalid_delay() {
    let handler = RequestHandler::new();

    let parsed = handler
        .parse_command_string_one_group("run-shell -d -1 true")
        .await
        .expect("command text should parse before semantic validation");
    let error = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect_err("negative run-shell delay should be rejected");

    assert!(
        error.to_string().contains("non-negative finite delay"),
        "unexpected error: {error}"
    );
}

#[test]
fn parsed_run_shell_accepts_tmux_clustered_no_value_flags() {
    let handler = RequestHandler::new();
    let state = handler.state.blocking_lock();
    let parsed = crate::handler::scripting_support::parse_request_from_parts(
        "run-shell".to_owned(),
        vec!["-bC".to_owned(), "set-option -g @compact yes".to_owned()],
        None,
        &state.sessions,
        &state.options,
        &TargetFindContext::new(None),
    )
    .expect("run-shell -bC parses like tmux");

    let Request::RunShell(request) = parsed else {
        panic!("expected RunShell request");
    };
    assert!(request.background);
    assert!(request.as_commands);
    assert!(!request.show_stderr);
    assert_eq!(request.command, "set-option -g @compact yes");
}

#[test]
fn parsed_send_keys_accepts_tmux_clustered_no_value_flags() {
    let handler = RequestHandler::new();
    let state = handler.state.blocking_lock();
    let parsed = crate::handler::scripting_support::parse_request_from_parts(
        "send-keys".to_owned(),
        vec!["-lR".to_owned(), "ABC".to_owned()],
        None,
        &state.sessions,
        &state.options,
        &TargetFindContext::new(None),
    )
    .expect("send-keys -lR parses like tmux");

    let Request::SendKeysExt(request) = parsed else {
        panic!("expected SendKeysExt request");
    };
    assert!(request.literal);
    assert!(request.reset_terminal);
    assert_eq!(request.keys, vec!["ABC".to_owned()]);
}

#[tokio::test]
async fn parsed_new_session_start_directory_sets_session_cwd() {
    let handler = RequestHandler::new();
    let root = temp_root("new-session-cwd");
    fs::create_dir_all(&root).expect("start directory");
    let parsed = CommandParser::new()
        .parse(&format!(
            "new-session -d -s alpha -c {}",
            shell_quote(&root)
        ))
        .expect("new-session -c parses");

    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("new-session -c executes");

    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&session_name("alpha"))
        .expect("session created");
    assert_eq!(session.cwd(), Some(root.as_path()));
}

#[test]
fn parsed_new_session_accepts_tmux_shell_command_after_double_dash() {
    let handler = RequestHandler::new();
    let state = handler.state.blocking_lock();
    let parsed = crate::handler::scripting_support::parse_request_from_parts(
        "new-session".to_owned(),
        vec![
            "-d".to_owned(),
            "-s".to_owned(),
            "alpha".to_owned(),
            "--".to_owned(),
            "sleep".to_owned(),
            "30".to_owned(),
        ],
        None,
        &state.sessions,
        &state.options,
        &TargetFindContext::new(None),
    )
    .expect("new-session shell command after -- parses");

    assert_eq!(
        parsed,
        Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session_name("alpha")),
            working_directory: None,
            detached: true,
            size: None,
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(vec!["sleep".to_owned(), "30".to_owned()]),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        }))
    );
}

#[test]
fn parsed_new_session_accepts_skip_environment_update() {
    let handler = RequestHandler::new();
    let state = handler.state.blocking_lock();
    let parsed = crate::handler::scripting_support::parse_request_from_parts(
        "new-session".to_owned(),
        vec![
            "-E".to_owned(),
            "-d".to_owned(),
            "-s".to_owned(),
            "alpha".to_owned(),
        ],
        None,
        &state.sessions,
        &state.options,
        &TargetFindContext::new(None),
    )
    .expect("new-session -E parses");

    let Request::NewSessionExt(request) = parsed else {
        panic!("expected NewSessionExt request");
    };

    assert!(request.skip_environment_update);
    assert_eq!(request.session_name, Some(session_name("alpha")));
    assert!(request.detached);
}
