use super::*;

#[tokio::test]
async fn source_file_uses_shared_parser_for_conditions_comments_and_continuations() {
    let handler = RequestHandler::new();
    let root = temp_root("cwd-[glob]");
    let config = root.join("main.conf");
    write_config(
        &config,
        "# ignored comment\n%if #{current_file}\nset-buffer -b chosen yes\\\n-suffix\n%else\nset-buffer -b chosen no\n%endif\n",
    );

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root.clone())) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.verbose = true;
    let response = handler.handle(Request::SourceFile(request)).await;

    let output = response
        .command_output()
        .expect("source-file -v prints parsed commands");
    assert!(
        std::str::from_utf8(output.stdout())
            .expect("verbose output is UTF-8")
            .contains("set-buffer -b chosen yes-suffix"),
        "{}",
        std::str::from_utf8(output.stdout()).expect("verbose output is UTF-8")
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("chosen".to_owned()),
            }))
            .await
            .command_output()
            .expect("chosen buffer output")
            .stdout(),
        b"yes-suffix"
    );
}

#[tokio::test]
async fn source_file_handles_crlf_backslash_continuations() {
    let handler = RequestHandler::new();
    let root = temp_root("crlf-continuation");
    let config = root.join("main.conf");
    write_config(&config, "set-buffer -b crlf win\\\r\ndows\r\n");

    let response = handler
        .handle(source_file_request(
            vec!["main.conf".to_owned()],
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
                name: Some("crlf".to_owned()),
            }))
            .await
            .command_output()
            .expect("crlf buffer output")
            .stdout(),
        b"windows"
    );
}

#[tokio::test]
async fn source_file_parse_only_reports_parse_without_executing() {
    let handler = RequestHandler::new();
    let root = temp_root("parse-only");
    let config = root.join("main.conf");
    write_config(&config, "set-buffer -b parsed value\n");

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.parse_only = true;
    request.verbose = true;
    let response = handler.handle(Request::SourceFile(request)).await;

    assert!(std::str::from_utf8(
        response
            .command_output()
            .expect("parse-only verbose output")
            .stdout()
    )
    .expect("verbose output is UTF-8")
    .contains("set-buffer -b parsed value"));
    assert!(matches!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("parsed".to_owned()),
            }))
            .await,
        Response::Error(_)
    ));
}

#[tokio::test]
async fn source_file_parse_only_validates_command_flags_without_executing() {
    let handler = RequestHandler::new();
    let root = temp_root("parse-only-invalid-command");
    let config = root.join("main.conf");
    write_config(&config, "new-window -Q\nset-buffer -b parsed value\n");

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.parse_only = true;

    let response = handler.handle(Request::SourceFile(request)).await;

    let Response::Error(response) = response else {
        panic!("expected source-file -n to reject invalid command flags");
    };
    assert!(
        response
            .error
            .to_string()
            .contains("command new-window: unknown flag -Q"),
        "{}",
        response.error
    );
    assert!(matches!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("parsed".to_owned()),
            }))
            .await,
        Response::Error(_)
    ));
}

#[tokio::test]
async fn source_file_parse_only_validates_nested_source_files_without_executing() {
    let handler = RequestHandler::new();
    let root = temp_root("parse-only-nested-source");
    write_config(
        &root.join("main.conf"),
        "source-file inner.conf\nset-buffer -b outer parsed\n",
    );
    write_config(
        &root.join("inner.conf"),
        "set-buffer -b inner parsed\nnew-window -Q\n",
    );

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.parse_only = true;

    let Response::Error(response) = handler.handle(Request::SourceFile(request)).await else {
        panic!("expected source-file -n to reject nested invalid command flags");
    };
    assert!(
        response.error.to_string().contains("inner.conf:2:")
            && response
                .error
                .to_string()
                .contains("command new-window: unknown flag -Q"),
        "{}",
        response.error
    );
    for name in ["inner", "outer"] {
        assert!(matches!(
            handler
                .handle(Request::ShowBuffer(ShowBufferRequest {
                    name: Some(name.to_owned()),
                }))
                .await,
            Response::Error(_)
        ));
    }
}

