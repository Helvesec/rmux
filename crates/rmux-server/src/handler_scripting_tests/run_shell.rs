use super::*;
use crate::handler::with_expected_attach_and_session_identity;
use crate::pane_io::AttachControl;
use rmux_proto::{ErrorResponse, RmuxError};
#[tokio::test]
async fn run_shell_foreground_returns_stdout_like_tmux() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;

    let response = handler
        .handle(run_shell(&shell_print_command("hello"), false))
        .await;

    assert_eq!(
        response,
        Response::RunShell(RunShellResponse::from_output_and_exit_status(
            rmux_proto::CommandOutput::from_stdout(b"hello\n".to_vec()),
            0,
        ))
    );
}

#[tokio::test]
async fn run_shell_nonzero_returns_stdout_and_returned_message() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;
    let command = shell_print_then_exit_command("hidden", 7);

    let response = handler.handle(run_shell(&command, false)).await;

    assert_eq!(
        response,
        Response::RunShell(RunShellResponse::from_output_and_exit_status(
            rmux_proto::CommandOutput::from_stdout(
                format!("hidden\n'{command}' returned 7\n").into_bytes()
            ),
            7,
        ))
    );
}

#[tokio::test]
async fn run_shell_stderr_output_flag_merges_stdout_and_stderr_like_tmux() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;

    let response = handler
        .handle(Request::RunShell(Box::new(RunShellRequest {
            command: format!(
                "{}; {}",
                shell_print_command("out"),
                shell_stderr_command("err")
            ),
            arguments: Vec::new(),
            background: false,
            as_commands: false,
            show_stderr: true,
            delay_seconds: None,
            start_directory: None,
            target: None,
            source_depth: None,
        })))
        .await;

    let output = response
        .command_output()
        .expect("run-shell -E returns stdout and stderr output");
    assert_eq!(output.stdout(), b"outerr\n");
    match response {
        Response::RunShell(response) => assert_eq!(response.exit_status(), Some(0)),
        other => panic!("expected RunShell response, got {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn run_shell_uses_bin_sh_instead_of_default_shell_like_tmux() {
    let handler = RequestHandler::new();
    let root = temp_root("run-shell-bin-sh");
    fs::create_dir_all(&root).expect("temp output root");
    let fake_shell = root.join("fake-shell.sh");
    let marker_path = root.join("default-shell-used.txt");
    let output_path = root.join("run-shell-output.txt");

    write_executable_script(
        &fake_shell,
        &format!(
            "#!/bin/sh\nprintf used > {}\nexit 42\n",
            shell_quote(&marker_path)
        ),
    );

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::DefaultShell,
                value: fake_shell.to_string_lossy().into_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    let response = handler
        .handle(Request::RunShell(Box::new(RunShellRequest {
            command: format!("printf ok > {}", shell_quote(&output_path)),
            arguments: Vec::new(),
            background: false,
            as_commands: false,
            show_stderr: false,
            delay_seconds: None,
            start_directory: None,
            target: None,
            source_depth: None,
        })))
        .await;

    assert_eq!(
        response,
        Response::RunShell(RunShellResponse::from_exit_status(0))
    );
    assert_eq!(
        fs::read_to_string(&output_path).expect("run-shell output"),
        "ok"
    );
    assert!(
        !marker_path.exists(),
        "run-shell should not execute default-shell for tmux jobs"
    );
}

#[tokio::test]
async fn run_shell_background_returns_immediately_without_output() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(run_shell(&shell_success_command(), true))
        .await;

    assert_eq!(response, Response::RunShell(RunShellResponse::background()));
}

#[tokio::test]
async fn background_run_shell_is_tracked_as_detached_request_until_finished() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::RunShell(Box::new(RunShellRequest {
            command: String::new(),
            arguments: Vec::new(),
            background: true,
            as_commands: false,
            show_stderr: false,
            delay_seconds: Some(RunShellDelaySeconds(0.2)),
            start_directory: None,
            target: None,
            source_depth: None,
        })))
        .await;

    assert_eq!(response, Response::RunShell(RunShellResponse::background()));
    wait_for_detached_request_count(&handler, 1).await;
    wait_for_detached_request_count(&handler, 0).await;
}

