use super::*;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::control::{ControlModeUpgrade, ControlServerEvent};
use crate::handler::ControlRegistration;
use crate::outer_terminal::OuterTerminalContext;
use rmux_os::identity::UserIdentity;
use tokio::sync::mpsc;

#[tokio::test]
async fn parsed_queue_assignments_apply_before_following_commands() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("FOO=bar ; run-shell true")
        .expect("commands parse");

    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue succeeds");

    assert!(output.stdout().is_empty());

    let state = handler.state.lock().await;
    assert_eq!(state.environment.global_value("FOO"), Some("bar"));
}

#[tokio::test]
async fn read_only_control_rejects_parse_time_assignments() {
    let handler = RequestHandler::new();
    let requester_pid = 42_001;
    register_read_only_control_client(&handler, requester_pid).await;
    let parsed = CommandParser::new()
        .parse("FOO=bar list-sessions")
        .expect("commands parse");

    let result = handler
        .execute_control_commands(requester_pid, parsed)
        .await;

    assert_eq!(
        result
            .error
            .expect("read-only assignment is rejected")
            .to_string(),
        "server error: client is read-only"
    );
    let state = handler.state.lock().await;
    assert_eq!(state.environment.global_value("FOO"), None);
}

#[tokio::test]
async fn read_only_control_rejects_inserted_parse_time_assignments() {
    let handler = RequestHandler::new();
    let requester_pid = 42_002;
    register_read_only_control_client(&handler, requester_pid).await;
    let parsed = CommandParser::new()
        .parse("if-shell -F 1 { FOO=bar list-sessions }")
        .expect("commands parse");

    let result = handler
        .execute_control_commands(requester_pid, parsed)
        .await;

    assert_eq!(
        result
            .error
            .expect("inserted read-only assignment is rejected")
            .to_string(),
        "server error: client is read-only"
    );
    let state = handler.state.lock().await;
    assert_eq!(state.environment.global_value("FOO"), None);
}

#[tokio::test]
async fn read_only_control_rejects_special_queue_invocations() {
    let handler = RequestHandler::new();
    let requester_pid = 42_003;
    register_read_only_control_client(&handler, requester_pid).await;

    for command in [
        "if-shell -F 1 { list-sessions }",
        "source-file /definitely/missing-rmux.conf",
        "clear-prompt-history",
    ] {
        let parsed = CommandParser::new().parse(command).expect("commands parse");

        let result = handler
            .execute_control_commands(requester_pid, parsed)
            .await;

        assert_eq!(
            result
                .error
                .unwrap_or_else(|| panic!("{command} should be rejected"))
                .to_string(),
            "server error: client is read-only"
        );
    }
}

#[tokio::test]
async fn unknown_requester_rejects_parse_time_assignments() {
    let handler = RequestHandler::new();
    let requester_pid = 424_001;
    let parsed = CommandParser::new()
        .parse("FOO=bar list-sessions")
        .expect("commands parse");

    let result = handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await;

    assert_eq!(
        result
            .expect_err("unknown requester should be read-only")
            .to_string(),
        "server error: client is read-only"
    );
    let state = handler.state.lock().await;
    assert_eq!(state.environment.global_value("FOO"), None);
}

