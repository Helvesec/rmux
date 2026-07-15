use super::*;
use crate::handler::with_expected_attach_and_session_identity;
use crate::pane_io::AttachControl;

#[tokio::test]
async fn if_shell_format_mode_dispatches_selected_rmux_command() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::IfShell(Box::new(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "set-buffer -b chosen selected".to_owned(),
            else_command: Some("set-buffer -b chosen wrong".to_owned()),
            target: None,
            caller_cwd: None,
            background: false,
        })))
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
async fn background_if_shell_keeps_detached_write_access_after_response() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;
    let requester_pid = 424_006;

    {
        let _access = handler.begin_detached_requester_access(requester_pid, true);
        let response = handler
            .dispatch(
                requester_pid,
                Request::IfShell(Box::new(IfShellRequest {
                    condition: delayed_true_shell_condition(),
                    format_mode: false,
                    then_command: "set-buffer -b bg-if-shell ok".to_owned(),
                    else_command: None,
                    target: None,
                    caller_cwd: None,
                    background: true,
                })),
            )
            .await
            .response;
        assert_eq!(
            response,
            Response::IfShell(rmux_proto::IfShellResponse::no_output())
        );
    }

    wait_for_named_buffer(&handler, "bg-if-shell", b"ok").await;
}

#[tokio::test]
async fn background_if_shell_request_rejects_a_reused_control_registration() {
    let handler = RequestHandler::new();
    let requester_pid = 424_106;
    let original = session_name("if-shell-request-control-original");
    let replacement = session_name("if-shell-request-control-replacement");
    let wait_channel = "if-shell-request-control-registration-reuse";
    create_background_identity_session(&handler, original.clone()).await;
    create_background_identity_session(&handler, replacement.clone()).await;
    let (original_control_id, original_events) =
        register_control_for_session(&handler, requester_pid, original.clone()).await;

    let response = with_control_queue_identity(
        ControlClientIdentity::new(requester_pid, original_control_id),
        handler.handle_if_shell(
            requester_pid,
            IfShellRequest {
                condition: "1".to_owned(),
                format_mode: true,
                then_command: format!(
                    "wait-for {wait_channel} ; kill-session -t {}",
                    replacement.as_str()
                ),
                else_command: None,
                target: None,
                caller_cwd: None,
                background: true,
            },
        ),
    )
    .await;
    assert_eq!(
        response,
        Response::IfShell(rmux_proto::IfShellResponse::no_output())
    );
    wait_for_background_waiter(&handler, wait_channel).await;

    let (_replacement_control_id, replacement_events) =
        register_control_for_session(&handler, requester_pid, replacement.clone()).await;
    release_background_waiter(&handler, wait_channel).await;

    assert_sessions_survive_background_control_reuse(&handler, &original, &replacement).await;
    drop((original_events, replacement_events));
}

#[tokio::test]
async fn queued_background_if_shell_keeps_detached_write_access_after_response() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;
    let requester_pid = 424_007;
    let parsed = CommandParser::new()
        .parse(&format!(
            "if-shell -b {} 'set-buffer -b bg-queued-if-shell ok'",
            command_quote(&delayed_true_shell_condition())
        ))
        .expect("background if-shell command parses");

    {
        let _access = handler.begin_detached_requester_access(requester_pid, true);
        let output = handler
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await
            .expect("background if-shell dispatch succeeds");
        assert!(output.stdout().is_empty());
    }

    wait_for_named_buffer(&handler, "bg-queued-if-shell", b"ok").await;
}

#[tokio::test]
async fn queued_if_shell_rejects_unknown_option_before_condition() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("if-shell -Q true { set-buffer -b queued-unknown mutated }")
        .expect("generic parser preserves the unknown if-shell option");

    let error = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect_err("queued if-shell must reject an unknown option before its condition");

    assert_eq!(
        error,
        rmux_proto::RmuxError::Server("command if-shell: unknown flag -Q".to_owned())
    );
    assert!(matches!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("queued-unknown".to_owned()),
            }))
            .await,
        Response::Error(_)
    ));
}