#[tokio::test]
async fn background_run_shell_commands_keep_detached_write_access_after_response() {
    let handler = RequestHandler::new();
    let requester_pid = 424_005;
    let parsed = CommandParser::new()
        .parse("run-shell -b -d 0.05 -C 'set-buffer -b bg-run-shell ok'")
        .expect("background run-shell command parses");

    {
        let _access = handler.begin_detached_requester_access(requester_pid, true);
        let output = handler
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await
            .expect("background run-shell dispatch succeeds");
        assert!(output.stdout().is_empty());
    }

    wait_for_named_buffer(&handler, "bg-run-shell", b"ok").await;
}

#[tokio::test]
async fn background_run_shell_commands_reject_a_reused_control_registration() {
    let handler = RequestHandler::new();
    let requester_pid = 424_105;
    let original = session_name("run-shell-control-original");
    let replacement = session_name("run-shell-control-replacement");
    let wait_channel = "run-shell-control-registration-reuse";
    create_background_identity_session(&handler, original.clone()).await;
    create_background_identity_session(&handler, replacement.clone()).await;
    let (original_control_id, original_events) =
        register_control_for_session(&handler, requester_pid, original.clone()).await;

    let commands = CommandParser::new()
        .parse(&format!(
            "run-shell -b -C 'wait-for {wait_channel} ; kill-session -t {}'",
            replacement.as_str()
        ))
        .expect("background run-shell command parses");
    let result = handler
        .execute_control_commands_identity(requester_pid, original_control_id, commands)
        .await;
    assert!(result.error.is_none(), "{result:?}");
    wait_for_background_waiter(&handler, wait_channel).await;

    let (_replacement_control_id, replacement_events) =
        register_control_for_session(&handler, requester_pid, replacement.clone()).await;
    release_background_waiter(&handler, wait_channel).await;

    assert_sessions_survive_background_control_reuse(&handler, &original, &replacement).await;
    drop((original_events, replacement_events));
}

#[tokio::test]
async fn background_run_shell_commands_reject_a_reused_attach_registration() {
    let handler = RequestHandler::new();
    let requester_pid = 424_205;
    let original = session_name("run-shell-attach-original");
    let replacement = session_name("run-shell-attach-replacement");
    let wait_channel = "run-shell-attach-registration-reuse";
    create_background_identity_session(&handler, original.clone()).await;
    create_background_identity_session(&handler, replacement.clone()).await;
    let (original_control_tx, _original_control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, original.clone(), original_control_tx)
        .await;
    let original_identity = handler.active_attach_identity_for_test(requester_pid).await;

    let commands = CommandParser::new()
        .parse(&format!(
            "run-shell -b -C 'wait-for {wait_channel} ; detach-client'"
        ))
        .expect("background run-shell command parses");
    let output = with_expected_attach_and_session_identity(
        original_identity,
        original.clone(),
        original_identity.session_id(),
        handler.execute_parsed_commands_for_test(requester_pid, commands),
    )
    .await
    .expect("background run-shell dispatch succeeds");
    assert!(output.stdout().is_empty());
    wait_for_background_waiter(&handler, wait_channel).await;

    let (replacement_control_tx, mut replacement_control_rx) =
        tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, replacement.clone(), replacement_control_tx)
        .await;
    let replacement_identity = handler.active_attach_identity_for_test(requester_pid).await;
    while replacement_control_rx.try_recv().is_ok() {}

    release_background_waiter(&handler, wait_channel).await;
    wait_for_detached_request_count(&handler, 0).await;
    assert!(
        handler
            .current_live_attach_input(replacement_identity)
            .await,
        "stale background run-shell queue must not detach the same-PID replacement"
    );
    while let Ok(control) = replacement_control_rx.try_recv() {
        assert!(
            !matches!(control, AttachControl::Detach),
            "stale background run-shell queue detached the replacement registration"
        );
    }

    let state = handler.state.lock().await;
    assert!(state.sessions.contains_session(&original));
    assert!(state.sessions.contains_session(&replacement));
}

