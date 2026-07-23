use super::*;
use crate::pane_io::AttachControl;

#[tokio::test]
async fn compact_short_options_execute_from_source_file() {
    let handler = RequestHandler::new();
    let root = temp_root("compact-server-flags");
    write_config(
        &root.join("compact.conf"),
        "show-messages -JT\nset-buffer -b compact-source ok\n",
    );

    let response = handler
        .handle(source_file_request(
            vec!["compact.conf".to_owned()],
            Some(root.clone()),
        ))
        .await;

    assert!(matches!(response, Response::SourceFile(_)), "{response:?}");
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("compact-source".to_owned()),
            }))
            .await
            .command_output()
            .expect("source command after compact flags should execute")
            .stdout(),
        b"ok"
    );
    fs::remove_dir_all(root).expect("remove compact source root");
}

#[tokio::test]
async fn compact_hidden_select_pane_style_executes_from_source_file() {
    let handler = RequestHandler::new();
    let alpha = session_name("source-compact-select-style");
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
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(alpha.clone(), 0, 0)),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));

    let root = temp_root("compact-hidden-select-style");
    write_config(
        &root.join("select.conf"),
        "select-pane -tsource-compact-select-style:0.1 -Pfg=blue,bg=red\n",
    );
    let response = handler
        .handle(source_file_request(
            vec!["select.conf".to_owned()],
            Some(root.clone()),
        ))
        .await;
    fs::remove_dir_all(root).expect("remove compact select source root");

    assert!(matches!(response, Response::SourceFile(_)), "{response:?}");
    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session
            .window_at(0)
            .expect("window exists")
            .active_pane_index(),
        1
    );
    assert_eq!(
        state.options.pane_value(
            &PaneTarget::with_window(alpha, 0, 1),
            OptionName::WindowStyle
        ),
        Some("fg=blue,bg=red")
    );
}

#[tokio::test]
async fn source_file_rejects_unimplemented_display_message_flags() {
    let handler = RequestHandler::new();
    let root = temp_root("display-message-inert-flags");
    write_config(&root.join("display.conf"), "display-message -d0 -p hello\n");

    let response = handler
        .handle(source_file_request(
            vec!["display.conf".to_owned()],
            Some(root.clone()),
        ))
        .await;
    fs::remove_dir_all(root).expect("remove display-message source root");

    let diagnostic = source_file_stdout_failure(response);
    assert!(
        diagnostic.contains("display.conf:1: command display-message: unknown flag -d"),
        "unexpected source diagnostic: {diagnostic}"
    );
}

#[tokio::test]
async fn source_file_command_bounds_matches_across_separate_paths() {
    let handler = RequestHandler::new();
    let root = temp_root("aggregate-path-count");
    let mut paths = Vec::new();
    for index in 0..=256 {
        let name = format!("{index:04}.conf");
        write_config(&root.join(&name), "");
        paths.push(name);
    }

    let response = handler
        .handle(source_file_request(paths, Some(root.clone())))
        .await;
    fs::remove_dir_all(root).expect("remove aggregate path root");

    let Response::SourceFile(response) = response else {
        panic!("aggregate source read limit should be a source-file diagnostic: {response:?}");
    };
    assert_eq!(response.exit_status(), Some(1));
    let stderr = std::str::from_utf8(response.stderr()).expect("source diagnostic is UTF-8");
    assert!(
        stderr.contains("exceeds 256 matched files"),
        "unexpected source diagnostic: {stderr:?}"
    );
}

#[tokio::test]
async fn source_file_preserves_target_client_and_show_hooks_flags() {
    let handler = RequestHandler::new();
    let alpha = session_name("source-target-client");
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
    let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler.register_attach(202, alpha, control_tx).await;
    while control_rx.try_recv().is_ok() {}

    let root = temp_root("target-client-flags");
    write_config(&root.join("input.txt"), "loaded from source");
    write_config(
        &root.join("flags.conf"),
        "set-buffer -b set-from-source -t 202 payload\n\
         load-buffer -b loaded-from-source -t 202 input.txt\n\
         show-options -gH after-load-buffer\n\
         display-panes -b -d 5000 -t 202\n",
    );

    let response = handler
        .handle(source_file_request(
            vec!["flags.conf".to_owned()],
            Some(root.clone()),
        ))
        .await;
    let output = response
        .command_output()
        .expect("source-file command output");
    assert_eq!(output.stdout(), b"after-load-buffer\n");
    assert!(matches!(
        control_rx.try_recv(),
        Ok(AttachControl::Overlay(_))
    ));

    for (name, expected) in [
        ("set-from-source", b"payload".as_slice()),
        ("loaded-from-source", b"loaded from source".as_slice()),
    ] {
        assert_eq!(
            handler
                .handle(Request::ShowBuffer(ShowBufferRequest {
                    name: Some(name.to_owned()),
                }))
                .await
                .command_output()
                .expect("source-created buffer")
                .stdout(),
            expected
        );
    }
}