#[tokio::test]
async fn unknown_requester_rejects_special_queue_invocations() {
    let handler = RequestHandler::new();
    let requester_pid = 424_002;

    for command in [
        "if-shell -F 1 { list-sessions }",
        "source-file /definitely/missing-rmux.conf",
        "clear-prompt-history",
    ] {
        let parsed = CommandParser::new().parse(command).expect("commands parse");

        let result = handler
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await;

        let error = match result {
            Ok(_) => panic!("{command} should be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "server error: client is read-only");
    }
}

#[tokio::test]
async fn detached_write_requester_allows_mutating_queue_commands() {
    let handler = RequestHandler::new();
    let requester_pid = 424_003;
    let _access = handler.begin_detached_requester_access(requester_pid, true);
    let parsed = CommandParser::new()
        .parse("set-buffer -b repro-buffer hello ; show-buffer -b repro-buffer")
        .expect("commands parse");

    let output = handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await
        .expect("authenticated detached requester can mutate");

    assert_eq!(String::from_utf8(output.stdout).expect("utf8"), "hello");
}

#[tokio::test]
async fn detached_read_only_requester_rejects_mutating_queue_commands() {
    let handler = RequestHandler::new();
    let requester_pid = 424_004;
    let _access = handler.begin_detached_requester_access(requester_pid, false);
    let parsed = CommandParser::new()
        .parse("set-buffer -b repro-buffer hello ; show-buffer -b repro-buffer")
        .expect("commands parse");

    let result = handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await;

    assert_eq!(
        result
            .expect_err("read-only detached requester should be rejected")
            .to_string(),
        "server error: client is read-only"
    );
}

#[tokio::test]
async fn read_only_control_allows_list_panes_all_observation() {
    let handler = RequestHandler::new();
    let requester_pid = 42_004;
    register_read_only_control_client(&handler, requester_pid).await;
    let parsed = CommandParser::new()
        .parse("list-panes -a")
        .expect("commands parse");

    let result = handler
        .execute_control_commands(requester_pid, parsed)
        .await;

    assert_eq!(result.error, None);
}

#[tokio::test]
async fn parsed_queue_lock_client_defaults_to_current_client() {
    let handler = RequestHandler::new();
    let alpha = SessionName::new("alpha").expect("valid session name");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(std::process::id(), alpha, control_tx)
        .await;

    let parsed = CommandParser::new()
        .parse("lock-client")
        .expect("commands parse");

    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue succeeds");

    assert!(output.stdout().is_empty());
}

async fn register_read_only_control_client(handler: &RequestHandler, requester_pid: u32) {
    let (event_tx, _event_rx) = mpsc::unbounded_channel::<ControlServerEvent>();
    handler
        .register_control_with_access(
            requester_pid,
            ControlModeUpgrade {
                mode: rmux_proto::ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            ControlRegistration {
                event_tx,
                closing: Arc::new(AtomicBool::new(false)),
                uid: 1000,
                user: UserIdentity::Uid(1000),
                can_write: false,
            },
        )
        .await;
}

#[tokio::test]
async fn if_shell_inserted_hidden_assignments_stay_out_of_process_environments() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("if-shell -F 1 { %hidden SECRET=classified } ; run-shell true")
        .expect("commands parse");

    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue succeeds");

    assert!(output.stdout().is_empty());

    let state = handler.state.lock().await;
    let entries = state
        .environment
        .show_environment_entries(&ScopeSelector::Global, true, Some("SECRET"))
        .expect("hidden show-environment succeeds");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].value.as_deref(), Some("classified"));
    let mut process_environment =
        std::collections::HashMap::from([("SECRET".to_owned(), "client".to_owned())]);
    state
        .environment
        .apply_to_process_environment(None, &mut process_environment);
    assert_eq!(process_environment.get("SECRET"), None);
}

#[tokio::test]
async fn queue_error_aborts_later_commands_in_the_same_group_only() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("show-buffer -b missing ; set-buffer -b skipped no\nset-buffer -b kept yes")
        .expect("commands parse");

    let result = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await;

    assert!(result.is_err());
    assert!(matches!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("skipped".to_owned()),
            }))
            .await,
        Response::Error(_)
    ));
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("kept".to_owned()),
            }))
            .await
            .command_output()
            .expect("kept buffer output")
            .stdout(),
        b"yes"
    );
}

#[tokio::test]
async fn parsed_queue_set_buffer_accepts_target_and_rename_trailing_content() {
    let handler = RequestHandler::new();
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name("alpha"),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let parsed = CommandParser::new()
        .parse(
            "set-buffer -t alpha target-tolerated; \
             set-buffer -b src original; \
             set-buffer -b src -n dst ignored",
        )
        .expect("commands parse");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue succeeds");
    assert!(output.stdout().is_empty());

    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
            .await
            .command_output()
            .expect("default buffer output")
            .stdout(),
        b"target-tolerated"
    );
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("dst".to_owned()),
            }))
            .await
            .command_output()
            .expect("renamed buffer output")
            .stdout(),
        b"original"
    );
}

#[tokio::test]
async fn if_shell_uses_preparsed_brace_command_lists_at_execution_time() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("if-shell -F 1 { show-buffer -b missing\nset-buffer -b kept yes }")
        .expect("commands parse");

    let result = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await;

    assert!(result.is_err());
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("kept".to_owned()),
            }))
            .await
            .command_output()
            .expect("kept buffer output")
            .stdout(),
        b"yes"
    );
}

#[tokio::test]
async fn if_shell_inserted_brace_errors_do_not_abort_parent_line_tail() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("if-shell -F 1 { show-buffer -b missing } ; set-buffer -b kept yes")
        .expect("commands parse");

    let result = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await;

    assert!(result.is_err());
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("kept".to_owned()),
            }))
            .await
            .command_output()
            .expect("kept buffer output")
            .stdout(),
        b"yes"
    );
}