#[tokio::test]
async fn background_run_shell_commands_survive_a_same_registration_session_switch() {
    let handler = RequestHandler::new();
    let requester_pid = 424_305;
    let alpha = session_name("run-shell-attach-switch-alpha");
    let beta = session_name("run-shell-attach-switch-beta");
    let wait_channel = "run-shell-attach-session-switch";
    let followed_window_name = "run-shell-followed-attached-session";
    create_background_identity_session(&handler, alpha.clone()).await;
    create_background_identity_session(&handler, beta.clone()).await;
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let identity = handler.active_attach_identity_for_test(requester_pid).await;

    let commands = CommandParser::new()
        .parse(&format!(
            "run-shell -b -C 'wait-for {wait_channel} ; rename-window {followed_window_name}' ; switch-client -t {beta}"
        ))
        .expect("background run-shell and attached switch parse");
    let output = with_expected_attach_and_session_identity(
        identity,
        alpha.clone(),
        identity.session_id(),
        handler.execute_parsed_commands_for_test(requester_pid, commands),
    )
    .await
    .expect("same-registration attached switch keeps the outer queue valid");
    assert!(output.stdout().is_empty());

    let switched_identity = handler.active_attach_identity_for_test(requester_pid).await;
    assert_eq!(switched_identity.attach_id(), identity.attach_id());
    assert_eq!(
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&requester_pid)
            .expect("attached registration survives")
            .session_name,
        beta
    );
    wait_for_background_waiter(&handler, wait_channel).await;
    replace_background_identity_session(&handler, alpha.clone()).await;
    release_background_waiter(&handler, wait_channel).await;
    wait_for_active_window_name(&handler, &beta, followed_window_name).await;
    let state = handler.state.lock().await;
    let replacement = state
        .sessions
        .session(&alpha)
        .expect("replacement alpha exists");
    assert_ne!(
        replacement
            .window_at(replacement.active_window_index())
            .and_then(rmux_core::Window::name),
        Some(followed_window_name),
        "stale background context mutated the replacement alpha session"
    );
}

#[tokio::test]
async fn background_run_shell_expands_implicit_formats_after_attached_switch() {
    let handler = RequestHandler::new();
    let requester_pid = 424_306;
    let alpha = session_name("run-shell-format-switch-alpha");
    let beta = session_name("run-shell-format-switch-beta");
    create_background_identity_session(&handler, alpha.clone()).await;
    create_background_identity_session(&handler, beta.clone()).await;
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let identity = handler.active_attach_identity_for_test(requester_pid).await;

    let commands = CommandParser::new()
        .parse(&format!(
            "run-shell -b -d 0.05 -C 'set-buffer -b bg-format x#{{session_name}}' ; switch-client -t {beta}"
        ))
        .expect("background format expansion and attached switch parse");
    with_expected_attach_and_session_identity(
        identity,
        alpha,
        identity.session_id(),
        handler.execute_parsed_commands_for_test(requester_pid, commands),
    )
    .await
    .expect("attached switch completes before delayed background expansion");

    wait_for_named_buffer(
        &handler,
        "bg-format",
        format!("x{}", beta.as_str()).as_bytes(),
    )
    .await;
}

#[tokio::test]
async fn background_run_shell_builds_environment_for_followed_attached_session() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;
    let requester_pid = 424_311;
    let alpha = session_name("run-shell-environment-switch-alpha");
    let beta = session_name("run-shell-environment-switch-beta");
    create_background_identity_session(&handler, alpha.clone()).await;
    create_background_identity_session(&handler, beta.clone()).await;
    for (session, value) in [(&alpha, "alpha"), (&beta, "beta")] {
        let response = handler
            .handle(Request::SetEnvironment(Box::new(SetEnvironmentRequest {
                scope: ScopeSelector::Session(session.clone()),
                name: "RMUX_BG_TARGET".to_owned(),
                value: value.to_owned(),
                mode: None,
                hidden: false,
                format: false,
            })))
            .await;
        assert!(
            matches!(response, Response::SetEnvironment(_)),
            "{response:?}"
        );
    }

    let root = temp_root("run-shell-followed-environment");
    std::fs::create_dir_all(&root).expect("background environment output root");
    let output_path = root.join("target.txt");
    #[cfg(unix)]
    let shell_command = format!(
        "printf '%s' \"$RMUX_BG_TARGET\" > {}",
        shell_quote(&output_path)
    );
    #[cfg(windows)]
    let shell_command = format!(
        "[IO.File]::WriteAllText({}, $env:RMUX_BG_TARGET)",
        crate::test_shell::powershell_quote_path(&output_path)
    );

    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let identity = handler.active_attach_identity_for_test(requester_pid).await;
    with_expected_attach_and_session_identity(identity, alpha, identity.session_id(), async {
        let response = handler
            .handle_run_shell(
                requester_pid,
                RunShellRequest {
                    command: shell_command,
                    arguments: Vec::new(),
                    background: true,
                    as_commands: false,
                    show_stderr: true,
                    delay_seconds: Some(RunShellDelaySeconds(0.05)),
                    start_directory: Some(root.clone()),
                    target: None,
                    source_depth: None,
                },
            )
            .await;
        assert_eq!(response, Response::RunShell(RunShellResponse::background()));

        let switch = CommandParser::new()
            .parse(&format!("switch-client -t {beta}"))
            .expect("attached environment switch parses");
        handler
            .execute_parsed_commands_for_test(requester_pid, switch)
            .await
            .expect("attached environment switch executes");
    })
    .await;

    wait_for_file_text(&output_path, "beta").await;
    std::fs::remove_dir_all(root).expect("remove background environment output root");
}

