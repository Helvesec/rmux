use super::*;

#[tokio::test]
async fn source_file_skips_parse_error_file_and_continues_other_paths() {
    let handler = RequestHandler::new();
    let root = temp_root("multi-path-parse-error");
    let bad = root.join("bad.conf");
    let good = root.join("good.conf");
    write_config(&bad, "set-buffer -b before-error ok\nnot-a-command\n");
    write_config(&good, "set-buffer -b parsed-after ok\n");

    let response = handler
        .handle(source_file_request(
            vec!["bad.conf".to_owned(), "good.conf".to_owned()],
            Some(root),
        ))
        .await;

    let Response::Error(error) = response else {
        panic!("source-file should fail on parse errors, got {response:?}");
    };
    assert!(
        matches!(
            error.error,
            rmux_proto::RmuxError::Server(ref message)
                if message == &format!("{}:2: unknown command: not-a-command", bad.display())
        ),
        "unexpected source-file parse error: {error:?}"
    );
    assert!(matches!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("before-error".to_owned()),
            }))
            .await,
        Response::Error(_)
    ));
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("parsed-after".to_owned()),
            }))
            .await
            .command_output()
            .expect("good source path should still run")
            .stdout(),
        b"ok"
    );
}

#[tokio::test]
async fn nested_source_file_continues_outer_after_parse_errors_without_recovered_commands() {
    let handler = RequestHandler::new();
    let root = temp_root("nested-source-parse-error");
    write_config(
        &root.join("outer.conf"),
        "source-file inner.conf\nset-buffer -b outer-after yes\n",
    );
    write_config(
        &root.join("inner.conf"),
        "bogus-command\nset-buffer -b nested-after yes\n",
    );

    let response = handler
        .handle(source_file_request(
            vec!["outer.conf".to_owned()],
            Some(root),
        ))
        .await;

    let Response::Error(error) = response else {
        panic!("nested source-file should fail on parse errors, got {response:?}");
    };
    assert!(
        matches!(
            error.error,
            rmux_proto::RmuxError::Server(ref message)
                if message.contains("inner.conf:1: unknown command: bogus-command")
        ),
        "unexpected nested source-file parse error: {error:?}"
    );
    assert!(matches!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("nested-after".to_owned()),
            }))
            .await,
        Response::Error(_)
    ));
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("outer-after".to_owned()),
            }))
            .await
            .command_output()
            .expect("outer command after nested parse error should run")
            .stdout(),
        b"yes"
    );
}

#[tokio::test]
async fn nested_source_file_skips_child_after_command_syntax_error() {
    let handler = RequestHandler::new();
    let root = temp_root("nested-source-command-syntax-error");
    write_config(
        &root.join("outer.conf"),
        "source-file inner.conf\nset-buffer -b outer-after yes\n",
    );
    write_config(
        &root.join("inner.conf"),
        "set-buffer -b child-before yes\nnew-window -Q\nset-buffer -b child-after yes\n",
    );

    let response = handler
        .handle(source_file_request(
            vec!["outer.conf".to_owned()],
            Some(root),
        ))
        .await;

    let Response::Error(error) = response else {
        panic!("nested source-file should fail on command syntax errors, got {response:?}");
    };
    assert!(
        matches!(
            error.error,
            rmux_proto::RmuxError::Server(ref message)
                if message.contains("inner.conf:2:")
                    && message.contains("new-window")
                    && message.contains("unknown flag -Q")
        ),
        "unexpected nested source-file syntax error: {error:?}"
    );
    for name in ["child-before", "child-after"] {
        assert!(matches!(
            handler
                .handle(Request::ShowBuffer(ShowBufferRequest {
                    name: Some(name.to_owned()),
                }))
                .await,
            Response::Error(_)
        ));
    }
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("outer-after".to_owned()),
            }))
            .await
            .command_output()
            .expect("outer command after nested syntax error should run")
            .stdout(),
        b"yes"
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
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
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
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
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
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
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
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: Some(root),
            stdin: None,
        })))
        .await;

    let Response::Error(error) = response else {
        panic!("source-file should report missing stdin, got {response:?}");
    };
    assert!(
        matches!(error.error, rmux_proto::RmuxError::Server(ref message) if message.contains("stdin")),
        "expected stdin error, got {error:?}"
    );
}

