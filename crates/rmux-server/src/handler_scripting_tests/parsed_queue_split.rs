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
        .expect("split-window stdin flag parses at core layer");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("split-window -I creates an empty keep-alive pane");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    let window = session.window_at(0).expect("window exists");
    assert!(
        window.pane(2).is_some(),
        "split-window -I should create a third pane after the earlier split"
    );
}

#[tokio::test]
async fn parsed_queue_split_window_keep_flag_retains_exited_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("split-keep");
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
        .parse("split-window -dk -t split-keep:0.0 'exit 7'")
        .expect("split-window -k parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("split-window -k executes");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let (pane_id, dead) = {
            let state = handler.state.lock().await;
            let pane_id = state
                .sessions
                .session(&alpha)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(1))
                .map(|pane| pane.id());
            let dead = pane_id.map(|pane_id| state.pane_is_dead(&alpha, pane_id));
            (pane_id, dead)
        };
        if dead == Some(true) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "split-window -k did not retain a dead pane; pane_id={pane_id:?}, dead={dead:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn parsed_queue_split_window_direction_flags_follow_tmux_priority() {
    let handler = RequestHandler::new();
    let alpha = session_name("split-priority");
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
        .parse("split-window -h -v -t split-priority:0.0")
        .expect("split-window direction flags parse");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("split-window repeated direction flags execute");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    let window = session.window_at(0).expect("window exists");
    let new_geometry = window.pane(1).expect("new pane exists").geometry();
    assert!(
        new_geometry.cols() < 80,
        "tmux-priority -h should produce a horizontal/left-right split, got {new_geometry:?}"
    );
}

#[tokio::test]
async fn parsed_queue_split_window_full_size_splits_the_window_root() {
    let handler = RequestHandler::new();
    let alpha = session_name("split-full");
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
        .parse("split-window -v -l 5 -t split-full:0.0 ; split-window -f -v -t split-full:0.1")
        .expect("split-window commands parse");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("split-window -f should execute");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    let window = session.window_at(0).expect("window exists");
    let target_rows = window
        .pane(1)
        .expect("nested target pane exists")
        .geometry()
        .rows();
    let full_size_rows = window
        .pane(2)
        .expect("full-size split pane exists")
        .geometry()
        .rows();

    assert!(
        target_rows <= 5,
        "setup split should keep pane 1 in the small nested region, got {target_rows}"
    );
    assert!(
        full_size_rows > 5,
        "split-window -f must split the window root, got {full_size_rows} rows"
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
