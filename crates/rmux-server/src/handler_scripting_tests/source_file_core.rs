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
async fn source_file_parse_only_verbose_uses_tmux37_end_lines_for_multiline_strings() {
    let handler = RequestHandler::new();
    let root = temp_root("parse-only-multiline-lines");
    let config = root.join("main.conf");
    write_config(&config, "display-message -p \"a\nb\"\nset -g @after yes\n");

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.parse_only = true;
    request.verbose = true;

    let Response::SourceFile(response) = handler.handle(Request::SourceFile(request)).await else {
        panic!("expected source-file -n -v to return verbose output");
    };
    let stdout = response
        .command_output()
        .expect("parse-only verbose output")
        .stdout();
    assert_eq!(
        std::str::from_utf8(stdout).expect("verbose output is UTF-8"),
        format!(
            "{}:2: display-message -p a\\nb\n{}:3: set-option -g @after yes\n",
            config.display(),
            config.display()
        )
    );
}

#[tokio::test]
async fn source_file_parse_only_validation_errors_use_multiline_command_end_line() {
    let handler = RequestHandler::new();
    let root = temp_root("parse-only-multiline-error-line");
    write_config(
        &root.join("main.conf"),
        "new-window -Q \"x\ny\"\nset -g @after yes\n",
    );

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.parse_only = true;

    let Response::Error(error) = handler.handle(Request::SourceFile(request)).await else {
        panic!("expected source-file -n to reject invalid flag");
    };
    assert!(
        error
            .error
            .to_string()
            .contains("main.conf:2: command new-window: unknown flag -Q"),
        "{}",
        error.error
    );
}