#[tokio::test]
async fn source_file_background_run_shell_preserves_its_implicit_target() {
    let handler = RequestHandler::new();
    let alpha = session_name("source-background-target-alpha");
    let beta = session_name("source-background-target-beta");
    let expected_window_name = "source-background-fixed-target";
    create_background_identity_session(&handler, alpha.clone()).await;

    let root = temp_root("background-implicit-target");
    write_config(
        &root.join("background.conf"),
        &format!("run-shell -b -d 0.2 -C 'rename-window {expected_window_name}'\n"),
    );
    let response = handler
        .handle(source_file_request(
            vec!["background.conf".to_owned()],
            Some(root.clone()),
        ))
        .await;
    assert!(matches!(response, Response::SourceFile(_)), "{response:?}");

    create_background_identity_session(&handler, beta.clone()).await;
    wait_for_active_window_name(&handler, &alpha, expected_window_name).await;
    let state = handler.state.lock().await;
    assert_ne!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(session.active_window_index()))
            .and_then(rmux_core::Window::name),
        Some(expected_window_name),
        "source-file background target must not drift to a newer preferred session"
    );
    drop(state);
    fs::remove_dir_all(root).expect("remove source background target root");
}

#[tokio::test]
async fn source_file_show_window_options_rejects_h() {
    let handler = RequestHandler::new();
    let root = temp_root("show-window-options-h");
    let config = root.join("bad.conf");
    write_config(&config, "show-window-options -H\n");

    let response = handler
        .handle(source_file_request(vec!["bad.conf".to_owned()], Some(root)))
        .await;
    assert_eq!(
        source_file_stdout_failure(response),
        format!(
            "{}:1: command show-window-options: unknown flag -H\n",
            config.display()
        )
    );
}

#[tokio::test]
async fn source_file_rejects_refresh_client_unsupported_fields() {
    let handler = RequestHandler::new();
    let root = temp_root("refresh-client-unsupported-fields");

    for (name, command, expected) in [
        (
            "a.conf",
            "refresh-client -A pane:on\n",
            "command refresh-client: unknown flag -A",
        ),
        (
            "b.conf",
            "refresh-client -B name:pane:format\n",
            "command refresh-client: unknown flag -B",
        ),
        (
            "r.conf",
            "refresh-client -r pane:rgb\n",
            "command refresh-client: unknown flag -r",
        ),
        (
            "c.conf",
            "refresh-client -c\n",
            "command refresh-client: unknown flag -c",
        ),
        (
            "d.conf",
            "refresh-client -D\n",
            "command refresh-client: unknown flag -D",
        ),
        (
            "l.conf",
            "refresh-client -L\n",
            "command refresh-client: unknown flag -L",
        ),
        (
            "rr.conf",
            "refresh-client -R\n",
            "command refresh-client: unknown flag -R",
        ),
        (
            "u.conf",
            "refresh-client -U\n",
            "command refresh-client: unknown flag -U",
        ),
        (
            "adjustment.conf",
            "refresh-client 10\n",
            "unexpected argument '10' for refresh-client",
        ),
    ] {
        let config = root.join(name);
        write_config(&config, command);
        let response = handler
            .handle(source_file_request(
                vec![name.to_owned()],
                Some(root.clone()),
            ))
            .await;
        assert_eq!(
            source_file_stdout_failure(response),
            format!("{}:1: {expected}\n", config.display()),
            "source-file should reject {command:?}"
        );
    }

    fs::remove_dir_all(root).expect("remove refresh-client source root");
}