#[tokio::test]
async fn source_file_ignores_server_scope_for_non_server_options_like_tmux() {
    let handler = RequestHandler::new();
    let root = temp_root("set-option-server-scope");
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
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: Some(PaneTarget::with_window(alpha, 0, 0)),
            caller_cwd: Some(root),
            stdin: Some("set-option -s status off\n".to_owned()),
        })))
        .await;

    assert_eq!(
        response,
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
    );
    let response = handler
        .handle(Request::ShowOptions(ShowOptionsRequest {
            scope: OptionScopeSelector::SessionGlobal,
            name: Some("status".to_owned()),
            value_only: true,
            include_inherited: false,
            quiet: false,
        }))
        .await;
    assert_eq!(
        response.command_output().expect("status output").stdout(),
        b"on\n"
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
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: Some(PaneTarget::with_window(alpha, 0, 0)),
            caller_cwd: Some(root),
            stdin: Some(
                "set-option -s message-limit 77\n\
set -gq status off\n\
set -gw pane-border-style fg=colour3\n\
set-window-option -gw pane-active-border-style fg=colour5\n\
set -gw copy-mode-selection-style bg=cyan,fg=black\n\
set-option -ag status-left append\n\
	show-options -gqsv -t alpha message-limit\n\
show-options -gqv status\n\
show-window-options -g -t alpha -v pane-border-style\n\
show-window-options -g -v pane-active-border-style\n\
show-window-options -g -v copy-mode-selection-style\n"
                    .to_owned(),
            ),
        })))
        .await;

    assert_eq!(
        response
            .command_output()
            .unwrap_or_else(|| panic!("queued show-options output, got {response:?}"))
            .stdout(),
        b"77\noff\nfg=colour3\nfg=colour5\nbg=cyan,fg=black\n"
    );
}

#[tokio::test]
async fn source_file_show_options_quiet_suppresses_missing_options_with_current_targets() {
    let handler = RequestHandler::new();
    let root = temp_root("show-options-quiet-missing");
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
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: Some(PaneTarget::with_window(alpha, 0, 0)),
            caller_cwd: Some(root),
            stdin: Some(
                "show-options -q @missing\n\
show-options -wq @missing\n\
show-options -pq @missing\n\
show-options -gq nonexistent\n"
                    .to_owned(),
            ),
        })))
        .await;

    assert_eq!(
        response,
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
    );
}

#[tokio::test]
async fn source_file_set_option_p_preserves_explicit_pane_target() {
    let handler = RequestHandler::new();
    let root = temp_root("set-option-pane-target");
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
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: Some(root),
            stdin: Some(
                "set-option -p -t alpha:0.0 pane-border-style fg=blue\n\
show-options -pqv -t alpha:0.0 pane-border-style\n"
                    .to_owned(),
            ),
        })))
        .await;

    assert_eq!(
        response
            .command_output()
            .unwrap_or_else(|| panic!("queued show-options output, got {response:?}"))
            .stdout(),
        b"fg=blue\n"
    );
}

#[tokio::test]
async fn source_file_set_option_format_expands_value_before_storage() {
    let handler = RequestHandler::new();
    let root = temp_root("set-option-format");
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
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: Some(PaneTarget::with_window(alpha, 0, 0)),
            caller_cwd: Some(root),
            stdin: Some(
                "set-option -gF @probe '#{session_name}-#{window_index}'\n\
show-options -gqv @probe\n"
                    .to_owned(),
            ),
        })))
        .await;

    assert_eq!(
        response
            .command_output()
            .unwrap_or_else(|| panic!("queued show-options output, got {response:?}"))
            .stdout(),
        b"alpha-0\n"
    );
}

#[tokio::test]
async fn source_file_set_option_format_expands_socket_path() {
    let handler = RequestHandler::new();
    handler.set_socket_path("/tmp/rmux-test.sock");
    let root = temp_root("set-option-socket-path");
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
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: Some(PaneTarget::with_window(alpha, 0, 0)),
            caller_cwd: Some(root),
            stdin: Some(
                "set-option -gF @probe '#{socket_path}'\n\
show-options -gqv @probe\n"
                    .to_owned(),
            ),
        })))
        .await;

    assert_eq!(
        response
            .command_output()
            .unwrap_or_else(|| panic!("queued show-options output, got {response:?}"))
            .stdout(),
        b"/tmp/rmux-test.sock\n"
    );
}

#[tokio::test]
async fn source_file_set_option_format_sees_earlier_global_user_option_without_target() {
    let handler = RequestHandler::new();
    let root = temp_root("set-option-format-global-option");
    fs::create_dir_all(&root).expect("create temp root");

    let response = handler
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: Some(root),
            stdin: Some(
                "set-option -g @fmt '#{session_name}'\n\
set-option -gF @expanded '#{@fmt}'\n\
show-options -gqv @expanded\n"
                    .to_owned(),
            ),
        })))
        .await;

    assert_eq!(
        response
            .command_output()
            .unwrap_or_else(|| panic!("queued show-options output, got {response:?}"))
            .stdout(),
        b"#{session_name}\n"
    );
}

#[tokio::test]
async fn source_file_without_target_routes_append_to_default_global_scope() {
    let handler = RequestHandler::new();
    let root = temp_root("set-option-append-no-target");
    fs::create_dir_all(&root).expect("create temp root");

    let response = handler
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: Some(root),
            stdin: Some("set-option -ag status-left append\n".to_owned()),
        })))
        .await;

    assert!(
        matches!(response, Response::SourceFile(_)),
        "set-option append without a current target should load, got {response:?}"
    );
    let state = handler.state.lock().await;
    assert_eq!(
        state.options.global_value(OptionName::StatusLeft),
        Some("[#{session_name}] append")
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
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
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
        })))
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
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
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
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
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