#[tokio::test]
async fn source_file_parse_only_aggregates_command_validation_errors() {
    let handler = RequestHandler::new();
    let root = temp_root("parse-only-aggregate-errors");
    write_config(
        &root.join("main.conf"),
        "new-window -Q\nserver-access --help\n",
    );

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.parse_only = true;

    let Response::Error(response) = handler.handle(Request::SourceFile(request)).await else {
        panic!("expected source-file -n to aggregate invalid command flags");
    };
    let message = response.error.to_string();
    assert!(
        message.contains("main.conf:1: command new-window: unknown flag -Q"),
        "{message}"
    );
    assert!(
        message.contains("main.conf:2: command server-access: unknown flag --help"),
        "{message}"
    );
}

#[tokio::test]
async fn source_file_parse_only_validates_nested_command_blocks() {
    let handler = RequestHandler::new();
    let root = temp_root("parse-only-command-block");
    write_config(
        &root.join("main.conf"),
        "if-shell -F 1 { new-window -Q }\nset-buffer -b after parsed\n",
    );

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.parse_only = true;

    let Response::Error(response) = handler.handle(Request::SourceFile(request)).await else {
        panic!("expected source-file -n to reject invalid command inside block");
    };
    assert!(
        response
            .error
            .to_string()
            .contains("main.conf:1: command new-window: unknown flag -Q"),
        "{}",
        response.error
    );
    assert!(matches!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("after".to_owned()),
            }))
            .await,
        Response::Error(_)
    ));
}

#[tokio::test]
async fn source_file_parse_only_validates_embedded_binding_and_hook_commands() {
    let handler = RequestHandler::new();
    let root = temp_root("parse-only-embedded-commands");
    write_config(
        &root.join("main.conf"),
        "bind-key X { new-window -Q }\nset-hook -g after-new-session { server-access --help }\n",
    );

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.parse_only = true;

    let Response::Error(response) = handler.handle(Request::SourceFile(request)).await else {
        panic!("expected source-file -n to reject invalid embedded commands");
    };
    let message = response.error.to_string();
    assert!(
        message.contains("main.conf:1: command new-window: unknown flag -Q"),
        "{message}"
    );
    assert!(
        message.contains("main.conf:1: command server-access: unknown flag --help"),
        "{message}"
    );
}

#[tokio::test]
async fn source_file_parse_only_preserves_bind_key_quoted_semicolons() {
    let handler = RequestHandler::new();
    let root = temp_root("parse-only-bind-key-quoted-semicolon");
    write_config(
        &root.join("main.conf"),
        "bind-key X display-message \"foo; new-window -Q\"\n",
    );

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.parse_only = true;

    assert_eq!(
        handler.handle(Request::SourceFile(request)).await,
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
    );
}

#[tokio::test]
async fn source_file_parse_only_rejects_server_access_help_and_bare_dash() {
    let handler = RequestHandler::new();
    let root = temp_root("parse-only-server-access-flags");
    write_config(
        &root.join("main.conf"),
        "server-access --help\nserver-access -\n",
    );

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.parse_only = true;

    let Response::Error(response) = handler.handle(Request::SourceFile(request)).await else {
        panic!("expected source-file -n to reject invalid server-access flags");
    };
    let message = response.error.to_string();
    assert!(
        message.contains("main.conf:1: command server-access: unknown flag --help"),
        "{message}"
    );
    assert!(
        message.contains("main.conf:2: command server-access: invalid flag -"),
        "{message}"
    );
}