#[tokio::test]
async fn source_file_rejects_unknown_options_before_command_tails() {
    for (label, command, expected_command, expected_flag) in [
        ("run-shell", "run-shell -Q true", "run-shell", "-Q"),
        (
            "if-shell",
            "if-shell -Q true { set-buffer -b branch bad }",
            "if-shell",
            "-Q",
        ),
        ("wait-for", "wait-for -Q", "wait-for", "-Q"),
        ("split-window", "split-window -Q true", "split-window", "-Q"),
        ("pipe-pane", "pipe-pane -Q true", "pipe-pane", "-Q"),
        (
            "respawn-pane",
            "respawn-pane -k -Q true",
            "respawn-pane",
            "-Q",
        ),
        (
            "bind-key",
            "bind-key -Q X { set-buffer -b binding bad }",
            "bind-key",
            "-Q",
        ),
        (
            "display-panes",
            "display-panes -Q 'select-pane -t %%'",
            "display-panes",
            "-Q",
        ),
    ] {
        let handler = RequestHandler::new();
        let root = temp_root(&format!("unknown-tail-{label}"));
        let config = root.join("main.conf");
        let canary = format!("after-{label}");
        write_config(
            &config,
            &format!("{command}\nset-buffer -b {canary} mutated\n"),
        );

        let response = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handler.handle(source_file_request(
                vec!["main.conf".to_owned()],
                Some(root.clone()),
            )),
        )
        .await
        .unwrap_or_else(|_| panic!("source-file {label} validation must not block"));

        assert_eq!(
            source_file_stdout_failure(response),
            format!(
                "{}:1: command {expected_command}: unknown flag {expected_flag}\n",
                config.display()
            ),
            "source-file accepted an unknown option before {label}'s positional tail"
        );
        assert!(
            matches!(
                handler
                    .handle(Request::ShowBuffer(ShowBufferRequest {
                        name: Some(canary),
                    }))
                    .await,
                Response::Error(_)
            ),
            "source-file executed commands after {label}'s validation error"
        );
        fs::remove_dir_all(root).expect("remove unknown-tail source root");
    }
}

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
async fn source_file_utf8_bom_is_not_stripped_like_tmux() {
    let handler = RequestHandler::new();
    let root = temp_root("utf8-bom-syntax");
    write_config(
        &root.join("main.conf"),
        "\u{feff}set-buffer -b bom yes\nset-buffer -b after yes\n",
    );

    let Response::Error(error) = handler
        .handle(source_file_request(
            vec!["main.conf".to_owned()],
            Some(root.clone()),
        ))
        .await
    else {
        panic!("source-file should reject BOM-prefixed command");
    };
    assert!(
        error
            .error
            .to_string()
            .contains("main.conf:1: unknown command:"),
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
async fn source_file_reversed_bom_is_not_a_read_error_or_valid_command() {
    let handler = RequestHandler::new();
    let root = temp_root("reversed-bom-syntax");
    fs::create_dir_all(&root).expect("config parent directory");
    fs::write(
        root.join("main.conf"),
        b"\xff\xfeset-buffer -b bom yes\nset-buffer -b after yes\n",
    )
    .expect("write config");

    let Response::Error(error) = handler
        .handle(source_file_request(
            vec!["main.conf".to_owned()],
            Some(root.clone()),
        ))
        .await
    else {
        panic!("source-file should reject reversed-BOM command");
    };
    let message = error.error.to_string();
    assert!(
        message.contains("main.conf:1: unknown command:"),
        "unexpected error: {message}"
    );
    assert!(
        !message.contains("stream did not contain valid UTF-8"),
        "source-file should parse lossy text like tmux, got {message}"
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
async fn internal_runtime_expansion_skips_source_only_flag_validation_without_executing() {
    let handler = RequestHandler::new();
    let request = SourceFileRequest {
        paths: vec![INTERNAL_RUNTIME_COMMAND_EXPANSION_PATH.to_owned()],
        quiet: false,
        parse_only: true,
        verbose: true,
        expand_paths: false,
        target: None,
        caller_cwd: None,
        stdin: Some(
            encode_internal_runtime_command_arguments(&[
                "set-environment".to_owned(),
                "-gh".to_owned(),
                "SECRET".to_owned(),
                "value".to_owned(),
            ])
            .expect("runtime argv serializes"),
        ),
    };

    let Response::SourceFile(response) =
        handler.handle(Request::SourceFile(Box::new(request))).await
    else {
        panic!("internal runtime expansion should return canonical output");
    };
    assert_eq!(response.exit_status(), None);
    assert_eq!(
        response
            .command_output()
            .expect("canonical output")
            .stdout(),
        b"set-environment -gh SECRET value"
    );
    assert!(matches!(
        handler
            .handle(Request::ShowEnvironment(ShowEnvironmentRequest {
                scope: ScopeSelector::Global,
                name: Some("SECRET".to_owned()),
                hidden: true,
                shell_format: false,
            }))
            .await,
        Response::Error(_)
    ));
}

#[tokio::test]
async fn internal_parse_time_assignments_apply_visible_and_hidden_values() {
    let handler = RequestHandler::new();
    let request = SourceFileRequest {
        paths: vec![INTERNAL_PARSE_TIME_ASSIGNMENTS_PATH.to_owned()],
        quiet: false,
        parse_only: false,
        verbose: false,
        expand_paths: false,
        target: None,
        caller_cwd: None,
        stdin: Some("FOO=bar ; %hidden SECRET=shh".to_owned()),
    };

    assert!(matches!(
        handler.handle(Request::SourceFile(Box::new(request))).await,
        Response::SourceFile(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::ShowEnvironment(ShowEnvironmentRequest {
                scope: ScopeSelector::Global,
                name: Some("FOO".to_owned()),
                hidden: false,
                shell_format: false,
            }))
            .await,
        Response::ShowEnvironment(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::ShowEnvironment(ShowEnvironmentRequest {
                scope: ScopeSelector::Global,
                name: Some("SECRET".to_owned()),
                hidden: true,
                shell_format: false,
            }))
            .await,
        Response::ShowEnvironment(_)
    ));
}

#[tokio::test]
async fn internal_parse_time_assignment_payload_rejects_commands_atomically() {
    let handler = RequestHandler::new();
    let request = SourceFileRequest {
        paths: vec![INTERNAL_PARSE_TIME_ASSIGNMENTS_PATH.to_owned()],
        quiet: false,
        parse_only: false,
        verbose: false,
        expand_paths: false,
        target: None,
        caller_cwd: None,
        stdin: Some("FOO=bar ; display-message no".to_owned()),
    };

    assert!(matches!(
        handler.handle(Request::SourceFile(Box::new(request))).await,
        Response::Error(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::ShowEnvironment(ShowEnvironmentRequest {
                scope: ScopeSelector::Global,
                name: Some("FOO".to_owned()),
                hidden: false,
                shell_format: false,
            }))
            .await,
        Response::Error(_)
    ));
}

#[tokio::test]
async fn mixed_or_unknown_internal_source_paths_fail_closed_without_execution() {
    let handler = RequestHandler::new();
    for reserved_path in [
        INTERNAL_RUNTIME_COMMAND_EXPANSION_PATH,
        INTERNAL_PARSE_TIME_ASSIGNMENTS_PATH,
        INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH,
        "\0rmux-unknown-internal-v1",
    ] {
        let response = handler
            .handle(Request::SourceFile(Box::new(SourceFileRequest {
                paths: vec![reserved_path.to_owned(), "-".to_owned()],
                quiet: false,
                parse_only: false,
                verbose: false,
                expand_paths: false,
                target: None,
                caller_cwd: None,
                stdin: Some("set-buffer -b internal-path-canary mutated".to_owned()),
            })))
            .await;
        assert!(matches!(response, Response::Error(_)), "{response:?}");
    }

    assert!(matches!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("internal-path-canary".to_owned()),
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
async fn queued_source_file_target_finder_uses_active_indexed_pane() {
    let handler = RequestHandler::new();
    for (option, value) in [
        (OptionName::BaseIndex, "3"),
        (OptionName::PaneBaseIndex, "4"),
    ] {
        assert!(matches!(
            handler
                .handle(Request::SetOption(SetOptionRequest {
                    scope: ScopeSelector::Global,
                    option,
                    value: value.to_owned(),
                    mode: SetOptionMode::Replace,
                }))
                .await,
            Response::SetOption(_)
        ));
    }

    let beta = session_name("beta");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: beta.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: beta.clone(),
                name: Some("active".to_owned()),
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
                target_window_index: Some(5),
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(beta.clone(), 5, 0)),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectWindow(rmux_proto::SelectWindowRequest {
                target: WindowTarget::with_window(beta.clone(), 5),
            }))
            .await,
        Response::SelectWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                target: PaneTarget::with_window(beta.clone(), 5, 1),
                title: None,
                style: None,
                input_disabled: None,
                preserve_zoom: false,
            })))
            .await,
        Response::SelectPane(_)
    ));

    let root = temp_root("source-file-active-indexed-pane");
    write_config(
        &root.join("target.conf"),
        "display-message -p '#{session_name}:#{window_index}.#{pane_index}'\n",
    );
    write_config(
        &root.join("outer.conf"),
        "source-file -t missing target.conf\n",
    );
    let parsed = CommandParser::new()
        .parse(
            "source-file -t beta target.conf; \
             source-file -tbeta:5 target.conf; \
             source-file -t beta:5.5 target.conf; \
             source-file -t beta:5.4 outer.conf",
        )
        .expect("source-file target matrix parses");
    let output = handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::new(Some(root.clone()))
                .with_current_target(Some(Target::Pane(PaneTarget::with_window(beta, 5, 0)))),
        )
        .await
        .expect("source-file target matrix should execute");

    assert_eq!(output.stdout(), b"beta:5.5\nbeta:5.5\nbeta:5.5\nbeta:5.5\n");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn queued_source_file_preserves_assignment_order_across_multi_paths() {
    let handler = RequestHandler::new();
    let root = temp_root("queued-multi-path-assignments-order");
    write_config(&root.join("first.conf"), "QUEUE_ORDER=from-first\n");
    write_config(
        &root.join("bad.conf"),
        "QUEUE_ORDER=from-bad\ndefinitely-not-a-command\nset-option -g @queued_must_not_run yes\n",
    );
    write_config(
        &root.join("third.conf"),
        "set-option -g @queued_order \"$QUEUE_ORDER\"\n",
    );
    let parsed = CommandParser::new()
        .parse("source-file first.conf bad.conf third.conf")
        .expect("queued source-file parses");

    let error = handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::new(Some(root.clone())),
        )
        .await
        .expect_err("queued source-file should retain the intermediate parse error");

    assert!(
        error
            .to_string()
            .contains("bad.conf:2: unknown command: definitely-not-a-command"),
        "expected queued source-file error, got {error}"
    );
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.environment.global_value("QUEUE_ORDER"),
            Some("from-bad"),
            "queued source-file assignments must apply in source order"
        );
    }
    assert_eq!(
        handler
            .handle(Request::ShowOptions(rmux_proto::ShowOptionsRequest {
                scope: OptionScopeSelector::SessionGlobal,
                name: Some("@queued_order".to_owned()),
                value_only: true,
                include_inherited: false,
                quiet: false,
                include_hooks: false,
            }))
            .await
            .command_output()
            .expect("queued source option output")
            .stdout(),
        b"from-bad\n"
    );
    assert!(
        handler
            .handle(Request::ShowOptions(rmux_proto::ShowOptionsRequest {
                scope: OptionScopeSelector::SessionGlobal,
                name: Some("@queued_must_not_run".to_owned()),
                value_only: true,
                include_inherited: false,
                quiet: true,
                include_hooks: false,
            }))
            .await
            .command_output()
            .expect("queued quiet show-options output")
            .stdout()
            .is_empty(),
        "commands after a fatal queued source-file parse error must not execute"
    );
    fs::remove_dir_all(root).expect("remove queued assignment config root");
}

