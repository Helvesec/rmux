use super::*;

#[tokio::test]
async fn source_file_continues_after_parse_errors_and_loads_later_files() {
    let handler = RequestHandler::new();
    let root = temp_root("multi-path-parse-error");
    let bad = root.join("bad.conf");
    let good = root.join("good.conf");
    write_config(&bad, "not-a-command\n");
    write_config(&good, "set-buffer -b parsed-after ok\n");

    let response = handler
        .handle(source_file_request(
            vec!["bad.conf".to_owned(), "good.conf".to_owned()],
            Some(root),
        ))
        .await;

    match response {
        Response::Error(rmux_proto::ErrorResponse { error }) => {
            assert_eq!(
                error.to_string(),
                format!(
                    "server error: {}: unknown command: not-a-command",
                    bad.display()
                )
            );
        }
        other => panic!("expected source-file error, got {other:?}"),
    }
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("parsed-after".to_owned()),
            }))
            .await
            .command_output()
            .expect("parsed-after buffer output")
            .stdout(),
        b"ok"
    );
}

#[tokio::test]
async fn source_file_continuation_inside_single_quoted_string() {
    let handler = RequestHandler::new();
    let root = temp_root("sq-cont");
    write_config(&root.join("sq.conf"), "set-buffer -b sq 'hello\\\nworld'\n");

    let response = handler
        .handle(source_file_request(vec!["sq.conf".to_owned()], Some(root)))
        .await;

    assert_eq!(
        response,
        Response::SourceFile(rmux_proto::SourceFileResponse { output: None })
    );
    // In single quotes, backslash-newline is literal (no joining).
    // tmux's lexer treats continuation (backslash-newline) at the get_char level,
    // before quote processing. So single-quoted strings DO get continuation joining.
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("sq".to_owned()),
            }))
            .await
            .command_output()
            .expect("sq buffer output")
            .stdout(),
        b"helloworld"
    );
}

#[tokio::test]
async fn source_file_nested_if_elif_else_endif_branches() {
    let handler = RequestHandler::new();
    let root = temp_root("nested-if");
    write_config(
        &root.join("branches.conf"),
        "%if 0\nset-buffer -b branch wrong1\n%elif 0\nset-buffer -b branch wrong2\n%elif 1\nset-buffer -b branch correct\n%else\nset-buffer -b branch wrong3\n%endif\n",
    );

    let response = handler
        .handle(source_file_request(
            vec!["branches.conf".to_owned()],
            Some(root),
        ))
        .await;

    assert_eq!(
        response,
        Response::SourceFile(rmux_proto::SourceFileResponse { output: None })
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("branch".to_owned()),
            }))
            .await
            .command_output()
            .expect("branch buffer output")
            .stdout(),
        b"correct"
    );
}

#[tokio::test]
async fn source_file_if_with_format_expression_condition() {
    let handler = RequestHandler::new();
    let root = temp_root("if-format");
    // current_file is set during source-file loading, so #{current_file} should be truthy.
    write_config(
        &root.join("fmt.conf"),
        "%if #{current_file}\nset-buffer -b fmt-cond yes\n%else\nset-buffer -b fmt-cond no\n%endif\n",
    );

    let response = handler
        .handle(source_file_request(vec!["fmt.conf".to_owned()], Some(root)))
        .await;

    assert_eq!(
        response,
        Response::SourceFile(rmux_proto::SourceFileResponse { output: None })
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("fmt-cond".to_owned()),
            }))
            .await
            .command_output()
            .expect("fmt-cond buffer output")
            .stdout(),
        b"yes"
    );
}

#[tokio::test]
async fn source_file_stdin_dash_without_stdin_returns_error() {
    let handler = RequestHandler::new();
    let root = temp_root("stdin-missing");
    fs::create_dir_all(&root).expect("create temp root");

    let response = handler
        .handle(Request::SourceFile(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: Some(root),
            stdin: None,
        }))
        .await;

    assert!(
        matches!(response, Response::Error(ref e) if e.error.to_string().contains("stdin")),
        "expected stdin error, got {response:?}"
    );
}