#[tokio::test]
async fn source_file_unquoted_percent_word_is_fatal_syntax_error() {
    let handler = RequestHandler::new();
    let root = temp_root("percent-word-syntax");
    write_config(&root.join("main.conf"), "%word\nset-buffer -b after yes\n");

    let Response::Error(error) = handler
        .handle(source_file_request(
            vec!["main.conf".to_owned()],
            Some(root.clone()),
        ))
        .await
    else {
        panic!("source-file should reject unquoted percent word");
    };
    assert!(
        error
            .error
            .to_string()
            .contains("main.conf:1: syntax error"),
        "unexpected error: {:?}",
        error
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
async fn source_file_execute_verbose_reports_lookup_prefix_without_running_bad_file() {
    let handler = RequestHandler::new();
    let root = temp_root("execute-verbose-lookup-stop");
    let config = root.join("main.conf");
    write_config(
        &config,
        "set-buffer -b before yes\nbogus\nset-buffer -b after yes\n",
    );

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.verbose = true;

    let Response::SourceFile(response) = handler.handle(Request::SourceFile(request)).await else {
        panic!("source-file -v should return verbose output plus parse error");
    };
    assert_eq!(response.exit_status(), Some(1));
    assert_eq!(
        response
            .command_output()
            .expect("source-file -v output")
            .stdout(),
        format!(
            "{}:1: set-buffer -b before yes\n{}:2: unknown command: bogus\n",
            config.display(),
            config.display()
        )
        .as_bytes()
    );

    for name in ["before", "after"] {
        assert!(
            matches!(
                handler
                    .handle(Request::ShowBuffer(ShowBufferRequest {
                        name: Some(name.to_owned()),
                    }))
                    .await,
                Response::Error(_)
            ),
            "{name} should not run from a file with a lookup parse error"
        );
    }
}

#[tokio::test]
async fn source_file_verbose_execution_errors_go_to_stderr() {
    let handler = RequestHandler::new();
    let root = temp_root("execute-verbose-stderr");
    let config = root.join("main.conf");
    write_config(&config, "set-option -g xyzzy on\n");

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.verbose = true;

    let Response::SourceFile(response) = handler.handle(Request::SourceFile(request)).await else {
        panic!("source-file -v should return verbose output plus execution stderr");
    };
    assert_eq!(response.exit_status(), Some(1));
    assert_eq!(
        response
            .command_output()
            .expect("source-file -v output")
            .stdout(),
        format!("{}:1: set-option -g xyzzy on\n", config.display()).as_bytes()
    );
    assert_eq!(
        std::str::from_utf8(response.stderr()).expect("stderr is UTF-8"),
        "invalid option: xyzzy\n"
    );
}

#[tokio::test]
async fn source_file_read_diagnostics_do_not_move_execution_errors_to_stdout() {
    let handler = RequestHandler::new();
    let root = temp_root("read-diagnostic-exec-stderr");
    fs::create_dir_all(root.join("adir")).expect("create directory entry");
    write_config(&root.join("b.conf"), "set-option -g xyzzy on\n");

    let response = handler
        .handle(source_file_request(
            vec!["*".to_owned()],
            Some(root.clone()),
        ))
        .await;

    let Response::SourceFile(response) = response else {
        panic!("source-file should return stderr diagnostics, got {response:?}");
    };
    assert_eq!(response.exit_status(), Some(1));
    assert!(
        response.command_output().is_none(),
        "read + execution diagnostics must not spill to stdout"
    );
    let stderr = std::str::from_utf8(response.stderr()).expect("stderr is UTF-8");
    assert!(
        stderr.contains("Input/output error"),
        "stderr should include directory read diagnostic: {stderr:?}"
    );
    assert!(
        stderr.contains("invalid option: xyzzy"),
        "stderr should include execution error: {stderr:?}"
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
async fn source_file_parse_only_does_not_load_nested_source_files() {
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

    assert_eq!(
        handler.handle(Request::SourceFile(request)).await,
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
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
async fn source_file_parse_only_does_not_load_if_shell_nested_source_files() {
    let handler = RequestHandler::new();
    let root = temp_root("parse-only-if-shell-nested-source");
    let missing = root.join("missing.conf");
    let missing = missing.display().to_string().replace('\\', "/");
    write_config(
        &root.join("main.conf"),
        &format!(
            "if-shell \"[ -f {} ]\" \"source-file {}\"\ndisplay-message -p after\n",
            missing, missing
        ),
    );

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.parse_only = true;
    request.verbose = true;

    let response = handler.handle(Request::SourceFile(request)).await;
    let output = response
        .command_output()
        .expect("parse-only verbose output");
    let stdout = std::str::from_utf8(output.stdout()).expect("verbose output is UTF-8");
    assert!(stdout.contains("if-shell"), "{stdout}");
    assert!(stdout.contains("display-message -p after"), "{stdout}");
}

#[tokio::test]
async fn source_file_parse_only_stops_at_first_command_validation_error() {
    let handler = RequestHandler::new();
    let root = temp_root("parse-only-first-error");
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
        panic!("expected source-file -n to reject the first invalid command flag");
    };
    let message = response.error.to_string();
    assert!(
        message.contains("main.conf:1: command new-window: unknown flag -Q"),
        "{message}"
    );
    assert!(
        !message.contains("server-access"),
        "source-file -n should stop at the first validation error like tmux; got {message}"
    );
}

#[tokio::test]
async fn source_file_parse_only_verbose_omits_commands_after_first_error() {
    let handler = RequestHandler::new();
    let root = temp_root("parse-only-verbose-first-error");
    write_config(
        &root.join("main.conf"),
        "set -g @before yes\nnew-window -Q\nset -g @after yes\n",
    );

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.parse_only = true;
    request.verbose = true;

    let Response::SourceFile(response) = handler.handle(Request::SourceFile(request)).await else {
        panic!("expected source-file -n -v to return tmux-style stdout");
    };
    assert_eq!(response.exit_status(), Some(1));
    let stdout = response
        .command_output()
        .expect("parse-only verbose output")
        .stdout();
    let stdout = std::str::from_utf8(stdout).expect("verbose output is UTF-8");
    assert!(
        stdout.contains("main.conf:1: set-option -g @before yes"),
        "{stdout}"
    );
    assert!(
        stdout.contains("main.conf:2: command new-window: unknown flag -Q"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("@after"),
        "source-file -n -v should not print commands after the first error; got {stdout}"
    );
}

#[tokio::test]
async fn source_file_parse_only_verbose_omits_commands_after_first_parse_error() {
    let handler = RequestHandler::new();
    let root = temp_root("parse-only-verbose-first-parse-error");
    write_config(
        &root.join("main.conf"),
        "set -g @before yes\nbogus\nset -g @after yes\n",
    );

    let mut request = match source_file_request(vec!["main.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.parse_only = true;
    request.verbose = true;

    let Response::SourceFile(response) = handler.handle(Request::SourceFile(request)).await else {
        panic!("expected source-file -n -v to return tmux-style stdout");
    };
    assert_eq!(response.exit_status(), Some(1));
    let stdout = response
        .command_output()
        .expect("parse-only verbose output")
        .stdout();
    let stdout = std::str::from_utf8(stdout).expect("verbose output is UTF-8");
    assert!(
        stdout.contains("main.conf:1: set-option -g @before yes"),
        "{stdout}"
    );
    assert!(
        stdout.contains("main.conf:2: unknown command: bogus"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("@after"),
        "source-file -n -v should not print commands after the first parse error; got {stdout}"
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
        !message.contains("server-access"),
        "source-file -n should stop at the first embedded validation error like tmux; got {message}"
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
        !message.contains("invalid flag -"),
        "source-file -n should stop at the first server-access flag error like tmux; got {message}"
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
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec![format!("{}/#{{session_name}}.conf", root.display())],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: true,
            target: Some(PaneTarget::with_window(alpha, 0, 0)),
            caller_cwd: None,
            stdin: None,
        })))
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
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec!["target.conf".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: Some(PaneTarget::with_window(alpha, 0, 0)),
            caller_cwd: Some(root),
            stdin: None,
        })))
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
async fn queued_source_file_accepts_compact_format_target_with_attached_value() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    for session in [&alpha, &beta] {
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
    }

    let root = temp_root("source-file-compact-format-target");
    write_config(
        &root.join("beta.conf"),
        "display-message -p '#{session_name}'\nset-buffer -b compact-source ok\n",
    );
    let parsed = CommandParser::new()
        .parse("source-file -Ftbeta:0.0 '#{session_name}.conf'")
        .expect("source-file compact target parses");
    let output = handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::new(Some(root.clone()))
                .with_current_target(Some(Target::Session(alpha))),
        )
        .await
        .expect("source-file compact target should execute");

    assert_eq!(output.stdout(), b"beta\n");
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("compact-source".to_owned()),
            }))
            .await
            .command_output()
            .expect("compact-source buffer output")
            .stdout(),
        b"ok"
    );

    let _ = fs::remove_dir_all(root);
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

    let Response::SourceFile(response) = response else {
        panic!("source-file should report recursion limit on stderr, got {response:?}");
    };
    assert_eq!(response.exit_status(), Some(1));
    let error = std::str::from_utf8(response.stderr()).expect("stderr is UTF-8");
    assert!(
        error.contains("too many nested files"),
        "unexpected error: {}",
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

    let Response::SourceFile(response) = response else {
        panic!("source-file should report empty glob, got {response:?}");
    };
    assert_eq!(response.exit_status(), Some(1));
    assert!(
        std::str::from_utf8(response.stderr())
            .expect("stderr is UTF-8")
            .contains("nonexistent*.conf"),
        "unexpected stderr: {:?}",
        response.stderr()
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
async fn source_file_glob_reports_directories_after_loading_regular_files() {
    let handler = RequestHandler::new();
    let root = temp_root("glob-directory-error");
    fs::create_dir_all(root.join("nested")).expect("create nested directory");
    write_config(&root.join("a.conf"), "set-buffer -b glob first\n");
    write_config(&root.join("b.conf"), "set-buffer -b glob second\n");

    let response = handler
        .handle(source_file_request(
            vec!["*".to_owned()],
            Some(root.clone()),
        ))
        .await;

    let Response::SourceFile(response) = response else {
        panic!("source-file glob should report the matched directory, got {response:?}");
    };
    assert_eq!(response.exit_status(), Some(1));
    assert!(
        std::str::from_utf8(response.stderr())
            .expect("stderr is UTF-8")
            .contains("Input/output error"),
        "unexpected glob stderr: {:?}",
        response.stderr()
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("glob".to_owned()),
            }))
            .await
            .command_output()
            .expect("glob buffer output")
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

    let Response::SourceFile(response) = response else {
        panic!("source-file should report missing paths, got {response:?}");
    };
    assert_eq!(response.exit_status(), Some(1));
    assert_eq!(
        std::str::from_utf8(response.stderr()).expect("stderr is UTF-8"),
        "No such file or directory: missing-a.conf\nNo such file or directory: missing-b.conf\n",
        "unexpected missing-path stderr"
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
    let stderr = String::from_utf8_lossy(response.stderr());
    assert!(
        stderr.contains("definitely/missing.conf"),
        "source-file should keep runtime error visible on stderr, got {stderr}"
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

#[tokio::test]
async fn source_file_continues_after_non_quiet_legacy_option_lookup_errors() {
    let handler = RequestHandler::new();
    let root = temp_root("non-quiet-legacy-options");
    write_config(
        &root.join("legacy.conf"),
        "set -g @before_legacy_error yes\n\
         set -g status-utf8 on\n\
         set -g @after_legacy_error yes\n",
    );

    let response = handler
        .handle(source_file_request(
            vec!["legacy.conf".to_owned()],
            Some(root),
        ))
        .await;

    let Response::SourceFile(response) = response else {
        panic!("non-quiet legacy option should report stderr, got {response:?}");
    };
    assert_eq!(response.exit_status(), Some(1));
    let error = std::str::from_utf8(response.stderr()).expect("stderr is UTF-8");
    assert!(error.contains("invalid option: status-utf8"), "{}", error);

    for name in ["@before_legacy_error", "@after_legacy_error"] {
        assert_eq!(
            handler
                .handle(Request::ShowOptions(rmux_proto::ShowOptionsRequest {
                    scope: OptionScopeSelector::SessionGlobal,
                    name: Some(name.to_owned()),
                    value_only: true,
                    include_inherited: false,
                    quiet: false,
                }))
                .await
                .command_output()
                .expect("show-options output")
                .stdout(),
            b"yes\n",
            "{name} should remain applied after a recoverable source-file option lookup error"
        );
    }
}

#[tokio::test]
async fn source_file_set_option_quiet_ignores_legacy_option_lookup_errors() {
    let handler = RequestHandler::new();
    let root = temp_root("quiet-legacy-options");
    write_config(
        &root.join("legacy.conf"),
        "set -q -g status-utf8 on\n\
         set -gq utf8 on\n\
         setw -qg utf8 on\n\
         set -qg status-utf8 on\n\
         set -g base-index 1\n\
         setw -g pane-base-index 1\n",
    );

    let response = handler
        .handle(source_file_request(
            vec!["legacy.conf".to_owned()],
            Some(root),
        ))
        .await;

    assert_eq!(
        response,
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
    );
    let state = handler.state.lock().await;
    assert_eq!(state.options.global_value(OptionName::BaseIndex), Some("1"));
    assert_eq!(
        state.options.global_value(OptionName::PaneBaseIndex),
        Some("1")
    );
    assert!(
        state.message_log.iter().all(|entry| {
            !entry.msg.contains("status-utf8") && !entry.msg.contains("invalid option: utf8")
        }),
        "quiet legacy option lookups should not leak into show-messages: {:?}",
        state
            .message_log
            .iter()
            .map(|entry| entry.msg.as_str())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn source_file_set_option_quiet_does_not_suppress_bad_values() {
    let handler = RequestHandler::new();
    let root = temp_root("quiet-bad-value");
    write_config(
        &root.join("bad-value.conf"),
        "set -q -g status maybe\nset -g base-index 1\n",
    );

    let response = handler
        .handle(source_file_request(
            vec!["bad-value.conf".to_owned()],
            Some(root),
        ))
        .await;

    let Response::SourceFile(response) = response else {
        panic!("bad option value should remain stderr, got {response:?}");
    };
    assert_eq!(response.exit_status(), Some(1));
    let error = std::str::from_utf8(response.stderr()).expect("stderr is UTF-8");
    assert!(error.contains("unknown value: maybe"), "{}", error);
    let state = handler.state.lock().await;
    assert_eq!(
        state.options.global_value(OptionName::BaseIndex),
        Some("1"),
        "later commands should still run after a recoverable command error"
    );
}