#[tokio::test]
async fn queued_display_message_accepts_compact_print_and_commands_flags() {
    let handler = RequestHandler::new();
    let alpha = session_name("display-compact-pc");
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
        .parse("display-message -pC '#{session_name}'")
        .expect("display-message -pC parses");
    let output = handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Session(alpha))),
        )
        .await
        .expect("display-message -pC should execute");

    assert_eq!(output.stdout(), b"display-compact-pc\n");
}

#[tokio::test]
async fn source_file_set_window_option_alias_uses_explicit_window_target_metadata() {
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
    for (session, name) in [(&alpha, "one"), (&alpha, "named"), (&beta, "other")] {
        assert!(matches!(
            handler
                .handle(Request::NewWindow(Box::new(NewWindowRequest {
                    target: session.clone(),
                    name: Some(name.to_owned()),
                    detached: true,
                    start_directory: None,
                    environment: None,
                    command: None,
                    process_command: None,
                    target_window_index: None,
                    insert_at_target: false,
                })))
                .await,
            Response::NewWindow(_)
        ));
    }

    let root = temp_root("source-set-window-option-alias");
    write_config(
        &root.join("options.conf"),
        "set-window-option -t:+ aggressive-resize on\n\
         set-option -w -t:named synchronize-panes on\n",
    );
    let parsed = CommandParser::new()
        .parse("source-file -t alpha:0.0 options.conf")
        .expect("source-file target command parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::new(Some(root.clone())).with_current_target(Some(Target::Pane(
                PaneTarget::with_window(beta.clone(), 0, 0),
            ))),
        )
        .await
        .expect("source-file set-window-option alias should execute");

    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.options.window_value(
                &WindowTarget::with_window(alpha.clone(), 1),
                OptionName::AggressiveResize
            ),
            Some("on")
        );
        assert_eq!(
            state.options.window_value(
                &WindowTarget::with_window(alpha.clone(), 2),
                OptionName::SynchronizePanes
            ),
            Some("on")
        );
        assert_eq!(
            state.options.window_value(
                &WindowTarget::with_window(beta.clone(), 1),
                OptionName::AggressiveResize
            ),
            None
        );
    }

    write_config(
        &root.join("missing-alias.conf"),
        "set-window-option -t nosuch automatic-rename off\n",
    );
    write_config(
        &root.join("missing-canonical.conf"),
        "set-option -w -t nosuch automatic-rename off\n",
    );
    let missing_source_request = |path: &str| {
        Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec![path.to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: Some(PaneTarget::with_window(alpha.clone(), 0, 0)),
            caller_cwd: Some(root.clone()),
            stdin: None,
        }))
    };

    let Response::SourceFile(alias_response) = handler
        .handle(missing_source_request("missing-alias.conf"))
        .await
    else {
        panic!("missing alias target should return a source-file response");
    };
    let Response::SourceFile(canonical_response) = handler
        .handle(missing_source_request("missing-canonical.conf"))
        .await
    else {
        panic!("missing canonical target should return a source-file response");
    };
    assert_eq!(alias_response.exit_status(), Some(1));
    assert_eq!(canonical_response.exit_status(), Some(1));
    for stderr in [alias_response.stderr(), canonical_response.stderr()] {
        let stderr = std::str::from_utf8(stderr).expect("source-file stderr is UTF-8");
        assert!(stderr.contains("nosuch"), "{stderr}");
        assert!(stderr.contains("window"), "{stderr}");
    }

    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn source_file_resolves_announced_window_and_pane_target_metadata() {
    let handler = RequestHandler::new();
    let alpha = session_name("metadata-alpha");
    let beta = session_name("metadata-beta");
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
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: alpha.clone(),
                name: Some("metadata-logs".to_owned()),
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
                target_window_index: Some(1),
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(alpha.clone(), 0, 0)),
                direction: SplitDirection::Vertical,
                environment: None,
                before: false,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 1)),
                option: OptionName::AutomaticRename,
                value: "off".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id().to_string())
            .expect("linked window pane id exists")
    };
    let root = temp_root("source-announced-target-metadata");
    write_config(
        &root.join("metadata.conf"),
        &format!(
            "resize-window -t metadata-logs -x 91 -y 23\n\
             show-window-options -v -t {pane_id} automatic-rename\n\
             send-prefix -t{pane_id}\n\
             link-window -s metadata-alpha:1 -t metadata-beta:1\n\
             unlink-window -t :+\n"
        ),
    );
    let parsed = CommandParser::new()
        .parse("source-file -t metadata-alpha:0.0 metadata.conf")
        .expect("source-file command parses");
    let output = handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::new(Some(root.clone())),
        )
        .await
        .expect("source-file target metadata should resolve");
    assert_eq!(output.stdout(), b"off\n");

    let state = handler.state.lock().await;
    assert!(
        state
            .sessions
            .session(&alpha)
            .expect("alpha exists")
            .window_at(1)
            .is_none(),
        "relative unlink-window should remove alpha's linked slot"
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .expect("beta exists")
            .window_at(1)
            .expect("beta linked window survives")
            .size(),
        TerminalSize { cols: 91, rows: 23 }
    );
    drop(state);
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
            Some(root.clone()),
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
                include_hooks: false,
            }))
            .await
            .command_output()
            .expect("show-options output")
            .stdout(),
        b"yes\n"
    );
}

