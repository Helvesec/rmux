use super::*;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};
use crate::handler::scripting_support::{
    CONTROL_QUEUE_INSERTED_COMMAND_LIMIT, CONTROL_QUEUE_STDOUT_LIMIT,
};
use crate::handler::ControlRegistration;
use crate::outer_terminal::OuterTerminalContext;
use rmux_core::LifecycleEvent;
use rmux_os::identity::UserIdentity;
use rmux_proto::{HookLifecycle, SetBufferRequest, SetHookRequest};
use tokio::sync::mpsc;

async fn wait_for_queued_window_presence(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
    present: bool,
) -> Option<rmux_core::WindowId> {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let window_id = handler
                .state
                .lock()
                .await
                .sessions
                .session(session_name)
                .and_then(|session| session.window_at(window_index))
                .map(|window| window.id());
            if window_id.is_some() == present {
                return window_id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("follow-on queued window mutation becomes observable")
}

#[tokio::test]
async fn parsed_queue_assignments_apply_before_following_commands() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("FOO=bar ; run-shell \"exit 0\"")
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
    let _control_events = register_read_only_control_client(&handler, requester_pid).await;
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
async fn read_only_control_rejects_nested_assignment_in_an_unselected_runtime_branch() {
    let handler = RequestHandler::new();
    let requester_pid = 42_002;
    let _control_events = register_read_only_control_client(&handler, requester_pid).await;
    let parsed = CommandParser::new()
        .parse("if-shell -F 0 { FOO=bar list-sessions }")
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
async fn nested_assignments_apply_at_parse_time_across_runtime_branches_and_hooks() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse(
            "if-shell -F 0 { FALSE_BRANCH=parsed } { ELSE_BRANCH=parsed } ; set-hook -g after-new-window { HOOK_BODY=parsed }",
        )
        .expect("commands parse");

    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queue succeeds");

    assert!(output.stdout().is_empty());
    let state = handler.state.lock().await;
    assert_eq!(
        state.environment.global_value("FALSE_BRANCH"),
        Some("parsed")
    );
    assert_eq!(
        state.environment.global_value("ELSE_BRANCH"),
        Some("parsed")
    );
    assert_eq!(state.environment.global_value("HOOK_BODY"), Some("parsed"));
}

#[tokio::test]
async fn read_only_control_rejects_special_queue_invocations() {
    let handler = RequestHandler::new();
    let requester_pid = 42_003;
    let _control_events = register_read_only_control_client(&handler, requester_pid).await;

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
    let _access =
        handler.begin_test_detached_requester_access(requester_pid, AccessMode::ReadWrite);
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
    let _access = handler.begin_test_detached_requester_access(requester_pid, AccessMode::ReadOnly);
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
async fn control_queue_rejects_excessive_runtime_command_insertion() {
    let handler = RequestHandler::new();
    let requester_pid = 424_005;
    let _control_events = register_control_client(&handler, requester_pid, true).await;
    let inserted = "start-server ;".repeat(CONTROL_QUEUE_INSERTED_COMMAND_LIMIT + 1);
    let parsed = CommandParser::new()
        .parse(&format!("if-shell -F 1 '{inserted}'"))
        .expect("dynamic command list parses as one initial command");

    let result = handler
        .execute_control_commands(requester_pid, parsed)
        .await;

    let error = result
        .error
        .expect("runtime insertion beyond the aggregate limit must fail");
    assert!(
        error.to_string().contains("inserted too many commands"),
        "{error}"
    );
    assert_eq!(result.exit_status, Some(1));
}

#[tokio::test]
async fn control_queue_bounds_aggregate_stdout_before_extension() {
    let handler = RequestHandler::new();
    let requester_pid = 424_006;
    let _control_events = register_control_client(&handler, requester_pid, true).await;
    let chunk = vec![b'x'; CONTROL_QUEUE_STDOUT_LIMIT / 2 + 1];
    let response = handler
        .handle(Request::SetBuffer(Box::new(SetBufferRequest {
            name: Some("control-limit".to_owned()),
            content: chunk.clone(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })))
        .await;
    assert!(matches!(response, Response::SetBuffer(_)), "{response:?}");
    let parsed = CommandParser::new()
        .parse("show-buffer -b control-limit ; show-buffer -b control-limit")
        .expect("bounded output commands parse");

    let result = handler
        .execute_control_commands(requester_pid, parsed)
        .await;

    let error = result
        .error
        .expect("aggregate control stdout beyond the limit must fail");
    assert!(error.to_string().contains("stdout exceeds"), "{error}");
    assert_eq!(
        result.stdout, chunk,
        "the rejected output must not be appended"
    );
    assert_eq!(result.exit_status, Some(1));
}

#[tokio::test]
async fn read_only_control_allows_list_panes_all_observation() {
    let handler = RequestHandler::new();
    let requester_pid = 42_004;
    let _control_events = register_read_only_control_client(&handler, requester_pid).await;
    let parsed = CommandParser::new()
        .parse("list-panes -a")
        .expect("commands parse");

    let result = handler
        .execute_control_commands(requester_pid, parsed)
        .await;

    assert_eq!(result.error, None);
}

#[tokio::test]
async fn read_only_control_allows_list_windows_all_observation() {
    let handler = RequestHandler::new();
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name("read-only-windows"),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    let requester_pid = 42_104;
    let _control_events = register_read_only_control_client(&handler, requester_pid).await;
    let parsed = CommandParser::new()
        .parse("list-windows -a -F '#{session_name}:#{window_index}'")
        .expect("commands parse");

    let result = handler
        .execute_control_commands(requester_pid, parsed)
        .await;

    assert_eq!(result.error, None);
    assert_eq!(result.stdout, b"read-only-windows:0\n");
}

#[tokio::test]
async fn compact_short_options_execute_in_control_queue() {
    let handler = RequestHandler::new();
    let requester_pid = 42_005;
    let _control_events = register_control_client(&handler, requester_pid, true).await;
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

    let commands = handler
        .parse_control_commands("run-shell -Ctalpha:0.0 'set-buffer -b compact-control ok'")
        .await
        .expect("control command parses");
    let result = handler
        .execute_control_commands(requester_pid, commands)
        .await;

    assert_eq!(result.error, None, "compact control command should execute");
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("compact-control".to_owned()),
            }))
            .await
            .command_output()
            .expect("compact control buffer")
            .stdout(),
        b"ok"
    );
}