#[tokio::test]
async fn queued_background_if_shell_rejects_a_reused_control_registration() {
    let handler = RequestHandler::new();
    let requester_pid = 424_107;
    let original = session_name("if-shell-queue-control-original");
    let replacement = session_name("if-shell-queue-control-replacement");
    let wait_channel = "if-shell-queue-control-registration-reuse";
    create_background_identity_session(&handler, original.clone()).await;
    create_background_identity_session(&handler, replacement.clone()).await;
    let (original_control_id, original_events) =
        register_control_for_session(&handler, requester_pid, original.clone()).await;

    let commands = CommandParser::new()
        .parse(&format!(
            "if-shell -b -F 1 {{ wait-for {wait_channel} ; kill-session -t {} }}",
            replacement.as_str()
        ))
        .expect("background queued if-shell command parses");
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

async fn assert_background_if_shell_rejects_reused_attach_registration(queued: bool) {
    let handler = RequestHandler::new();
    let requester_pid = if queued { 424_208 } else { 424_207 };
    let suffix = if queued { "queue" } else { "request" };
    let original = session_name(&format!("if-shell-{suffix}-attach-original"));
    let replacement = session_name(&format!("if-shell-{suffix}-attach-replacement"));
    let wait_channel = format!("if-shell-{suffix}-attach-registration-reuse");
    create_background_identity_session(&handler, original.clone()).await;
    create_background_identity_session(&handler, replacement.clone()).await;
    let (original_control_tx, _original_control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, original.clone(), original_control_tx)
        .await;
    let original_identity = handler.active_attach_identity_for_test(requester_pid).await;

    if queued {
        let commands = CommandParser::new()
            .parse(&format!(
                "if-shell -b -F 1 {{ wait-for {wait_channel} ; detach-client }}"
            ))
            .expect("background queued if-shell command parses");
        let output = with_expected_attach_and_session_identity(
            original_identity,
            original.clone(),
            original_identity.session_id(),
            handler.execute_parsed_commands_for_test(requester_pid, commands),
        )
        .await
        .expect("background queued if-shell dispatch succeeds");
        assert!(output.stdout().is_empty());
    } else {
        let response = with_expected_attach_and_session_identity(
            original_identity,
            original.clone(),
            original_identity.session_id(),
            handler.handle_if_shell(
                requester_pid,
                IfShellRequest {
                    condition: "1".to_owned(),
                    format_mode: true,
                    then_command: format!("wait-for {wait_channel} ; detach-client"),
                    else_command: None,
                    target: None,
                    caller_cwd: None,
                    background: true,
                },
            ),
        )
        .await;
        assert_eq!(
            response,
            Response::IfShell(rmux_proto::IfShellResponse::no_output())
        );
    }
    wait_for_background_waiter(&handler, &wait_channel).await;

    let (replacement_control_tx, mut replacement_control_rx) =
        tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, replacement.clone(), replacement_control_tx)
        .await;
    let replacement_identity = handler.active_attach_identity_for_test(requester_pid).await;
    while replacement_control_rx.try_recv().is_ok() {}

    release_background_waiter(&handler, &wait_channel).await;
    wait_for_detached_request_count(&handler, 0).await;
    assert!(
        handler
            .current_live_attach_input(replacement_identity)
            .await,
        "stale background if-shell must not detach the same-PID replacement"
    );
    while let Ok(control) = replacement_control_rx.try_recv() {
        assert!(
            !matches!(control, AttachControl::Detach),
            "stale background if-shell detached the replacement registration"
        );
    }

    let state = handler.state.lock().await;
    assert!(state.sessions.contains_session(&original));
    assert!(state.sessions.contains_session(&replacement));
}

#[tokio::test]
async fn background_if_shell_rejects_a_reused_attach_registration() {
    assert_background_if_shell_rejects_reused_attach_registration(false).await;
    assert_background_if_shell_rejects_reused_attach_registration(true).await;
}