#[tokio::test]
async fn source_file_new_window_k_validates_environment_before_replacing_target() {
    let handler = RequestHandler::new();
    let alpha = session_name("source-new-window-k-env-validation");
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
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: alpha.clone(),
                name: Some("protected".to_owned()),
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
                target_window_index: Some(1),
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));
    let protected_window_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .and_then(|session| session.window_at(1))
        .expect("protected window exists")
        .id();

    let root = temp_root("new-window-k-env-validation");
    write_config(
        &root.join("invalid.conf"),
        "new-window -d -k -t source-new-window-k-env-validation:1 -e NOT_AN_ASSIGNMENT\n",
    );
    let response = handler
        .handle(source_file_request(
            vec!["invalid.conf".to_owned()],
            Some(root.clone()),
        ))
        .await;
    fs::remove_dir_all(root).expect("remove source-file test root");

    let Response::SourceFile(response) = response else {
        panic!("source-file should report invalid environment, got {response:?}");
    };
    assert_eq!(response.exit_status(), Some(1));
    assert!(
        String::from_utf8_lossy(response.stderr())
            .contains("environment assignment must be NAME=VALUE"),
        "{}",
        String::from_utf8_lossy(response.stderr())
    );
    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session survives");
    assert_eq!(session.windows().len(), 2);
    let protected = session
        .window_at(1)
        .expect("invalid replacement must preserve the target window");
    assert_eq!(protected.id(), protected_window_id);
    assert_eq!(protected.name(), Some("protected"));
}