#[tokio::test]
async fn compact_short_options_execute_in_detached_queue() {
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
    let commands = CommandParser::new()
        .parse("capture-pane -epJtalpha:0.0")
        .expect("detached queue command parses");

    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), commands)
        .await
        .expect("compact detached queue command should execute");

    assert!(!output.stdout().is_empty());
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

async fn register_read_only_control_client(
    handler: &RequestHandler,
    requester_pid: u32,
) -> mpsc::Receiver<ControlServerEvent> {
    register_control_client(handler, requester_pid, false).await
}

async fn register_control_client(
    handler: &RequestHandler,
    requester_pid: u32,
    can_write: bool,
) -> mpsc::Receiver<ControlServerEvent> {
    let (event_tx, event_rx) = mpsc::channel::<ControlServerEvent>(CONTROL_SERVER_EVENT_CAPACITY);
    handler
        .register_control_with_access(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: rmux_proto::ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            ControlRegistration {
                event_tx,
                closing: Arc::new(AtomicBool::new(false)),
                uid: 1000,
                user: UserIdentity::Uid(1000),
                can_write,
            },
        )
        .await
        .expect("control registration succeeds");
    event_rx
}

#[tokio::test]
async fn if_shell_inserted_hidden_assignments_stay_out_of_process_environments() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("if-shell -F 1 { %hidden SECRET=classified } ; run-shell \"exit 0\"")
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
async fn parsed_queue_new_window_prepares_linked_identity_before_same_slot_reuse() {
    let handler = Arc::new(RequestHandler::new());
    let alpha = session_name("queued-new-window-slot-reuse");
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
    let hook = handler
        .handle(Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Global,
            hook: HookName::WindowLinked,
            command: "kill-window".to_owned(),
            lifecycle: HookLifecycle::Persistent,
        }))
        .await;
    assert!(matches!(hook, Response::SetHook(_)), "{hook:?}");
    let mut events = handler.subscribe_lifecycle_events();
    let pause = handler.install_window_lifecycle_emit_pause();
    let parsed = CommandParser::new()
        .parse("new-window -d -t queued-new-window-slot-reuse:1")
        .expect("commands parse");

    let queue_handler = Arc::clone(&handler);
    let creating = tokio::spawn(async move {
        queue_handler
            .execute_parsed_commands_for_test(std::process::id(), parsed)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("queued new-window reaches post-commit lifecycle pause");

    let remove_handler = Arc::clone(&handler);
    let remove_alpha = alpha.clone();
    let removing = tokio::spawn(async move {
        remove_handler
            .handle_kill_window(KillWindowRequest {
                target: WindowTarget::with_window(remove_alpha, 1),
                kill_all_others: false,
            })
            .await
    });
    assert!(
        wait_for_queued_window_presence(&handler, &alpha, 1, false)
            .await
            .is_none(),
        "original queued window is removed"
    );
    let replacement_handler = Arc::clone(&handler);
    let replacement_alpha = alpha.clone();
    let replacing = tokio::spawn(async move {
        replacement_handler
            .handle_new_window(
                std::process::id(),
                NewWindowRequest {
                    target: replacement_alpha,
                    name: None,
                    detached: true,
                    start_directory: None,
                    environment: None,
                    command: None,
                    process_command: None,
                    target_window_index: Some(1),
                    insert_at_target: false,
                },
            )
            .await
    });
    let replacement_id = wait_for_queued_window_presence(&handler, &alpha, 1, true)
        .await
        .expect("replacement window exists");
    while events.try_recv().is_ok() {}
    pause.release.notify_one();
    creating
        .await
        .expect("queued new-window task joins")
        .expect("queue succeeds");
    let removed = removing.await.expect("kill-window task joins");
    assert!(matches!(removed, Response::KillWindow(_)), "{removed:?}");
    let replacement = replacing.await.expect("replacement new-window task joins");
    assert!(
        matches!(replacement, Response::NewWindow(_)),
        "{replacement:?}"
    );

    let event = loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
            .await
            .expect("queued window-linked event arrives")
            .expect("lifecycle sender remains open");
        if matches!(
            &event.event,
            LifecycleEvent::WindowLinked { session_name, .. } if session_name == &alpha
        ) {
            break event;
        }
    };
    handler.dispatch_lifecycle_hook(event).await;
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(1))
            .expect("replacement slot survives the destructive hook")
            .id(),
        replacement_id
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
async fn parsed_queue_new_window_k_validates_environment_before_replacing_target() {
    let handler = RequestHandler::new();
    let alpha = session_name("new-window-k-env-validation");
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
        .parse("new-window -d -t new-window-k-env-validation:1 -n protected")
        .expect("window setup parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), setup)
        .await
        .expect("window setup succeeds");
    let protected_window_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .and_then(|session| session.window_at(1))
        .expect("protected window exists")
        .id();

    let invalid = CommandParser::new()
        .parse("new-window -d -k -t new-window-k-env-validation:1 -e NOT_AN_ASSIGNMENT")
        .expect("invalid environment reaches runtime validation");
    let error = handler
        .execute_parsed_commands_for_test(std::process::id(), invalid)
        .await
        .expect_err("invalid environment is rejected");

    assert!(
        error
            .to_string()
            .contains("environment assignment must be NAME=VALUE"),
        "{error}"
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
async fn parsed_queue_new_window_k_replaces_the_only_window_without_destroying_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("queued-new-window-k-only");
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
    let previous_window_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .and_then(|session| session.window_at(0))
        .expect("initial window exists")
        .id();

    let replace = CommandParser::new()
        .parse("new-window -d -k -t queued-new-window-k-only:0 -n replacement")
        .expect("queued replacement parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), replace)
        .await
        .expect("queued replacement succeeds");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session survives");
    assert_eq!(session.windows().len(), 1);
    let replacement = session
        .window_at(0)
        .expect("replacement keeps target index");
    assert_ne!(replacement.id(), previous_window_id);
    assert_eq!(replacement.name(), Some("replacement"));
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
async fn parsed_queue_compact_client_and_overlay_flags_preserve_their_meaning() {
    let handler = RequestHandler::new();
    let alpha = session_name("compact-flags");
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

    let state = handler.state.lock().await;
    let current = TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(alpha, 0, 0))));
    let parse = |command: &str, arguments: &[&str]| {
        crate::handler::scripting_support::parse_request_from_parts(
            command.to_owned(),
            arguments.iter().map(|value| (*value).to_owned()).collect(),
            None,
            &state.sessions,
            &state.options,
            &current,
        )
        .unwrap_or_else(|error| panic!("{command} compact flags should parse: {error}"))
    };

    let Request::DetachClientExt(request) = parse("detach-client", &["-aP"]) else {
        panic!("expected detach-client request");
    };
    assert!(request.all_other_clients);
    assert!(request.kill_on_detach);

    let Request::RefreshClient(request) = parse(
        "refresh-client",
        &[
            "-lS",
            "-C80x24",
            "-factive-pane",
            "-Fno-detach-on-destroy",
            "-t=",
        ],
    ) else {
        panic!("expected refresh-client request");
    };
    assert!(request.clipboard_query);
    assert!(request.status_only);
    assert_eq!(request.control_size.as_deref(), Some("80x24"));
    assert_eq!(request.flags.as_deref(), Some("active-pane"));
    assert_eq!(request.flags_alias.as_deref(), Some("no-detach-on-destroy"));
    assert_eq!(request.target_client.as_deref(), Some("="));

    let Request::SwitchClientExt3(request) = parse("switch-client", &["-Er"]) else {
        panic!("expected switch-client request");
    };
    assert!(request.skip_environment_update);
    assert!(request.toggle_read_only);

    let Request::ListClients(request) = parse("list-clients", &["-rFclient"]) else {
        panic!("expected list-clients request");
    };
    assert!(request.reversed);
    assert_eq!(request.format.as_deref(), Some("client"));

    let Request::ServerAccess(request) = parse("server-access", &["-lr"]) else {
        panic!("expected server-access request");
    };
    assert!(request.list);
    assert!(request.read_only);

    let Request::DisplayPanes(request) = parse("display-panes", &["-bN"]) else {
        panic!("expected display-panes request");
    };
    assert!(request.non_blocking);
    assert!(request.no_command);
    assert_eq!(request.template, None);

    let Request::LastPane(request) = parse("last-pane", &["-de"]) else {
        panic!("expected last-pane request");
    };
    assert_eq!(request.input_disabled, Some(false));
}

#[tokio::test]
async fn parsed_queue_refresh_client_unsupported_fields_are_rejected() {
    let handler = RequestHandler::new();
    for (command, expected) in [
        (
            "refresh-client -A pane:on",
            "command refresh-client: unknown flag -A",
        ),
        (
            "refresh-client -B name:pane:format",
            "command refresh-client: unknown flag -B",
        ),
        (
            "refresh-client -r pane:rgb",
            "command refresh-client: unknown flag -r",
        ),
        (
            "refresh-client -c",
            "command refresh-client: unknown flag -c",
        ),
        (
            "refresh-client -D",
            "command refresh-client: unknown flag -D",
        ),
        (
            "refresh-client -L",
            "command refresh-client: unknown flag -L",
        ),
        (
            "refresh-client -R",
            "command refresh-client: unknown flag -R",
        ),
        (
            "refresh-client -U",
            "command refresh-client: unknown flag -U",
        ),
        (
            "refresh-client 10",
            "unexpected argument '10' for refresh-client",
        ),
    ] {
        let parsed = CommandParser::new()
            .parse(command)
            .expect("generic command parser preserves command arguments");

        let error = handler
            .execute_parsed_commands_for_test(std::process::id(), parsed)
            .await
            .expect_err("reserved refresh-client flag must fail during request parsing");

        assert_eq!(error, rmux_proto::RmuxError::Server(expected.to_owned()));
    }
}

#[tokio::test]
async fn control_queue_refresh_client_pan_fields_fail_closed() {
    let handler = RequestHandler::new();
    let requester_pid = 42_006;
    let _control_events = register_control_client(&handler, requester_pid, true).await;

    for (command, expected) in [
        (
            "refresh-client -c",
            "server error: command refresh-client: unknown flag -c",
        ),
        (
            "refresh-client -D",
            "server error: command refresh-client: unknown flag -D",
        ),
        (
            "refresh-client -L",
            "server error: command refresh-client: unknown flag -L",
        ),
        (
            "refresh-client -R",
            "server error: command refresh-client: unknown flag -R",
        ),
        (
            "refresh-client -U",
            "server error: command refresh-client: unknown flag -U",
        ),
        (
            "refresh-client 10",
            "server error: unexpected argument '10' for refresh-client",
        ),
    ] {
        let parsed = CommandParser::new()
            .parse(command)
            .expect("generic command parser preserves command arguments");
        let result = handler
            .execute_control_commands(requester_pid, parsed)
            .await;

        assert_eq!(
            result
                .error
                .unwrap_or_else(|| panic!("{command} must fail in control queue"))
                .to_string(),
            expected
        );
        assert_eq!(result.exit_status, None);
    }
}

#[tokio::test]
async fn parsed_queue_server_access_rejects_runtime_invalid_flags() {
    let handler = RequestHandler::new();
    for (command, expected) in [
        (
            "server-access -t%0 -l",
            "command server-access: unknown flag -t",
        ),
        (
            "server-access -ad nobody",
            "-a and -d cannot be used together",
        ),
        (
            "server-access -rw nobody",
            "-r and -w cannot be used together",
        ),
    ] {
        let parsed = CommandParser::new()
            .parse(command)
            .expect("generic command parser preserves server-access arguments");

        let error = handler
            .execute_parsed_commands_for_test(std::process::id(), parsed)
            .await
            .expect_err("tmux-invalid server-access flags must fail during request parsing");

        assert_eq!(error, rmux_proto::RmuxError::Server(expected.to_owned()));
    }
}

#[tokio::test]
async fn parsed_queue_server_access_list_ignores_conflicting_flags() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("server-access -ladrw nobody")
        .expect("generic command parser preserves server-access arguments");

    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("server-access list mode ignores mutation flag conflicts like tmux");
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