#[tokio::test]
async fn background_if_shell_queue_survives_a_same_registration_session_switch() {
    let handler = RequestHandler::new();
    let requester_pid = 424_308;
    let alpha = session_name("if-shell-attach-switch-alpha");
    let beta = session_name("if-shell-attach-switch-beta");
    let wait_channel = "if-shell-attach-session-switch";
    let followed_window_name = "if-shell-followed-attached-session";
    create_background_identity_session(&handler, alpha.clone()).await;
    create_background_identity_session(&handler, beta.clone()).await;
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let identity = handler.active_attach_identity_for_test(requester_pid).await;

    let commands = CommandParser::new()
        .parse(&format!(
            "if-shell -b -F 1 {{ wait-for {wait_channel} ; rename-window {followed_window_name} }} ; switch-client -t {beta}"
        ))
        .expect("background if-shell and attached switch parse");
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

async fn assert_explicit_background_if_shell_target_survives_switch(queued: bool) {
    let handler = RequestHandler::new();
    let requester_pid = if queued { 424_310 } else { 424_309 };
    let suffix = if queued { "queue" } else { "request" };
    let alpha = session_name(&format!("if-shell-explicit-{suffix}-alpha"));
    let beta = session_name(&format!("if-shell-explicit-{suffix}-beta"));
    let gamma = session_name(&format!("if-shell-explicit-{suffix}-gamma"));
    let wait_channel = format!("if-shell-explicit-{suffix}-wait");
    let expected_window_name = format!("if-shell-explicit-{suffix}-target");
    create_background_identity_session(&handler, alpha.clone()).await;
    create_background_identity_session(&handler, beta.clone()).await;
    create_background_identity_session(&handler, gamma.clone()).await;
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let identity = handler.active_attach_identity_for_test(requester_pid).await;

    if queued {
        let commands = CommandParser::new()
            .parse(&format!(
                "if-shell -b -F -t {gamma}:0.0 1 {{ wait-for {wait_channel} ; rename-window {expected_window_name} }}"
            ))
            .expect("queued explicit background if-shell parses");
        with_expected_attach_and_session_identity(
            identity,
            alpha.clone(),
            identity.session_id(),
            handler.execute_parsed_commands_for_test(requester_pid, commands),
        )
        .await
        .expect("queued explicit background if-shell starts");
    } else {
        let response = with_expected_attach_and_session_identity(
            identity,
            alpha.clone(),
            identity.session_id(),
            handler.handle_if_shell(
                requester_pid,
                IfShellRequest {
                    condition: "1".to_owned(),
                    format_mode: true,
                    then_command: format!(
                        "wait-for {wait_channel} ; rename-window {expected_window_name}"
                    ),
                    else_command: None,
                    target: Some(Target::Pane(PaneTarget::with_window(gamma.clone(), 0, 0))),
                    caller_cwd: None,
                    background: true,
                },
            ),
        )
        .await;
        assert_eq!(
            response,
            Response::IfShell(rmux_proto::IfShellResponse::no_output())
        );
    }

    wait_for_background_waiter(&handler, &wait_channel).await;
    let switch = CommandParser::new()
        .parse(&format!("switch-client -t {beta}"))
        .expect("attached switch parses");
    with_expected_attach_and_session_identity(
        identity,
        alpha,
        identity.session_id(),
        handler.execute_parsed_commands_for_test(requester_pid, switch),
    )
    .await
    .expect("attached client switches while explicit background command waits");
    release_background_waiter(&handler, &wait_channel).await;

    wait_for_active_window_name(&handler, &gamma, &expected_window_name).await;
    let state = handler.state.lock().await;
    assert_ne!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(session.active_window_index()))
            .and_then(rmux_core::Window::name),
        Some(expected_window_name.as_str()),
        "explicit background if-shell target must not rebase onto the attached session"
    );
}

#[tokio::test]
async fn explicit_background_if_shell_targets_survive_attached_switch() {
    assert_explicit_background_if_shell_target_survives_switch(false).await;
    assert_explicit_background_if_shell_target_survives_switch(true).await;
}

#[tokio::test]
async fn background_if_shell_is_tracked_as_detached_request_until_finished() {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;

    #[cfg(unix)]
    let condition = "sleep 0.2; true".to_owned();
    #[cfg(windows)]
    let condition = "Start-Sleep -Milliseconds 200; exit 0".to_owned();

    let response = handler
        .handle(Request::IfShell(Box::new(IfShellRequest {
            condition,
            format_mode: false,
            then_command: "display-message done".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: true,
        })))
        .await;

    assert_eq!(
        response,
        Response::IfShell(rmux_proto::IfShellResponse::no_output())
    );
    wait_for_detached_request_count(&handler, 1).await;
    wait_for_detached_request_count(&handler, 0).await;
}

