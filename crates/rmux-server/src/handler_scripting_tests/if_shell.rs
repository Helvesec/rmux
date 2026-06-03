use super::*;

#[tokio::test]
async fn if_shell_format_mode_dispatches_selected_rmux_command() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::IfShell(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "set-buffer -b chosen selected".to_owned(),
            else_command: Some("set-buffer -b chosen wrong".to_owned()),
            target: None,
            caller_cwd: None,
            background: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::IfShell(rmux_proto::IfShellResponse::no_output())
    );

    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("chosen".to_owned()),
        }))
        .await;
    assert_eq!(
        response
            .command_output()
            .expect("show-buffer output")
            .stdout(),
        b"selected"
    );
}

#[tokio::test]
async fn source_file_if_shell_true_executes_brace_command_list() {
    let handler = RequestHandler::new();
    let root = temp_root("if-shell-true-brace");
    let config = root.join("main.conf");
    write_config(&config, "if-shell true { set-buffer -b chosen selected }\n");

    let response = handler
        .handle(source_file_request(
            vec!["main.conf".to_owned()],
            Some(root.clone()),
        ))
        .await;
    assert_eq!(
        response,
        Response::SourceFile(rmux_proto::SourceFileResponse { output: None })
    );

    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("chosen".to_owned()),
        }))
        .await;
    assert_eq!(
        response
            .command_output()
            .expect("show-buffer output")
            .stdout(),
        b"selected"
    );

    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn if_shell_pane_id_target_resolves_like_display_message() {
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

    let display_message = CommandParser::new()
        .parse("display-message -p -t %0 OKDM")
        .expect("display-message parses");
    let display_output = handler
        .execute_parsed_commands(
            std::process::id(),
            display_message,
            QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("display-message -t %0 should resolve");
    assert_eq!(String::from_utf8_lossy(&display_output.stdout), "OKDM\n");

    let if_shell = CommandParser::new()
        .parse("if-shell -F -t %0 1 \"display-message -p XOK\"")
        .expect("if-shell parses");
    let if_shell_output = handler
        .execute_parsed_commands(
            std::process::id(),
            if_shell,
            QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("if-shell -t %0 should resolve");
    assert_eq!(String::from_utf8_lossy(&if_shell_output.stdout), "XOK\n");
}

#[tokio::test]
async fn if_shell_false_without_else_is_a_successful_noop() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::IfShell(IfShellRequest {
            condition: "0".to_owned(),
            format_mode: true,
            then_command: "set-buffer impossible".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::IfShell(rmux_proto::IfShellResponse::no_output())
    );
}

#[tokio::test]
async fn scripted_pane_commands_accept_session_targets_like_tmux() {
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

    let response = handler
        .handle(Request::IfShell(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "copy-mode -t alpha".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        }))
        .await;
    assert!(matches!(response, Response::IfShell(_)));

    let mode = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::new(alpha, 0))),
            print: true,
            message: Some("#{pane_in_mode}".to_owned()),
        }))
        .await;
    let output = mode.command_output().expect("display-message output");
    assert_eq!(output.stdout(), b"1\n");
}

#[cfg(unix)]
#[tokio::test]
async fn if_shell_shell_mode_uses_tmux_shell_environment_and_caller_cwd() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let root = temp_root("if-shell-shell-mode");
    let marker = root.join("shell-used.txt");
    let shell_path = root.join("record-shell.sh");

    write_executable_script(
        &shell_path,
        &format!(
            "#!/bin/sh\nprintf used > {}\nexec /bin/sh \"$@\"\n",
            shell_quote(&marker)
        ),
    );

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
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::DefaultShell,
                value: shell_path.to_string_lossy().into_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetEnvironment(SetEnvironmentRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                name: "FOO".to_owned(),
                value: "bar".to_owned(),
                mode: None,
                hidden: false,
                format: false,
            }))
            .await,
        Response::SetEnvironment(_)
    ));

    let response = handler
        .handle(Request::IfShell(IfShellRequest {
            condition: format!(
                "test \"$FOO\" = bar && test \"$PWD\" = {}",
                shell_quote(&root)
            ),
            format_mode: false,
            then_command: "set-buffer -b chosen yes".to_owned(),
            else_command: Some("set-buffer -b chosen no".to_owned()),
            target: Some(Target::Session(alpha)),
            caller_cwd: Some(root),
            background: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::IfShell(rmux_proto::IfShellResponse::no_output())
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("chosen".to_owned()),
            }))
            .await
            .command_output()
            .expect("show-buffer output")
            .stdout(),
        b"yes"
    );
    assert_eq!(fs::read_to_string(marker).expect("shell marker"), "used");
}