#[tokio::test]
async fn if_shell_string_mode_newlines_share_one_abort_group() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::IfShell(Box::new(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "show-buffer -b missing\nset-buffer -b skipped no".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        })))
        .await;

    assert!(matches!(response, Response::Error(_)));
    assert!(matches!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("skipped".to_owned()),
            }))
            .await,
        Response::Error(_)
    ));
}

#[tokio::test]
async fn parsed_queue_resolves_unresolved_window_targets_before_protocol_dispatch() {
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
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: alpha.clone(),
                name: Some("logs".to_owned()),
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

    let parsed = CommandParser::new()
        .parse("rename-window -t alp:1 renamed")
        .expect("commands parse");

    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue command succeeds");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window_at(1)
            .expect("window exists")
            .name(),
        Some("renamed")
    );
}

#[tokio::test]
async fn parsed_queue_resolves_session_only_new_window_targets_at_protocol_boundary() {
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
        .parse("new-window -t alp -d -n logs")
        .expect("commands parse");

    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue command succeeds");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window_at(1)
            .expect("window exists")
            .name(),
        Some("logs")
    );
}

#[tokio::test]
async fn parsed_queue_resolves_session_colon_new_window_targets_at_protocol_boundary() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha-colon");
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
        .parse("new-window -t alpha-col: -d -n logs")
        .expect("commands parse");

    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue command resolves session: targets through target-find");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window_at(1)
            .expect("window exists")
            .name(),
        Some("logs")
    );
}

#[tokio::test]
async fn parsed_queue_keeps_signed_new_window_targets_relative() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha-relative");
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
        .parse(
            "new-window -d -t alpha-relative:1 -n one ; \
             select-window -t alpha-relative:1 ; \
             new-window -d -t alpha-relative:+1 -n rel",
        )
        .expect("commands parse");

    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("relative target should not be treated as absolute index 1");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session.window_at(1).and_then(|window| window.name()),
        Some("one")
    );
    assert_eq!(
        session.window_at(2).and_then(|window| window.name()),
        Some("rel")
    );
}

#[tokio::test]
async fn parsed_queue_accepts_compact_new_window_flags() {
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
        .parse(
            "new-window -ad -n after0 ; \
             new-window -dn named ; \
             new-window -adt alpha:1 -n after1",
        )
        .expect("compact new-window commands parse");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("compact new-window flags should execute");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session.window_at(1).and_then(|window| window.name()),
        Some("after0")
    );
    assert_eq!(
        session.window_at(2).and_then(|window| window.name()),
        Some("after1")
    );
    assert_eq!(
        session.window_at(3).and_then(|window| window.name()),
        Some("named")
    );
}

#[tokio::test]
async fn parsed_queue_new_window_before_beats_after_like_tmux() {
    for flags in ["-b -a", "-ba"] {
        let handler = RequestHandler::new();
        let session = session_name("alpha");
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

        let parsed = CommandParser::new()
            .parse(&format!("new-window {flags} -t alpha:0 -n inserted"))
            .expect("new-window command parses");
        handler
            .execute_parsed_commands_for_test(std::process::id(), parsed)
            .await
            .unwrap_or_else(|error| panic!("new-window {flags} should execute: {error}"));

        let state = handler.state.lock().await;
        let session = state.sessions.session(&session).expect("session exists");
        assert_eq!(
            session.window_at(0).and_then(|window| window.name()),
            Some("inserted"),
            "{flags} must insert before the target like tmux"
        );
        assert!(
            session.window_at(1).is_some(),
            "{flags} must push the original target window to index 1"
        );
    }
}

#[tokio::test]
async fn parsed_queue_accepts_compact_break_pane_flag_clusters() {
    for (session, flags) in [
        ("breakdprint", "-dP"),
        ("breakafter", "-adP"),
        ("breakprintafter", "-Pad"),
    ] {
        let handler = RequestHandler::new();
        let alpha = session_name(session);
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
            .parse(&format!(
                "split-window -d -t {session}:0.0 ; break-pane {flags} -s {session}:0.1"
            ))
            .expect("compact break-pane command parses");
        let output = handler
            .execute_parsed_commands_for_test(std::process::id(), parsed)
            .await
            .unwrap_or_else(|error| {
                panic!("break-pane {flags} should execute with compact flags: {error}")
            });

        assert!(
            String::from_utf8_lossy(output.stdout()).starts_with(&format!("{session}:")),
            "break-pane {flags} should print its target, got {:?}",
            output.stdout()
        );
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        assert_eq!(session.windows().len(), 2);
    }
}

