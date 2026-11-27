use super::*;

async fn fixture() -> RequestHandler {
    let handler = RequestHandler::new();
    create_background_identity_session(&handler, session_name("alpha")).await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name("alpha"),
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
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    queue(&handler, "set-option -w -t alpha:0 window-status-format ZERO; set-option -w -t alpha:1 window-status-format ONE").await;
    handler
}

async fn queue(handler: &RequestHandler, script: &str) -> Vec<u8> {
    handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            CommandParser::new()
                .parse(script)
                .expect("fixture commands parse"),
        )
        .await
        .expect("fixture commands execute")
        .stdout()
        .to_vec()
}

async fn snapshot_and_close(handler: &RequestHandler) -> Vec<u8> {
    let result = queue(handler, "show-options -wv -t alpha:0 window-status-format; show-options -wv -t alpha:1 window-status-format").await;
    queue(handler, "kill-session -t alpha").await;
    result
}

#[tokio::test]
async fn config_parse_natural_window_option_preserves_explicit_window_and_pane() {
    for target in ["alpha:1", "alpha:1.0"] {
        let handler = fixture().await;
        queue(&handler, "select-window -t alpha:0").await;
        queue(
            &handler,
            &format!("set-option -t {target} window-status-format MARKER"),
        )
        .await;
        assert_eq!(
            snapshot_and_close(&handler).await,
            b"ZERO\nMARKER\n",
            "{target}"
        );
    }
}

#[tokio::test]
async fn config_parse_natural_window_option_works_without_window_zero() {
    let handler = fixture().await;
    queue(&handler, "kill-window -t alpha:0").await;
    let result = handler.execute_parsed_commands_for_test(std::process::id(),
        CommandParser::new().parse("set-option -t alpha window-status-format MARKER; show-options -wv -t alpha:1 window-status-format").unwrap()).await;
    queue(&handler, "kill-session -t alpha").await;
    assert_eq!(
        result
            .expect("valid session resolves its surviving window")
            .stdout(),
        b"MARKER\n"
    );
}

#[tokio::test]
async fn config_parse_natural_window_option_source_file_keeps_target() {
    let handler = fixture().await;
    let root = temp_root("natural-window-option");
    let path = root.join("options.conf");
    write_config(&path, "set-option -t alpha window-status-format MARKER\n");
    let response = handler
        .handle(source_file_request(
            vec![path.to_string_lossy().into_owned()],
            None,
        ))
        .await;
    fs::remove_dir_all(root).expect("remove source fixture");
    let observed = snapshot_and_close(&handler).await;
    let Response::SourceFile(response) = response else {
        panic!("{response:?}")
    };
    assert_eq!(response.exit_status().unwrap_or(0), 0);
    assert_eq!(observed, b"ZERO\nMARKER\n");
}

#[tokio::test]
async fn config_parse_natural_window_option_control_keeps_target() {
    let handler = fixture().await;
    let pid = u32::MAX - 251;
    let (id, _events) = register_control_for_session(&handler, pid, session_name("alpha")).await;
    let result = handler
        .execute_control_commands_identity(
            pid,
            id,
            CommandParser::new()
                .parse("set-option -t alpha window-status-format MARKER")
                .unwrap(),
        )
        .await;
    handler.finish_control(pid, id).await;
    let observed = snapshot_and_close(&handler).await;
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(observed, b"ZERO\nMARKER\n");
}

#[tokio::test]
async fn config_parse_natural_window_option_keeps_session_current_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
    assert!(matches!(
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
            .await,
        Response::NewWindow(_)
    ));
    let output = handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            CommandParser::new()
                .parse(
                    "set-option -w -t alpha:0 window-status-format ZERO; \
             set-option -w -t alpha:1 window-status-format ONE; \
             set-option -t alpha window-status-format MARKER; \
             show-options -wv -t alpha:0 window-status-format; \
             show-options -wv -t alpha:1 window-status-format",
                )
                .expect("acceptance queue parses"),
        )
        .await;
    handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert_eq!(
        output.expect("acceptance queue executes").stdout(),
        b"ZERO\nMARKER\n"
    );
}