#[tokio::test]
async fn source_file_quiet_suppresses_missing_file_and_glob_miss() {
    let handler = RequestHandler::new();
    let root = temp_root("quiet");
    fs::create_dir_all(&root).expect("quiet temp root");

    let mut request = match source_file_request(vec!["missing*.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.quiet = true;

    assert_eq!(
        handler.handle(Request::SourceFile(request)).await,
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
    );
}

#[tokio::test]
async fn source_file_format_expands_path_against_target_context() {
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

    let root = temp_root("format-path");
    let config = root.join("alpha.conf");
    write_config(&config, "set-buffer -b formatted ok\n");
    let response = handler
        .handle(Request::SourceFile(SourceFileRequest {
            paths: vec![format!("{}/#{{session_name}}.conf", root.display())],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: true,
            target: Some(PaneTarget::with_window(alpha, 0, 0)),
            caller_cwd: None,
            stdin: None,
        }))
        .await;

    assert_eq!(
        response,
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("formatted".to_owned()),
            }))
            .await
            .command_output()
            .expect("formatted buffer output")
            .stdout(),
        b"ok"
    );
}

#[tokio::test]
async fn source_file_if_condition_uses_target_format_context_at_parse_time() {
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

    let root = temp_root("if-target-format");
    write_config(
        &root.join("target.conf"),
        "%if #{session_name}\nset-buffer -b parse-target yes\n%else\nset-buffer -b parse-target no\n%endif\n",
    );

    let response = handler
        .handle(Request::SourceFile(SourceFileRequest {
            paths: vec!["target.conf".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: Some(PaneTarget::with_window(alpha, 0, 0)),
            caller_cwd: Some(root),
            stdin: None,
        }))
        .await;

    assert_eq!(
        response,
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("parse-target".to_owned()),
            }))
            .await
            .command_output()
            .expect("parse-target buffer output")
            .stdout(),
        b"yes"
    );
}

#[tokio::test]
async fn nested_source_file_format_expansion_sees_current_file() {
    let handler = RequestHandler::new();
    let root = temp_root("nested-current-file");
    let config = root.join("main.conf");
    let nested = root.join("main.conf.next");
    write_config(&config, "source-file -F '#{current_file}.next'\n");
    write_config(&nested, "set-buffer -b current-file ok\n");

    let response = handler
        .handle(source_file_request(
            vec!["main.conf".to_owned()],
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
                name: Some("current-file".to_owned()),
            }))
            .await
            .command_output()
            .expect("current-file buffer output")
            .stdout(),
        b"ok"
    );
}

#[tokio::test]
async fn nested_source_file_format_path_inherits_current_target() {
    let handler = RequestHandler::new();
    let session = session_name("s");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let root = temp_root("nested-format-option-path");
    write_config(
        &root.join("main.conf"),
        "set -g @name s\nsource-file -F '#{@name}.conf'\n",
    );
    write_config(&root.join("s.conf"), "set-buffer -b nested-target ok\n");

    let response = handler
        .handle(source_file_request(
            vec!["main.conf".to_owned()],
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
                name: Some("nested-target".to_owned()),
            }))
            .await
            .command_output()
            .expect("nested-target buffer output")
            .stdout(),
        b"ok"
    );
}

#[tokio::test]
async fn nested_source_file_preserves_implicit_target_canfail_behavior() {
    let handler = RequestHandler::new();
    for session in [session_name("alpha"), session_name("beta")] {
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session,
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
    }

    let root = temp_root("nested-source-implicit-canfail");
    write_config(&root.join("main.conf"), "source-file inner.conf\n");
    write_config(
        &root.join("inner.conf"),
        "display-message -p -t nosuch '#{session_name}:#{window_index}.#{pane_index}'\n",
    );

    let response = handler
        .handle(source_file_request(
            vec!["main.conf".to_owned()],
            Some(root),
        ))
        .await;

    assert_eq!(
        response
            .command_output()
            .expect("nested source-file output")
            .stdout(),
        b":.\n"
    );
}

#[tokio::test]
async fn source_file_nested_limit_reports_too_many_nested_files() {
    let handler = RequestHandler::new();
    let root = temp_root("nested-limit");
    let config = root.join("loop.conf");
    write_config(&config, "source-file loop.conf\n");

    let response = handler
        .handle(source_file_request(
            vec!["loop.conf".to_owned()],
            Some(root),
        ))
        .await;

    let Response::Error(error) = response else {
        panic!("source-file should report recursion limit, got {response:?}");
    };
    assert!(
        error.error.to_string().contains("too many nested files"),
        "unexpected error: {:?}",
        error
    );
}