#[tokio::test]
async fn source_file_multi_paths_carry_assignments_and_continue_after_file_error() {
    let handler = RequestHandler::new();
    let root = temp_root("multi-path-assignments-order");
    write_config(&root.join("first.conf"), "SOURCE_ORDER=from-first\n");
    write_config(
        &root.join("bad.conf"),
        "BROKEN_ORDER=from-bad\ndefinitely-not-a-command\nset-option -g @must_not_run yes\n",
    );
    write_config(
        &root.join("third.conf"),
        "set-option -g @source_order \"$SOURCE_ORDER\"\n\
         set-option -g @after_bad \"$BROKEN_ORDER\"\n",
    );

    let response = handler
        .handle(source_file_request(
            vec![
                "first.conf".to_owned(),
                "bad.conf".to_owned(),
                "third.conf".to_owned(),
            ],
            Some(root.clone()),
        ))
        .await;

    let Response::SourceFile(response) = response else {
        panic!("source-file should report the intermediate parse error, got {response:?}");
    };
    assert_eq!(response.exit_status(), Some(1));
    let mut diagnostics = String::new();
    if let Some(output) = response.command_output() {
        diagnostics.push_str(&String::from_utf8_lossy(output.stdout()));
    }
    diagnostics.push_str(&String::from_utf8_lossy(response.stderr()));
    assert!(
        diagnostics.contains("bad.conf:2: unknown command: definitely-not-a-command"),
        "source-file should report the bad file and line, got {diagnostics}"
    );
    for (name, value) in [
        ("@source_order", "from-first\n"),
        ("@after_bad", "from-bad\n"),
    ] {
        assert_eq!(
            handler
                .handle(Request::ShowOptions(rmux_proto::ShowOptionsRequest {
                    scope: OptionScopeSelector::SessionGlobal,
                    name: Some(name.to_owned()),
                    value_only: true,
                    include_inherited: false,
                    quiet: false,
                    include_hooks: false,
                }))
                .await
                .command_output()
                .expect("show-options output")
                .stdout(),
            value.as_bytes()
        );
    }
    assert!(
        handler
            .handle(Request::ShowOptions(rmux_proto::ShowOptionsRequest {
                scope: OptionScopeSelector::SessionGlobal,
                name: Some("@must_not_run".to_owned()),
                value_only: true,
                include_inherited: false,
                quiet: true,
                include_hooks: false,
            }))
            .await
            .command_output()
            .expect("quiet show-options output")
            .stdout()
            .is_empty(),
        "commands after a fatal parse error in the same file must not execute"
    );
    fs::remove_dir_all(root).expect("remove multi-path assignment config root");
}