#[tokio::test]
async fn source_file_routes_window_show_commands_and_global_show_scope_compatibility() {
    let handler = RequestHandler::new();
    let root = temp_root("show-options-compat");
    fs::create_dir_all(&root).expect("create temp root");
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

    let response = handler
        .handle(Request::SourceFile(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: Some(PaneTarget::with_window(alpha, 0, 0)),
            caller_cwd: Some(root),
            stdin: Some(
                "set-option -s message-limit 77\n\
set-window-option -g pane-border-style fg=colour3\n\
set-option -g copy-mode-selection-style bg=cyan,fg=black\n\
	show-options -gqsv -t alpha message-limit\n\
show-window-options -g -t alpha -v pane-border-style\n\
show-window-options -g -v copy-mode-selection-style\n"
                    .to_owned(),
            ),
        }))
        .await;

    assert_eq!(
        response
            .command_output()
            .unwrap_or_else(|| panic!("queued show-options output, got {response:?}"))
            .stdout(),
        b"77\nfg=colour3\nbg=cyan,fg=black\n"
    );
}

#[tokio::test]
async fn source_file_without_target_uses_preferred_session_for_parse_time_formats() {
    let handler = RequestHandler::new();
    let root = temp_root("source-file-implicit-target");
    fs::create_dir_all(&root).expect("create temp root");
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

    let response = handler
        .handle(Request::SourceFile(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: Some(root),
            stdin: Some(
                "%if #{==:#{session_name},alpha}\n\
set-buffer -b implicit yes\n\
%else\n\
set-buffer -b implicit no\n\
%endif\n\
if-shell -F '#{==:#{window_index},0}' 'set-buffer -b implicit-if yes' 'set-buffer -b implicit-if no'\n"
                    .to_owned(),
            ),
        }))
        .await;

    assert!(matches!(response, Response::SourceFile(_)));
    let state = handler.state.lock().await;
    let (_, content) = state
        .buffers
        .show(Some("implicit"))
        .expect("implicit buffer exists");
    assert_eq!(String::from_utf8_lossy(content), "yes");
    let (_, content) = state
        .buffers
        .show(Some("implicit-if"))
        .expect("implicit-if buffer exists");
    assert_eq!(String::from_utf8_lossy(content), "yes");
}

#[tokio::test]
async fn source_file_comment_after_command_is_ignored() {
    let handler = RequestHandler::new();
    let root = temp_root("comment-after");
    write_config(
        &root.join("commented.conf"),
        "set-buffer -b commented value # this is a comment\n",
    );

    let response = handler
        .handle(source_file_request(
            vec!["commented.conf".to_owned()],
            Some(root),
        ))
        .await;

    assert_eq!(
        response,
        Response::SourceFile(rmux_proto::SourceFileResponse { output: None })
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("commented".to_owned()),
            }))
            .await
            .command_output()
            .expect("commented buffer output")
            .stdout(),
        b"value"
    );
}

#[tokio::test]
async fn source_file_glob_expands_matching_files() {
    let handler = RequestHandler::new();
    let root = temp_root("glob-expand");
    write_config(&root.join("a.conf"), "set-buffer -b glob-a yes\n");
    write_config(&root.join("b.conf"), "set-buffer -b glob-b yes\n");

    let response = handler
        .handle(source_file_request(vec!["*.conf".to_owned()], Some(root)))
        .await;

    assert_eq!(
        response,
        Response::SourceFile(rmux_proto::SourceFileResponse { output: None })
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("glob-a".to_owned()),
            }))
            .await
            .command_output()
            .expect("glob-a buffer output")
            .stdout(),
        b"yes"
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("glob-b".to_owned()),
            }))
            .await
            .command_output()
            .expect("glob-b buffer output")
            .stdout(),
        b"yes"
    );
}