#[tokio::test]
async fn if_shell_format_mode_expands_socket_path_without_target() {
    let handler = RequestHandler::new();
    handler.set_socket_path("/tmp/rmux-test.sock");

    let response = handler
        .handle(Request::IfShell(Box::new(IfShellRequest {
            condition: "#{socket_path}".to_owned(),
            format_mode: true,
            then_command: "set-buffer -b chosen selected".to_owned(),
            else_command: Some("set-buffer -b chosen wrong".to_owned()),
            target: None,
            caller_cwd: None,
            background: false,
        })))
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
async fn if_shell_format_mode_treats_zero_prefixed_values_as_false_like_tmux() {
    let handler = RequestHandler::new();

    for condition in ["00", "09", "01", "0abc", "0.0"] {
        let buffer = format!("chosen-{condition}");
        let response = handler
            .handle(Request::IfShell(Box::new(IfShellRequest {
                condition: condition.to_owned(),
                format_mode: true,
                then_command: format!("set-buffer -b {buffer} selected"),
                else_command: Some(format!("set-buffer -b {buffer} fallback")),
                target: None,
                caller_cwd: None,
                background: false,
            })))
            .await;
        assert_eq!(
            response,
            Response::IfShell(rmux_proto::IfShellResponse::no_output())
        );

        let response = handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some(buffer),
            }))
            .await;
        assert_eq!(
            response
                .command_output()
                .expect("show-buffer output")
                .stdout(),
            b"fallback",
            "condition {condition:?} should be false"
        );
    }
}

#[tokio::test]
async fn if_shell_format_mode_without_target_uses_preferred_session_context() {
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
        .handle(Request::IfShell(Box::new(IfShellRequest {
            condition: "#{session_name}".to_owned(),
            format_mode: true,
            then_command: "set-buffer -b chosen selected".to_owned(),
            else_command: Some("set-buffer -b chosen wrong".to_owned()),
            target: None,
            caller_cwd: None,
            background: false,
        })))
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
async fn if_shell_missing_explicit_target_is_nonfatal() {
    let handler = RequestHandler::new();
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
        .handle(Request::IfShell(Box::new(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "display-message -p '#{session_name}'".to_owned(),
            else_command: None,
            target: Some(Target::Session(session_name("missing"))),
            caller_cwd: None,
            background: false,
        })))
        .await;

    assert_eq!(
        response
            .command_output()
            .expect("if-shell nested command output")
            .stdout(),
        b"alpha\n"
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
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
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
async fn queued_if_shell_target_becomes_branch_current_target() {
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

    let parsed = CommandParser::new()
        .parse("if-shell -F -t beta:0.0 1 { new-window -d -n nested }")
        .expect("if-shell parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd().with_current_target(Some(Target::Pane(
                PaneTarget::with_window(alpha.clone(), 0, 0),
            ))),
        )
        .await
        .expect("if-shell branch should execute");

    let state = handler.state.lock().await;
    let alpha_windows = state
        .sessions
        .session(&alpha)
        .expect("alpha exists")
        .windows()
        .keys()
        .copied()
        .collect::<Vec<_>>();
    let beta_session = state.sessions.session(&beta).expect("beta exists");
    assert_eq!(alpha_windows, vec![0]);
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        beta_session
            .window_at(1)
            .expect("nested window exists")
            .name(),
        Some("nested")
    );
}

#[tokio::test]
async fn queued_if_shell_accepts_compact_format_target_with_attached_value() {
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

    let parsed = CommandParser::new()
        .parse("if-shell -Ft= 1 { display-message -p '#{session_name}' }")
        .expect("if-shell compact target parses");
    let output = handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Session(beta)))
                .with_mouse_target(Some(Target::Window(rmux_proto::WindowTarget::with_window(
                    alpha, 0,
                )))),
        )
        .await
        .expect("compact if-shell branch should execute");
    assert_eq!(output.stdout(), b"alpha\n");
}

#[tokio::test]
async fn queued_if_shell_compact_mouse_target_falls_back_to_current_target() {
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

    let parsed = CommandParser::new()
        .parse("if-shell -Ft= 1 { display-message -p '#{session_name}' }")
        .expect("if-shell compact target parses");
    let output = handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Session(beta))),
        )
        .await
        .expect("compact if-shell branch should execute without mouse context");
    assert_eq!(output.stdout(), b"beta\n");
}