#[cfg(windows)]
#[tokio::test]
async fn if_shell_shell_mode_uses_windows_shell_environment_and_caller_cwd() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let root = temp_root("if-shell-shell-mode");
    fs::create_dir_all(&root).expect("caller cwd");
    let cmd = std::env::var_os("COMSPEC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cmd.exe"));

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
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::DefaultShell,
                value: cmd.to_string_lossy().into_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetEnvironment(SetEnvironmentRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                name: "FOO".to_owned(),
                value: "bar".to_owned(),
                mode: None,
                hidden: false,
                format: false,
            }))
            .await,
        Response::SetEnvironment(_)
    ));

    let root = root.to_string_lossy().into_owned();
    let response = handler
        .handle(Request::IfShell(IfShellRequest {
            condition: format!(
                "if not \"%FOO%\"==\"bar\" exit /b 1 & if not \"%CD%\"==\"{root}\" exit /b 1 & exit /b 0"
            ),
            format_mode: false,
            then_command: "set-buffer -b chosen yes".to_owned(),
            else_command: Some("set-buffer -b chosen no".to_owned()),
            target: Some(Target::Session(alpha)),
            caller_cwd: Some(PathBuf::from(root)),
            background: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::IfShell(rmux_proto::IfShellResponse::no_output())
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("chosen".to_owned()),
            }))
            .await
            .command_output()
            .expect("show-buffer output")
            .stdout(),
        b"yes"
    );
}

#[tokio::test]
async fn if_shell_nested_set_buffer_accepts_hyphen_prefixed_content() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::IfShell(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "set-buffer -b hyphen -value".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::IfShell(rmux_proto::IfShellResponse::no_output())
    );

    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("hyphen".to_owned()),
        }))
        .await;
    assert_eq!(
        response
            .command_output()
            .expect("show-buffer output")
            .stdout(),
        b"-value"
    );
}

#[tokio::test]
async fn if_shell_nested_wait_for_accepts_hyphen_prefixed_channel_after_mode_flag() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::IfShell(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "wait-for -S -channel".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::IfShell(rmux_proto::IfShellResponse::no_output())
    );
}

#[tokio::test]
async fn if_shell_nested_run_shell_accepts_double_dash_before_command() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::IfShell(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: format!("run-shell -- {}", command_quote(&shell_success_command())),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::IfShell(rmux_proto::IfShellResponse::no_output())
    );
}

#[tokio::test]
async fn if_shell_string_mode_runs_multiple_commands_in_one_group() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::IfShell(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "set-buffer -b one first; set-buffer -b two second".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::IfShell(rmux_proto::IfShellResponse::no_output())
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("one".to_owned()),
            }))
            .await
            .command_output()
            .expect("one buffer output")
            .stdout(),
        b"first"
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("two".to_owned()),
            }))
            .await
            .command_output()
            .expect("two buffer output")
            .stdout(),
        b"second"
    );
}

#[tokio::test]
async fn if_shell_inserted_assignments_apply_before_parent_queue_tail() {
    let handler = RequestHandler::new();
    let command = shell_env_or_default_command("FOO", "unset");
    let parsed = CommandParser::new()
        .parse(&format!(
            "if-shell -F 1 {{ FOO=bar }} ; run-shell {}",
            command_quote(&command)
        ))
        .expect("commands parse");

    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue succeeds");

    assert_eq!(output.stdout(), b"bar");

    let state = handler.state.lock().await;
    assert_eq!(state.environment.global_value("FOO"), Some("bar"));
}