#[tokio::test]
async fn explicit_background_run_shell_target_survives_attached_switch() {
    let handler = RequestHandler::new();
    let requester_pid = 424_307;
    let alpha = session_name("run-shell-explicit-switch-alpha");
    let beta = session_name("run-shell-explicit-switch-beta");
    let gamma = session_name("run-shell-explicit-switch-gamma");
    let expected_window_name = "run-shell-explicit-target";
    create_background_identity_session(&handler, alpha.clone()).await;
    create_background_identity_session(&handler, beta.clone()).await;
    create_background_identity_session(&handler, gamma.clone()).await;
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let identity = handler.active_attach_identity_for_test(requester_pid).await;

    let commands = CommandParser::new()
        .parse(&format!(
            "run-shell -b -d 0.05 -C -t {gamma}:0.0 'rename-window {expected_window_name}' ; switch-client -t {beta}"
        ))
        .expect("explicit background target and attached switch parse");
    with_expected_attach_and_session_identity(
        identity,
        alpha,
        identity.session_id(),
        handler.execute_parsed_commands_for_test(requester_pid, commands),
    )
    .await
    .expect("attached switch does not cancel an explicit background target");

    wait_for_active_window_name(&handler, &gamma, expected_window_name).await;
    let state = handler.state.lock().await;
    assert_ne!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(session.active_window_index()))
            .and_then(rmux_core::Window::name),
        Some(expected_window_name),
        "explicit background run-shell must not rebase onto the attached session"
    );
}

#[tokio::test]
async fn explicit_background_shell_target_survives_origin_attach_detach() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;
    let requester_pid = 424_312;
    let origin = session_name("run-shell-explicit-detach-origin");
    let target = session_name("run-shell-explicit-detach-target");
    create_background_identity_session(&handler, origin.clone()).await;
    create_background_identity_session(&handler, target.clone()).await;

    let root = temp_root("run-shell-explicit-detach");
    std::fs::create_dir_all(&root).expect("explicit detach output root");
    let output_path = root.join("completed.txt");
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, origin.clone(), control_tx)
        .await;
    let identity = handler.active_attach_identity_for_test(requester_pid).await;

    let response = with_expected_attach_and_session_identity(
        identity,
        origin,
        identity.session_id(),
        handler.handle_run_shell(
            requester_pid,
            RunShellRequest {
                command: write_text_command(&output_path, "ok"),
                arguments: Vec::new(),
                background: true,
                as_commands: false,
                show_stderr: true,
                delay_seconds: Some(RunShellDelaySeconds(0.05)),
                start_directory: Some(root.clone()),
                target: Some(PaneTarget::with_window(target, 0, 0)),
                source_depth: None,
            },
        ),
    )
    .await;
    assert_eq!(response, Response::RunShell(RunShellResponse::background()));

    let detached = handler.handle_detach_client_for_identity(identity).await;
    assert!(
        matches!(detached, Response::DetachClient(_)),
        "{detached:?}"
    );
    wait_for_file_text(&output_path, "ok").await;
    std::fs::remove_dir_all(root).expect("remove explicit detach output root");
}

