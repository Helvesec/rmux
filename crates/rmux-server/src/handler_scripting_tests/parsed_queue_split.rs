use super::*;

#[tokio::test]
async fn parsed_queue_split_window_accepts_start_directory() {
    let handler = RequestHandler::new();
    let alpha = session_name("split-cwd");
    let cwd = temp_root("split-cwd");
    fs::create_dir_all(&cwd).expect("split cwd");
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
        .parse(&format!("split-window -c {}", shell_quote(&cwd)))
        .expect("command parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd().with_current_target(Some(Target::Pane(
                PaneTarget::with_window(alpha.clone(), 0, 0),
            ))),
        )
        .await
        .expect("split-window -c succeeds");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    let pane = session
        .window_at(0)
        .expect("window exists")
        .pane(1)
        .expect("split pane exists");
    let lifecycle = state
        .pane_lifecycle(pane.id())
        .expect("split lifecycle exists");
    assert_eq!(lifecycle.working_directory(), Some(cwd.as_path()));

    let _ = fs::remove_dir_all(cwd);
}

#[tokio::test]
async fn parsed_queue_split_window_applies_stateful_compat_flags() {
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
        .parse("split-window -d -Z -l 5 -t alpha:0.0")
        .expect("split-window compat flags parse");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("supported split-window compat flags execute");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    let window = session.window_at(0).expect("window exists");
    assert_eq!(
        window.active_pane_index(),
        0,
        "split-window -d must preserve the active pane"
    );
    let new_geometry = window.pane(1).expect("new pane exists").geometry();
    assert!(
        new_geometry.cols() == 5 || new_geometry.rows() == 5,
        "split-window -l must size one axis of the new split, got {new_geometry:?}"
    );
    drop(state);

    let parsed = CommandParser::new()
        .parse("split-window -I -t alpha:0.0")
        .expect("split-window unsupported flag parses at core layer");
    let error = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect_err("split-window -I should be rejected before spawn");

    assert_eq!(
        error,
        rmux_proto::RmuxError::Server("unsupported split-window flag: -I".to_owned())
    );
}

#[tokio::test]
async fn parsed_queue_split_window_prints_formatted_target() {
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
        .parse("split-window -P -F '#{session_name}:#{window_index}.#{pane_index}' -t alpha:0.0")
        .expect("split-window print flags parse");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("split-window -P -F succeeds");

    assert_eq!(String::from_utf8_lossy(&output.stdout), "alpha:0.1\n");
}

#[tokio::test]
async fn parsed_queue_split_window_percentage_size_uses_target_pane_axis() {
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
        .parse("split-window -v -l 5 -t alpha:0.0 ; split-window -v -l 50% -t alpha:0.1")
        .expect("nested split-window percentage parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("nested split-window percentage executes");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    let window = session.window_at(0).expect("window exists");
    let original_nested_rows = window
        .pane(1)
        .expect("split target pane exists")
        .geometry()
        .rows();
    let new_rows = window
        .pane(2)
        .expect("percentage split pane exists")
        .geometry()
        .rows();

    assert!(
        new_rows <= 5,
        "percentage split should size from the target pane, got new rows {new_rows}"
    );
    assert!(
        original_nested_rows <= 5,
        "target pane should remain a small nested pane, got rows {original_nested_rows}"
    );
}