#[tokio::test]
async fn source_file_non_quiet_rejects_empty_glob_pattern() {
    let handler = RequestHandler::new();
    let root = temp_root("empty-glob");
    fs::create_dir_all(&root).expect("create temp root");

    let response = handler
        .handle(source_file_request(
            vec!["nonexistent*.conf".to_owned()],
            Some(root),
        ))
        .await;

    let Response::Error(error) = response else {
        panic!("source-file should report empty glob, got {response:?}");
    };
    assert!(
        error.error.to_string().contains("nonexistent*.conf"),
        "unexpected error: {:?}",
        error
    );
}

#[tokio::test]
async fn source_file_multiple_paths_loads_all_in_order() {
    let handler = RequestHandler::new();
    let root = temp_root("multi-path");
    write_config(&root.join("a.conf"), "set-buffer -b multi first\n");
    write_config(&root.join("b.conf"), "set-buffer -b multi second\n");

    let response = handler
        .handle(source_file_request(
            vec!["a.conf".to_owned(), "b.conf".to_owned()],
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
                name: Some("multi".to_owned()),
            }))
            .await
            .command_output()
            .expect("multi buffer output")
            .stdout(),
        b"second"
    );
}

#[tokio::test]
async fn source_file_continues_after_missing_paths_and_reports_one_clean_error_prefix() {
    let handler = RequestHandler::new();
    let root = temp_root("multi-path-missing");
    write_config(&root.join("a.conf"), "set-buffer -b multi first\n");
    write_config(&root.join("b.conf"), "set-buffer -b multi second\n");

    let response = handler
        .handle(source_file_request(
            vec![
                "a.conf".to_owned(),
                "missing-a.conf".to_owned(),
                "b.conf".to_owned(),
                "missing-b.conf".to_owned(),
            ],
            Some(root),
        ))
        .await;

    let Response::Error(error) = response else {
        panic!("source-file should report missing paths, got {response:?}");
    };
    assert!(
        matches!(
            error.error,
            rmux_proto::RmuxError::Server(ref message)
                if message == "missing-a.conf: No such file or directory\nmissing-b.conf: No such file or directory"
        ),
        "unexpected missing-path error: {error:?}"
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("multi".to_owned()),
            }))
            .await
            .command_output()
            .expect("multi buffer output")
            .stdout(),
        b"second"
    );
}

#[tokio::test]
async fn source_file_continues_after_runtime_errors_and_reports_error() {
    let handler = RequestHandler::new();
    let root = temp_root("runtime-error-continues");
    write_config(
        &root.join("runtime.conf"),
        "source-file /definitely/missing.conf\ndisplay-message -p after\nset-option -g @after_runtime yes\n",
    );

    let response = handler
        .handle(source_file_request(
            vec!["runtime.conf".to_owned()],
            Some(root),
        ))
        .await;

    let Response::SourceFile(response) = response else {
        panic!("source-file should report nested runtime error, got {response:?}");
    };
    assert_eq!(response.exit_status(), Some(1));
    let output = response
        .command_output()
        .expect("source-file should preserve later stdout")
        .stdout();
    assert!(
        String::from_utf8_lossy(output).contains("after\n"),
        "source-file should preserve later stdout, got {}",
        String::from_utf8_lossy(output)
    );
    assert!(
        String::from_utf8_lossy(output).contains("definitely/missing.conf"),
        "source-file should keep runtime error visible, got {}",
        String::from_utf8_lossy(output)
    );
    assert_eq!(
        handler
            .handle(Request::ShowOptions(rmux_proto::ShowOptionsRequest {
                scope: OptionScopeSelector::SessionGlobal,
                name: Some("@after_runtime".to_owned()),
                value_only: true,
                include_inherited: false,
                quiet: false,
            }))
            .await
            .command_output()
            .expect("show-options output")
            .stdout(),
        b"yes\n"
    );
}