#[tokio::test]
async fn explicit_background_shell_target_survives_same_pid_attach_replacement() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;
    let requester_pid = 424_313;
    let origin = session_name("run-shell-explicit-reuse-origin");
    let replacement = session_name("run-shell-explicit-reuse-replacement");
    let target = session_name("run-shell-explicit-reuse-target");
    create_background_identity_session(&handler, origin.clone()).await;
    create_background_identity_session(&handler, replacement.clone()).await;
    create_background_identity_session(&handler, target.clone()).await;

    let root = temp_root("run-shell-explicit-reuse");
    std::fs::create_dir_all(&root).expect("explicit reuse output root");
    let output_path = root.join("completed.txt");
    let (origin_control_tx, _origin_control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, origin.clone(), origin_control_tx)
        .await;
    let origin_identity = handler.active_attach_identity_for_test(requester_pid).await;

    let response = with_expected_attach_and_session_identity(
        origin_identity,
        origin,
        origin_identity.session_id(),
        handler.handle_run_shell(
            requester_pid,
            RunShellRequest {
                command: write_text_command(&output_path, "ok"),
                arguments: Vec::new(),
                background: true,
                as_commands: false,
                show_stderr: true,
                delay_seconds: Some(RunShellDelaySeconds(0.05)),
                start_directory: Some(root.clone()),
                target: Some(PaneTarget::with_window(target, 0, 0)),
                source_depth: None,
            },
        ),
    )
    .await;
    assert_eq!(response, Response::RunShell(RunShellResponse::background()));

    let (replacement_control_tx, mut replacement_control_rx) =
        tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, replacement.clone(), replacement_control_tx)
        .await;
    let replacement_identity = handler.active_attach_identity_for_test(requester_pid).await;
    assert_ne!(
        replacement_identity.attach_id(),
        origin_identity.attach_id()
    );
    while replacement_control_rx.try_recv().is_ok() {}

    wait_for_file_text(&output_path, "ok").await;
    assert!(
        handler
            .current_live_attach_input(replacement_identity)
            .await,
        "explicit background shell must not invalidate the replacement registration"
    );
    while let Ok(control) = replacement_control_rx.try_recv() {
        assert!(
            !matches!(control, AttachControl::Detach),
            "explicit background shell detached the replacement registration"
        );
    }
    std::fs::remove_dir_all(root).expect("remove explicit reuse output root");
}

#[tokio::test]
async fn background_run_shell_commands_still_emit_after_hooks_outside_hook_context() {
    let handler = RequestHandler::new();
    create_named_session(&handler, "run-shell-after-hooks").await;
    execute_test_command(
        &handler,
        "set-hook -g after-new-window 'set-buffer -b after-run-shell yes'",
    )
    .await;

    execute_test_command(&handler, "run-shell -b -C 'new-window -d -n bg'").await;

    wait_for_named_buffer(&handler, "after-run-shell", b"yes").await;
}

#[tokio::test]
async fn queued_run_shell_command_mode_ignores_positional_arguments_like_tmux() {
    let handler = RequestHandler::new();

    execute_test_command(
        &handler,
        "run-shell -C 'set-buffer -b positional #{1}-#{2}' alpha beta",
    )
    .await;

    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("positional".to_owned()),
        }))
        .await;
    assert_eq!(
        response
            .command_output()
            .expect("show-buffer output")
            .stdout(),
        b"-"
    );
}

#[tokio::test]
async fn run_shell_command_mode_attach_session_requires_terminal_like_tmux() {
    let handler = RequestHandler::new();
    create_named_session(&handler, "run-shell-attach-target").await;

    let response = handler
        .handle(Request::RunShell(Box::new(RunShellRequest {
            command: "attach-session -t run-shell-attach-target".to_owned(),
            arguments: Vec::new(),
            background: false,
            as_commands: true,
            show_stderr: false,
            delay_seconds: None,
            start_directory: None,
            target: None,
            source_depth: None,
        })))
        .await;

    assert!(matches!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::Server(message)
        }) if message == "open terminal failed: not a terminal"
    ));
}

