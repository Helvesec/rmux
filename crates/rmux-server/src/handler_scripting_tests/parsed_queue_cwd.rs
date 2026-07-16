use super::*;

#[tokio::test]
async fn queued_window_spawns_use_non_attached_caller_cwd() {
    let handler = RequestHandler::new();
    let root = temp_root("queued-window-caller-cwd");
    let session_cwd = root.join("session");
    let caller_cwd = root.join("caller");
    fs::create_dir_all(&session_cwd).expect("session cwd");
    fs::create_dir_all(&caller_cwd).expect("caller cwd");
    let session_cwd = fs::canonicalize(session_cwd).expect("canonical session cwd");
    let caller_cwd = fs::canonicalize(caller_cwd).expect("canonical caller cwd");
    let session = session_name("queued-window-caller-cwd");
    create_session_with_cwd(&handler, &session, &session_cwd).await;

    let context = QueueExecutionContext::new(Some(caller_cwd.clone())).with_current_target(Some(
        Target::Pane(PaneTarget::with_window(session.clone(), 0, 0)),
    ));
    let new_window = CommandParser::new()
        .parse("new-window -d -n caller-window")
        .expect("new-window parses");
    handler
        .execute_parsed_commands(std::process::id(), new_window, context.clone())
        .await
        .expect("new-window executes");
    let split_window = CommandParser::new()
        .parse("split-window -d")
        .expect("split-window parses");
    handler
        .execute_parsed_commands(std::process::id(), split_window, context)
        .await
        .expect("split-window executes");

    assert_window_and_split_cwds(&handler, &session, &caller_cwd).await;
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn queued_window_spawns_without_caller_cwd_keep_session_cwd() {
    let handler = RequestHandler::new();
    let root = temp_root("queued-window-attached-cwd");
    let session_cwd = root.join("session");
    fs::create_dir_all(&session_cwd).expect("session cwd");
    let session_cwd = fs::canonicalize(session_cwd).expect("canonical session cwd");
    let session = session_name("queued-window-attached-cwd");
    create_session_with_cwd(&handler, &session, &session_cwd).await;

    let context = QueueExecutionContext::without_caller_cwd().with_current_target(Some(
        Target::Pane(PaneTarget::with_window(session.clone(), 0, 0)),
    ));
    let new_window = CommandParser::new()
        .parse("new-window -d -n attached-window")
        .expect("new-window parses");
    handler
        .execute_parsed_commands(std::process::id(), new_window, context.clone())
        .await
        .expect("new-window executes");
    let split_window = CommandParser::new()
        .parse("split-window -d")
        .expect("split-window parses");
    handler
        .execute_parsed_commands(std::process::id(), split_window, context)
        .await
        .expect("split-window executes");

    assert_window_and_split_cwds(&handler, &session, &session_cwd).await;
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn queued_window_explicit_cwd_overrides_non_attached_caller_cwd() {
    let handler = RequestHandler::new();
    let root = temp_root("queued-window-explicit-cwd");
    let session_cwd = root.join("session");
    let caller_cwd = root.join("caller");
    let explicit_cwd = root.join("explicit");
    for path in [&session_cwd, &caller_cwd, &explicit_cwd] {
        fs::create_dir_all(path).expect("test cwd");
    }
    let session_cwd = fs::canonicalize(session_cwd).expect("canonical session cwd");
    let caller_cwd = fs::canonicalize(caller_cwd).expect("canonical caller cwd");
    let explicit_cwd = fs::canonicalize(explicit_cwd).expect("canonical explicit cwd");
    let session = session_name("queued-window-explicit-cwd");
    create_session_with_cwd(&handler, &session, &session_cwd).await;

    let context = QueueExecutionContext::new(Some(caller_cwd)).with_current_target(Some(
        Target::Pane(PaneTarget::with_window(session.clone(), 0, 0)),
    ));
    let new_window = CommandParser::new()
        .parse(&format!(
            "new-window -d -n explicit-window -c {}",
            shell_quote(&explicit_cwd)
        ))
        .expect("new-window parses");
    handler
        .execute_parsed_commands(std::process::id(), new_window, context.clone())
        .await
        .expect("new-window executes");
    let split_window = CommandParser::new()
        .parse(&format!(
            "split-window -d -c {}",
            shell_quote(&explicit_cwd)
        ))
        .expect("split-window parses");
    handler
        .execute_parsed_commands(std::process::id(), split_window, context)
        .await
        .expect("split-window executes");

    assert_window_and_split_cwds(&handler, &session, &explicit_cwd).await;
    let _ = fs::remove_dir_all(root);
}

async fn create_session_with_cwd(handler: &RequestHandler, session: &SessionName, cwd: &Path) {
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: Some(cwd.to_string_lossy().into_owned()),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
}

async fn assert_window_and_split_cwds(
    handler: &RequestHandler,
    session_name: &SessionName,
    expected: &Path,
) {
    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(session_name)
        .expect("session exists");
    let new_window_pane_id = session
        .window_at(1)
        .expect("new window exists")
        .pane(0)
        .expect("new window pane exists")
        .id();
    let split_pane_id = session
        .window_at(0)
        .expect("initial window exists")
        .pane(1)
        .expect("split pane exists")
        .id();
    assert_eq!(
        state
            .pane_lifecycle(new_window_pane_id)
            .expect("new window lifecycle")
            .working_directory(),
        Some(expected)
    );
    assert_eq!(
        state
            .pane_lifecycle(split_pane_id)
            .expect("split pane lifecycle")
            .working_directory(),
        Some(expected)
    );
}