#[tokio::test]
async fn source_file_sets_server_option_without_explicit_scope_or_target() {
    let handler = RequestHandler::new();
    let root = temp_root("server-option-no-target");
    write_config(&root.join("server.conf"), "set escape-time 77\n");

    let response = handler
        .handle(source_file_request(
            vec!["server.conf".to_owned()],
            Some(root),
        ))
        .await;

    let Response::SourceFile(response) = response else {
        panic!("source-file should accept server option without target, got {response:?}");
    };
    assert_eq!(response.exit_status(), None);
    assert!(response.stderr().is_empty());
    assert_eq!(
        handler
            .handle(Request::ShowOptions(rmux_proto::ShowOptionsRequest {
                scope: OptionScopeSelector::ServerGlobal,
                name: Some("escape-time".to_owned()),
                value_only: true,
                include_inherited: false,
                quiet: false,
                include_hooks: false,
            }))
            .await
            .command_output()
            .expect("show-options output")
            .stdout(),
        b"77\n"
    );
}

#[tokio::test]
async fn source_file_sets_bare_server_option_with_current_runtime_target() {
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
    let root = temp_root("server-option-current-target");
    write_config(
        &root.join("server.conf"),
        "set escape-time 77\nset -q escape-time 78\nset -g @after_runtime_escape yes\n",
    );
    let mut request = match source_file_request(vec!["server.conf".to_owned()], Some(root)) {
        Request::SourceFile(request) => request,
        _ => unreachable!("source file request"),
    };
    request.target = Some(PaneTarget::with_window(alpha, 0, 0));

    let response = handler.handle(Request::SourceFile(request)).await;

    let Response::SourceFile(response) = response else {
        panic!("source-file should accept server option with current target, got {response:?}");
    };
    assert_eq!(response.exit_status(), None);
    assert!(response.stderr().is_empty());
    assert_eq!(
        handler
            .handle(Request::ShowOptions(rmux_proto::ShowOptionsRequest {
                scope: OptionScopeSelector::ServerGlobal,
                name: Some("escape-time".to_owned()),
                value_only: true,
                include_inherited: false,
                quiet: false,
                include_hooks: false,
            }))
            .await
            .command_output()
            .expect("show-options output")
            .stdout(),
        b"78\n"
    );
    assert_eq!(
        handler
            .handle(Request::ShowOptions(rmux_proto::ShowOptionsRequest {
                scope: OptionScopeSelector::SessionGlobal,
                name: Some("@after_runtime_escape".to_owned()),
                value_only: true,
                include_inherited: false,
                quiet: false,
                include_hooks: false,
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
                    include_hooks: false,
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

#[tokio::test]
async fn source_file_grouped_new_window_insertion_preserves_and_arms_silence_timers() {
    let handler = RequestHandler::new();
    let owner =
        create_quiet_source_timer_session(&handler, "source-new-window-timer-owner", None).await;
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(owner.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    let peer = create_quiet_source_timer_session(
        &handler,
        "source-new-window-timer-peer",
        Some(owner.clone()),
    )
    .await;

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::MonitorSilence,
                value: "60".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    let base_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    for (offset, target) in [&owner, &peer]
        .into_iter()
        .flat_map(|session_name| {
            (0..=1).map(move |window_index| {
                WindowTarget::with_window(session_name.clone(), window_index)
            })
        })
        .enumerate()
    {
        handler.replace_silence_timer_deadline_for_test(
            &target,
            base_deadline + std::time::Duration::from_secs(offset as u64),
        );
    }

    let before = {
        let state = handler.state.lock().await;
        let mut before = Vec::new();
        for session_name in [&owner, &peer] {
            let session = state.sessions.session(session_name).unwrap_or_else(|| {
                panic!("group member {session_name} exists before source-file insertion")
            });
            for window_index in 0..=1 {
                let target = WindowTarget::with_window(session_name.clone(), window_index);
                let window_id = session
                    .window_at(window_index)
                    .expect("original alias exists")
                    .id();
                let timer = handler
                    .silence_timer_snapshot_for_test(&target)
                    .expect("original alias silence timer is armed");
                before.push((
                    session_name.clone(),
                    session.id(),
                    window_index,
                    window_id,
                    timer.1,
                ));
            }
        }
        before
    };

    let root = temp_root("grouped-new-window-silence-timers");
    let config_path = root.join("new-window.conf");
    write_config(&config_path, &format!("new-window -b -d -t {owner}:0\n"));
    let response = handler
        .handle(source_file_request(
            vec![config_path.to_string_lossy().into_owned()],
            Some(std::env::temp_dir()),
        ))
        .await;
    fs::remove_dir_all(root).expect("remove grouped new-window config root");
    assert_eq!(
        response,
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
    );

    for (session_name, session_id, previous_index, window_id, deadline) in before {
        let shifted_index = previous_index + 1;
        let shifted = WindowTarget::with_window(session_name.clone(), shifted_index);
        {
            let state = handler.state.lock().await;
            let session = state
                .sessions
                .session(&session_name)
                .expect("group member survives source-file insertion");
            assert_eq!(
                session
                    .window_at(shifted_index)
                    .expect("original alias shifts")
                    .id(),
                window_id
            );
        }
        assert_eq!(
            handler
                .silence_timer_snapshot_for_test(&shifted)
                .expect("shifted source-file timer survives")
                .1,
            deadline,
            "queued source-file insertion must preserve each alias deadline by ordinal"
        );
        let shifted_identity = handler
            .silence_timer_identity_for_test(&shifted)
            .expect("shifted source-file timer identity exists");
        assert_eq!(
            (shifted_identity.0, shifted_identity.1),
            (session_id, window_id)
        );
    }

    for session_name in [&owner, &peer] {
        let inserted = WindowTarget::with_window(session_name.clone(), 0);
        let inserted_identity = handler
            .silence_timer_identity_for_test(&inserted)
            .expect("inserted source-file group peer timer is armed");
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(session_name)
            .expect("group member exists after source-file insertion");
        assert_eq!(inserted_identity.0, session.id());
        assert_eq!(
            inserted_identity.1,
            session.window_at(0).expect("inserted window exists").id()
        );
    }
}

async fn create_quiet_source_timer_session(
    handler: &RequestHandler,
    name: &str,
    group_target: Option<SessionName>,
) -> SessionName {
    let session = session_name(name);
    let command = group_target
        .is_none()
        .then(quiet_source_timer_window_command);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    session
}

#[cfg(unix)]
fn quiet_source_timer_window_command() -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 60".to_owned()]
}

#[cfg(windows)]
fn quiet_source_timer_window_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = PathBuf::from(system_root).join("System32").join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
    ]
}