#[tokio::test]
async fn run_shell_command_mode_rejects_nested_and_implicit_attach_like_tmux() {
    // Oracle probe 2026-07-08: tmux 3.7b fails run-shell -C with
    // "open terminal failed: not a terminal" for attach-session nested in a
    // brace body and for a non-detached new-session (and creates nothing).
    let handler = RequestHandler::new();
    create_named_session(&handler, "run-shell-nested-attach").await;

    for command in [
        "if-shell -F 1 { attach-session -t run-shell-nested-attach }",
        "new-session",
    ] {
        let response = handler
            .handle(Request::RunShell(Box::new(RunShellRequest {
                command: command.to_owned(),
                arguments: Vec::new(),
                background: false,
                as_commands: true,
                show_stderr: false,
                delay_seconds: None,
                start_directory: None,
                target: None,
                source_depth: None,
            })))
            .await;

        assert!(
            matches!(
                &response,
                Response::Error(ErrorResponse {
                    error: RmuxError::Server(message)
                }) if message == "open terminal failed: not a terminal"
            ),
            "command {command:?} must be rejected, got {response:?}"
        );
    }

    let detached = handler
        .handle(Request::RunShell(Box::new(RunShellRequest {
            command: "new-session -d -s run-shell-detached-ok".to_owned(),
            arguments: Vec::new(),
            background: false,
            as_commands: true,
            show_stderr: false,
            delay_seconds: None,
            start_directory: None,
            target: None,
            source_depth: None,
        })))
        .await;
    assert!(
        !matches!(detached, Response::Error(_)),
        "detached new-session must stay allowed, got {detached:?}"
    );
}

#[tokio::test]
async fn queued_run_shell_accepts_empty_command_as_noop_like_tmux() {
    let handler = RequestHandler::new();

    for command in ["run-shell", "run-shell -b", "run-shell -C"] {
        execute_test_command(&handler, command).await;
    }
}

#[tokio::test]
async fn run_shell_missing_explicit_target_is_nonfatal() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;

    let response = handler
        .handle(Request::RunShell(Box::new(RunShellRequest {
            command: shell_success_command(),
            arguments: Vec::new(),
            background: false,
            as_commands: false,
            show_stderr: false,
            delay_seconds: None,
            start_directory: None,
            target: Some(PaneTarget::new(session_name("missing"), 0)),
            source_depth: None,
        })))
        .await;

    assert_eq!(
        response,
        Response::RunShell(RunShellResponse::from_exit_status(0))
    );
}

#[tokio::test]
async fn background_if_shell_still_emits_after_hooks_outside_hook_context() {
    let handler = RequestHandler::new();
    create_named_session(&handler, "if-shell-after-hooks").await;
    execute_test_command(
        &handler,
        "set-hook -g after-new-window 'set-buffer -b after-if-shell yes'",
    )
    .await;

    execute_test_command(&handler, "if-shell -b -F '1' 'new-window -d -n ifbg'").await;

    wait_for_named_buffer(&handler, "after-if-shell", b"yes").await;
}

#[tokio::test]
async fn queued_background_if_shell_preserves_hook_formats_after_hook_scope_exits() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse(r#"if-shell -b -F '#{==:#{hook_pane},%1}' 'run-shell -C "set-buffer -b bg-hook-if ok"'"#)
        .expect("background queued if-shell command parses");

    let output = crate::hook_runtime::with_hook_execution(
        vec![("hook_pane".to_owned(), "%1".to_owned())],
        async {
            handler
                .execute_parsed_commands_for_test(std::process::id(), parsed)
                .await
        },
    )
    .await
    .expect("background queued if-shell dispatch succeeds");

    assert!(output.stdout().is_empty());
    wait_for_named_buffer(&handler, "bg-hook-if", b"ok").await;
}

#[tokio::test]
async fn background_run_shell_preserves_hook_formats_after_hook_scope_exits() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;
    let root = temp_root("run-shell-background-hook-formats");
    std::fs::create_dir_all(&root).expect("temp output root");
    let output_path = root.join("hook-pane.txt");
    let command = write_literal_format_command(&output_path, "#{hook_pane}");

    let response = crate::hook_runtime::with_hook_execution(
        vec![("hook_pane".to_owned(), "%1".to_owned())],
        async {
            handler
                .handle(Request::RunShell(Box::new(RunShellRequest {
                    command,
                    arguments: Vec::new(),
                    background: true,
                    as_commands: false,
                    show_stderr: true,
                    delay_seconds: None,
                    start_directory: None,
                    target: None,
                    source_depth: None,
                })))
                .await
        },
    )
    .await;

    assert_eq!(response, Response::RunShell(RunShellResponse::background()));
    wait_for_file_text(&output_path, "%1").await;
}