#[tokio::test]
async fn parsed_queue_accepts_compact_kill_window_and_kill_pane_targets() {
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

    let setup = CommandParser::new()
        .parse("new-window -d -n keep ; new-window -d -n remove")
        .expect("window setup parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), setup)
        .await
        .expect("window setup succeeds");

    let kill_window = CommandParser::new()
        .parse("kill-window -at alpha:1")
        .expect("compact kill-window parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), kill_window)
        .await
        .expect("compact kill-window target should execute");

    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(alpha.clone(), 1, 0)),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));

    let kill_pane = CommandParser::new()
        .parse("kill-pane -at alpha:1.1")
        .expect("compact kill-pane parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), kill_pane)
        .await
        .expect("compact kill-pane target should execute");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(
        session
            .window_at(1)
            .expect("target window exists")
            .pane_count(),
        1
    );
}

#[tokio::test]
async fn parsed_queue_uses_current_target_for_new_window_split_and_zoom() {
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
    let current_target = Target::Pane(PaneTarget::with_window(alpha.clone(), 0, 0));

    for command in ["new-window -d -n logs", "split-window -h", "resize-pane -Z"] {
        let parsed = CommandParser::new().parse(command).expect("command parses");
        handler
            .execute_parsed_commands(
                std::process::id(),
                parsed,
                QueueExecutionContext::without_caller_cwd()
                    .with_current_target(Some(current_target.clone())),
            )
            .await
            .unwrap_or_else(|error| {
                panic!("{command} should succeed with current target: {error}")
            });
    }

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session.windows().len(),
        2,
        "new-window and split-window should both apply"
    );
    assert!(
        session.window_at(0).expect("window 0 exists").is_zoomed(),
        "resize-pane -Z should zoom the current pane"
    );
    assert_eq!(
        session.window_at(0).expect("window 0 exists").pane_count(),
        2,
        "split-window should split the current window without -t"
    );
}

#[tokio::test]
async fn parsed_queue_reports_missing_target_client_before_input() {
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

    for command in [
        "send-keys -c 999999 -t alpha:0.0 echo SHOULD_NOT_TYPE",
        "display-message -c 999999 hello",
    ] {
        let parsed = CommandParser::new()
            .parse(command)
            .expect("command parses at queue layer");
        let output = handler
            .execute_parsed_commands_for_test(std::process::id(), parsed)
            .await
            .expect("missing target-client is a tmux-compatible noop");

        assert!(output.stdout().is_empty());
    }

    let parsed = CommandParser::new()
        .parse("display-message -p -c 999999 hello")
        .expect("command parses at queue layer");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("print with missing target-client still succeeds");

    assert_eq!(output.stdout(), b"hello\n");
}

#[tokio::test]
async fn parsed_queue_uses_current_target_for_display_panes_without_t() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = 52_u32;
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
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let parsed = CommandParser::new()
        .parse("display-panes")
        .expect("command parses");
    handler
        .execute_parsed_commands(
            requester_pid,
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Pane(PaneTarget::with_window(alpha, 0, 0)))),
        )
        .await
        .expect("display-panes should use the current target");

    let _overlay = control_rx.recv().await.expect("display-panes overlay");
}

#[tokio::test]
async fn parsed_queue_display_panes_t_reports_target_client_errors_like_cli() {
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

    for (command, expected) in [
        ("display-panes -t 999999", "can't find client: 999999"),
        ("display-panes -t alpha", "can't find client: alpha"),
        ("display-panes -t alpha:0", "can't find client: alpha:0"),
        ("display-panes -t alpha:", "can't find client: alpha"),
    ] {
        let parsed = CommandParser::new()
            .parse(command)
            .expect("display-panes command parses");
        let error = handler
            .execute_parsed_commands_for_test(std::process::id(), parsed)
            .await
            .expect_err("missing target-client should fail");

        assert_eq!(error, rmux_proto::RmuxError::Message(expected.to_owned()));
    }
}

#[tokio::test]
async fn parsed_queue_refresh_client_r_is_unknown_flag_like_tmux() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("refresh-client -r %0")
        .expect("refresh-client command parses");

    let error = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect_err("refresh-client -r should be rejected");

    assert_eq!(
        error,
        rmux_proto::RmuxError::Server("command refresh-client: unknown flag -r".to_owned())
    );
}

#[tokio::test]
async fn parsed_queue_uses_current_target_for_kill_pane_without_t() {
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

    let parsed = CommandParser::new()
        .parse("kill-pane")
        .expect("command parses");
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd().with_current_target(Some(Target::Pane(
                PaneTarget::with_window(alpha.clone(), 0, 1),
            ))),
        )
        .await
        .expect("kill-pane should use the current pane target");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session.window_at(0).expect("window exists").pane_count(),
        1,
        "kill-pane without -t should remove the current pane"
    );
}