#[tokio::test]
async fn queued_if_shell_separated_mouse_target_falls_back_to_current_target() {
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

    let parsed = CommandParser::new()
        .parse("if-shell -F -t = 1 { display-message -p '#{session_name}' }")
        .expect("if-shell separated target parses");
    let output = handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Session(beta))),
        )
        .await
        .expect("separated if-shell branch should execute without mouse context");
    assert_eq!(output.stdout(), b"beta\n");
}

#[tokio::test]
async fn queued_if_shell_accepts_compact_format_target_with_next_argument() {
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

    let parsed = CommandParser::new()
        .parse("if-shell -Ft beta:0.0 1 { new-window -d -n compact }")
        .expect("if-shell compact target parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd().with_current_target(Some(Target::Pane(
                PaneTarget::with_window(alpha.clone(), 0, 0),
            ))),
        )
        .await
        .expect("compact if-shell branch should execute");

    let state = handler.state.lock().await;
    let alpha_windows = state
        .sessions
        .session(&alpha)
        .expect("alpha exists")
        .windows()
        .keys()
        .copied()
        .collect::<Vec<_>>();
    let beta_session = state.sessions.session(&beta).expect("beta exists");
    assert_eq!(alpha_windows, vec![0]);
    assert_eq!(
        beta_session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        beta_session
            .window_at(1)
            .expect("compact window exists")
            .name(),
        Some("compact")
    );
}

#[tokio::test]
async fn if_shell_false_without_else_is_a_successful_noop() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::IfShell(Box::new(IfShellRequest {
            condition: "0".to_owned(),
            format_mode: true,
            then_command: "set-buffer impossible".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        })))
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
        .handle(Request::IfShell(Box::new(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "copy-mode -t alpha".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        })))
        .await;
    assert!(matches!(response, Response::IfShell(_)));

    let mode = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::new(alpha, 0))),
            print: true,
            message: Some("#{pane_in_mode}".to_owned()),
            empty_target_context: false,
        }))
        .await;
    let output = mode.command_output().expect("display-message output");
    assert_eq!(output.stdout(), b"1\n");
}

#[cfg(unix)]
#[tokio::test]
async fn if_shell_shell_mode_uses_bin_sh_environment_and_caller_cwd() {
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
            .handle(Request::SetEnvironment(Box::new(SetEnvironmentRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                name: "FOO".to_owned(),
                value: "bar".to_owned(),
                mode: None,
                hidden: false,
                format: false,
            })))
            .await,
        Response::SetEnvironment(_)
    ));

    let response = handler
        .handle(Request::IfShell(Box::new(IfShellRequest {
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
        })))
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
    assert!(
        !marker.exists(),
        "if-shell should not execute default-shell for tmux jobs"
    );
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
            .handle(Request::SetEnvironment(Box::new(SetEnvironmentRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                name: "FOO".to_owned(),
                value: "bar".to_owned(),
                mode: None,
                hidden: false,
                format: false,
            })))
            .await,
        Response::SetEnvironment(_)
    ));

    let root = root.to_string_lossy().into_owned();
    let response = handler
        .handle(Request::IfShell(Box::new(IfShellRequest {
            condition: format!(
                "if not \"%FOO%\"==\"bar\" exit /b 1 & if not \"%CD%\"==\"{root}\" exit /b 1 & exit /b 0"
            ),
            format_mode: false,
            then_command: "set-buffer -b chosen yes".to_owned(),
            else_command: Some("set-buffer -b chosen no".to_owned()),
            target: Some(Target::Session(alpha)),
            caller_cwd: Some(PathBuf::from(root)),
            background: false,
        })))
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
        .handle(Request::IfShell(Box::new(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "set-buffer -b hyphen -value".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        })))
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
        .handle(Request::IfShell(Box::new(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "wait-for -S -channel".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        })))
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
        .handle(Request::IfShell(Box::new(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: format!("run-shell -- {}", command_quote(&shell_success_command())),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        })))
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
        .handle(Request::IfShell(Box::new(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "set-buffer -b one first; set-buffer -b two second".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        })))
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
    let parsed = CommandParser::new()
        .parse("if-shell -F 1 { FOO=bar } ; run-shell \"exit 0\"")
        .expect("commands parse");

    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue succeeds");

    assert!(output.stdout().is_empty());

    let state = handler.state.lock().await;
    assert_eq!(state.environment.global_value("FOO"), Some("bar"));
}