#[tokio::test]
async fn run_shell_expands_socket_path_without_target() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;
    handler.set_socket_path("/tmp/rmux-test.sock");
    let root = temp_root("run-shell-socket-path");
    std::fs::create_dir_all(&root).expect("temp output root");
    let output_path = root.join("socket-path.txt");
    let command = write_text_command(&output_path, "#{socket_path}");

    let response = handler
        .handle(Request::RunShell(Box::new(RunShellRequest {
            command,
            arguments: Vec::new(),
            background: false,
            as_commands: false,
            show_stderr: true,
            delay_seconds: None,
            start_directory: None,
            target: None,
            source_depth: None,
        })))
        .await;

    assert_eq!(
        response,
        Response::RunShell(RunShellResponse::from_exit_status(0))
    );
    assert_eq!(
        std::fs::read_to_string(output_path).expect("socket path output"),
        "/tmp/rmux-test.sock"
    );
}

async fn create_named_session(handler: &RequestHandler, name: &str) {
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name(name),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
}

async fn execute_test_command(handler: &RequestHandler, command: &str) {
    let parsed = CommandParser::new()
        .parse(command)
        .unwrap_or_else(|error| panic!("{command:?} should parse: {error}"));
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .unwrap_or_else(|error| panic!("{command:?} should execute: {error}"));
}

async fn wait_for_file_text(path: &std::path::Path, expected: &str) {
    tokio::time::timeout(background_shell_test_timeout(), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                if text == expected {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("file {path:?} did not become {expected:?}"));
}

fn write_literal_format_command(path: &std::path::Path, text: &str) -> String {
    #[cfg(unix)]
    {
        format!(
            "printf '%s' {} > {}",
            command_quote(text),
            shell_quote(path)
        )
    }
    #[cfg(windows)]
    {
        format!(
            "[IO.File]::WriteAllText({}, {})",
            crate::test_shell::powershell_quote_path(path),
            crate::test_shell::powershell_quote(text)
        )
    }
}

fn write_text_command(path: &std::path::Path, text: &str) -> String {
    #[cfg(unix)]
    {
        format!("printf {} > {}", command_quote(text), shell_quote(path))
    }
    #[cfg(windows)]
    {
        format!(
            "[IO.File]::WriteAllText({}, {})",
            crate::test_shell::powershell_quote_path(path),
            crate::test_shell::powershell_quote(text)
        )
    }
}

#[tokio::test]
async fn queue_parsed_run_shell_accepts_tmux_compact_delay_flag_without_running_a_shell_command() {
    let handler = RequestHandler::new();

    let parsed = handler
        .parse_command_string_one_group("run-shell -d0.01")
        .await
        .expect("compact tmux delay syntax parses");

    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("delay-only run-shell executes");

    assert!(
        output.stdout().is_empty(),
        "delay-only run-shell should not emit stdout, got: {:?}",
        String::from_utf8_lossy(output.stdout())
    );
}

#[tokio::test]
async fn run_shell_rejects_invalid_delay_without_closing_connection() {
    let handler = RequestHandler::new();

    for delay in [-1.0, f64::NAN, f64::INFINITY] {
        let response = handler
            .handle(Request::RunShell(Box::new(RunShellRequest {
                command: shell_success_command(),
                arguments: Vec::new(),
                background: false,
                as_commands: false,
                show_stderr: false,
                delay_seconds: Some(RunShellDelaySeconds(delay)),
                start_directory: None,
                target: None,
                source_depth: None,
            })))
            .await;

        assert!(
            matches!(&response, Response::Error(error) if error.error.to_string().contains("non-negative finite delay")),
            "expected invalid delay error for {delay:?}, got {response:?}"
        );
    }
}

#[tokio::test]
async fn run_shell_background_rejects_invalid_delay_before_reporting_success() {
    let handler = RequestHandler::new();

    for delay in [-1.0, f64::NAN, f64::INFINITY] {
        let response = handler
            .handle(Request::RunShell(Box::new(RunShellRequest {
                command: shell_success_command(),
                arguments: Vec::new(),
                background: true,
                as_commands: false,
                show_stderr: false,
                delay_seconds: Some(RunShellDelaySeconds(delay)),
                start_directory: None,
                target: None,
                source_depth: None,
            })))
            .await;

        assert!(
            matches!(&response, Response::Error(error) if error.error.to_string().contains("non-negative finite delay")),
            "expected invalid background delay error for {delay:?}, got {response:?}"
        );
    }
}

#[tokio::test]
async fn queue_parsed_run_shell_rejects_invalid_delay() {
    let handler = RequestHandler::new();

    let parsed = handler
        .parse_command_string_one_group("run-shell -d -1 true")
        .await
        .expect("command text should parse before semantic validation");
    let error = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect_err("negative run-shell delay should be rejected");

    assert!(
        error.to_string().contains("non-negative finite delay"),
        "unexpected error: {error}"
    );
}

#[test]
fn parsed_run_shell_accepts_tmux_clustered_no_value_flags() {
    let handler = RequestHandler::new();
    let state = handler.state.blocking_lock();
    let parsed = crate::handler::scripting_support::parse_request_from_parts(
        "run-shell".to_owned(),
        vec!["-bC".to_owned(), "set-option -g @compact yes".to_owned()],
        None,
        &state.sessions,
        &state.options,
        &TargetFindContext::new(None),
    )
    .expect("run-shell -bC parses like tmux");

    let Request::RunShell(request) = parsed else {
        panic!("expected RunShell request");
    };
    assert!(request.background);
    assert!(request.as_commands);
    assert!(!request.show_stderr);
    assert_eq!(request.command, "set-option -g @compact yes");
}

#[test]
fn parsed_send_keys_accepts_tmux_clustered_no_value_flags() {
    let handler = RequestHandler::new();
    let state = handler.state.blocking_lock();
    let parsed = crate::handler::scripting_support::parse_request_from_parts(
        "send-keys".to_owned(),
        vec!["-lR".to_owned(), "ABC".to_owned()],
        None,
        &state.sessions,
        &state.options,
        &TargetFindContext::new(None),
    )
    .expect("send-keys -lR parses like tmux");

    let Request::SendKeysExt(request) = parsed else {
        panic!("expected SendKeysExt request");
    };
    assert!(request.literal);
    assert!(request.reset_terminal);
    assert_eq!(request.keys, vec!["ABC".to_owned()]);
}

#[tokio::test]
async fn parsed_new_session_start_directory_sets_session_cwd() {
    let handler = RequestHandler::new();
    let root = temp_root("new-session-cwd");
    fs::create_dir_all(&root).expect("start directory");
    let parsed = CommandParser::new()
        .parse(&format!(
            "new-session -d -s alpha -c {}",
            shell_quote(&root)
        ))
        .expect("new-session -c parses");

    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("new-session -c executes");

    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&session_name("alpha"))
        .expect("session created");
    assert_eq!(session.cwd(), Some(root.as_path()));
}

