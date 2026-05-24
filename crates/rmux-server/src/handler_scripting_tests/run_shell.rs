use super::*;

#[tokio::test]
async fn run_shell_foreground_captures_stdout() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(run_shell(&shell_print_command("hello"), false))
        .await;

    assert_eq!(
        response,
        Response::RunShell(RunShellResponse::from_output(CommandOutput::from_stdout(
            b"hello".to_vec()
        )))
    );
}

#[tokio::test]
async fn run_shell_nonzero_returns_error_without_stdout_payload() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(run_shell(
            &shell_print_then_exit_command("hidden", 7),
            false,
        ))
        .await;

    assert!(matches!(response, Response::Error(_)));
    assert!(response.command_output().is_none());
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
        &TargetFindContext::new(None),
    )
    .expect("new-session shell command after -- parses");

    assert_eq!(
        parsed,
        Request::NewSessionExt(NewSessionExtRequest {
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
            passthrough: false,
        })
    );
}