#[test]
fn parsed_new_session_accepts_tmux_shell_command_after_double_dash() {
    let handler = RequestHandler::new();
    let state = handler.state.blocking_lock();
    let parsed = crate::handler::scripting_support::parse_request_from_parts(
        "new-session".to_owned(),
        vec![
            "-d".to_owned(),
            "-s".to_owned(),
            "alpha".to_owned(),
            "--".to_owned(),
            "sleep".to_owned(),
            "30".to_owned(),
        ],
        None,
        &state.sessions,
        &state.options,
        &TargetFindContext::new(None),
    )
    .expect("new-session shell command after -- parses");

    assert_eq!(
        parsed,
        Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session_name("alpha")),
            working_directory: None,
            detached: true,
            size: None,
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(vec!["sleep".to_owned(), "30".to_owned()]),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        }))
    );
}

#[test]
fn parsed_new_session_accepts_skip_environment_update() {
    let handler = RequestHandler::new();
    let state = handler.state.blocking_lock();
    let parsed = crate::handler::scripting_support::parse_request_from_parts(
        "new-session".to_owned(),
        vec![
            "-E".to_owned(),
            "-d".to_owned(),
            "-s".to_owned(),
            "alpha".to_owned(),
        ],
        None,
        &state.sessions,
        &state.options,
        &TargetFindContext::new(None),
    )
    .expect("new-session -E parses");

    let Request::NewSessionExt(request) = parsed else {
        panic!("expected NewSessionExt request");
    };

    assert!(request.skip_environment_update);
    assert_eq!(request.session_name, Some(session_name("alpha")));
    assert!(request.detached);
}
