use super::*;

#[tokio::test]
async fn parsed_queue_accepts_display_message_format_flag() {
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
        .parse("display-message -p -F '#{session_name}' -t alpha")
        .expect("commands parse");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("display-message -F executes");

    assert_eq!(output.stdout(), b"alpha\n");
}

#[tokio::test]
async fn parsed_queue_display_message_consumes_neutral_compat_flags_before_message() {
    let handler = RequestHandler::new();

    let parsed = CommandParser::new()
        .parse("display-message -p -d 10 -I -l -N -v hello")
        .expect("display-message compat flags parse");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("display-message compat flags execute");

    assert_eq!(
        output.stdout(),
        b"# expanding format: hello\n# result is: hello\nhello\n"
    );
}

#[tokio::test]
async fn parsed_queue_display_message_rejects_multiple_message_arguments() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("display-message a b c")
        .expect("command parser keeps queue validation for execution");
    let error = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect_err("display-message must accept at most one message argument");

    assert!(
        error
            .to_string()
            .contains("command display-message: too many arguments (need at most 1)"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn parsed_queue_display_message_consumes_compact_delay_before_message() {
    let handler = RequestHandler::new();

    let parsed = CommandParser::new()
        .parse("display-message -d0 -p hello")
        .expect("display-message compact delay flag parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("display-message compact delay flag executes");

    assert_eq!(output.stdout(), b"hello\n");
}

#[tokio::test]
async fn parsed_queue_display_message_accepts_compact_print_target_cluster() {
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
        .parse("display-message -pt alpha:0.0 'hi-#{pane_index}'")
        .expect("display-message -pt cluster parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("display-message -pt cluster executes");

    assert_eq!(output.stdout(), b"hi-0\n");
}

#[tokio::test]
async fn parsed_queue_display_message_c_missing_client_is_noop() {
    let handler = RequestHandler::new();

    let parsed = CommandParser::new()
        .parse("display-message -c 999999 hello")
        .expect("display-message -c parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("missing target-client is a tmux-compatible noop");

    assert!(output.stdout().is_empty());
}

#[tokio::test]
async fn parsed_queue_display_message_print_ignores_missing_target_client() {
    let handler = RequestHandler::new();

    let parsed = CommandParser::new()
        .parse("display-message -p -c 999999 hello")
        .expect("display-message -p -c parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("print with missing target-client still succeeds");

    assert_eq!(output.stdout(), b"hello\n");
}

#[tokio::test]
async fn parsed_queue_send_keys_c_missing_client_is_noop() {
    let handler = RequestHandler::new();

    let parsed = CommandParser::new()
        .parse("send-keys -c 999999 A")
        .expect("send-keys -c parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("missing target-client is a tmux-compatible noop");

    assert!(output.stdout().is_empty());
}

#[tokio::test]
async fn parsed_queue_display_message_all_formats_uses_core_inventory() {
    let handler = RequestHandler::new();

    let parsed = CommandParser::new()
        .parse("display-message -a")
        .expect("display-message -a parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("display-message -a executes");
    let stdout = std::str::from_utf8(output.stdout()).expect("display-message -a utf8");

    assert!(stdout.contains("client_session="), "{stdout}");
    assert!(stdout.contains("window_offset_x="), "{stdout}");
    assert!(stdout.contains("command=display-message"), "{stdout}");
}

#[tokio::test]
async fn parsed_queue_list_panes_all_does_not_require_current_target() {
    let handler = RequestHandler::new();
    for name in ["alpha", "beta"] {
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

    let parsed = CommandParser::new()
        .parse("list-panes -a -F '#{session_name}:#{pane_index}'")
        .expect("list-panes -a parses");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("list-panes -a executes");
    let stdout = std::str::from_utf8(output.stdout()).expect("list-panes utf8");

    assert!(stdout.contains("alpha:0\n"), "{stdout}");
    assert!(stdout.contains("beta:0\n"), "{stdout}");
}

#[tokio::test]
async fn parse_control_commands_rejects_invalid_prompt_history_type() {
    let handler = RequestHandler::new();

    let parsed = handler
        .parse_control_commands("show-prompt-history -T bogus")
        .await
        .expect("command should parse before execution");
    let error = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect_err("invalid prompt type should fail during execution");

    assert_eq!(
        error,
        rmux_proto::RmuxError::Server("invalid type: bogus".to_owned())
    );
}

#[tokio::test]
async fn parsed_queue_set_environment_requires_a_value() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse("set-environment -g TERM")
        .expect("commands parse");

    let error = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect_err("missing set-environment value should fail");

    assert_eq!(
        error,
        rmux_proto::RmuxError::Server("no value specified".to_owned())
    );
}

#[tokio::test]
async fn parsed_queue_set_environment_uses_current_session_by_default() {
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
            "set-environment -h SECRET classified; \
             show-environment SECRET; \
             show-environment -h SECRET",
        )
        .expect("commands parse");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("environment commands execute");

    assert_eq!(output.stdout(), b"SECRET=classified\n");
}

#[tokio::test]
async fn hook_string_mode_newlines_share_one_abort_group() {
    let handler = RequestHandler::new();

    let result = with_hook_execution(Vec::new(), async {
        handler
            .execute_hook_command(
                std::process::id(),
                "show-buffer -b missing\nset-buffer -b skipped no",
            )
            .await
    })
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
}

#[tokio::test]
async fn compact_short_options_execute_through_builtin_alias_in_hook() {
    let handler = RequestHandler::new();

    let result = with_hook_execution(Vec::new(), async {
        handler
            .execute_hook_command(std::process::id(), "server-info")
            .await
    })
    .await;

    assert!(
        result.is_ok(),
        "builtin compact alias should execute: {result:?}"
    );
}

#[tokio::test]
async fn wait_for_signal_wakes_current_waiters_and_latches_one_future_wait() {
    let handler = Arc::new(RequestHandler::new());
    let first_waiter = {
        let handler = Arc::clone(&handler);
        tokio::spawn(async move { handler.handle(wait_for("signal", WaitForMode::Wait)).await })
    };
    let second_waiter = {
        let handler = Arc::clone(&handler);
        tokio::spawn(async move { handler.handle(wait_for("signal", WaitForMode::Wait)).await })
    };
    yield_until_counts(&handler, "signal", (2, 0, false)).await;

    assert_eq!(
        handler
            .handle(wait_for("signal", WaitForMode::Signal))
            .await,
        Response::WaitFor(WaitForResponse)
    );
    assert_eq!(
        first_waiter.await.expect("first waiter task"),
        Response::WaitFor(WaitForResponse)
    );
    assert_eq!(
        second_waiter.await.expect("second waiter task"),
        Response::WaitFor(WaitForResponse)
    );
    yield_until_counts(&handler, "signal", (0, 0, false)).await;

    assert_eq!(
        handler
            .handle(wait_for("future", WaitForMode::Signal))
            .await,
        Response::WaitFor(WaitForResponse)
    );
    yield_until_counts(&handler, "future", (0, 0, true)).await;

    let future_waiter = {
        let handler = Arc::clone(&handler);
        tokio::spawn(async move { handler.handle(wait_for("future", WaitForMode::Wait)).await })
    };
    assert_eq!(
        future_waiter.await.expect("future waiter task"),
        Response::WaitFor(WaitForResponse)
    );
    yield_until_counts(&handler, "future", (0, 0, false)).await;

    let second_future_waiter = {
        let handler = Arc::clone(&handler);
        tokio::spawn(async move { handler.handle(wait_for("future", WaitForMode::Wait)).await })
    };
    yield_until_counts(&handler, "future", (1, 0, false)).await;
    assert!(!second_future_waiter.is_finished());
    second_future_waiter.abort();
    assert!(second_future_waiter
        .await
        .expect_err("waiter is cancelled")
        .is_cancelled());
    yield_until_counts(&handler, "future", (0, 0, false)).await;
}

#[tokio::test]
async fn wait_for_unlock_hands_locks_to_queued_waiters_in_fifo_order() {
    let handler = Arc::new(RequestHandler::new());

    assert_eq!(
        handler.handle(wait_for("lock", WaitForMode::Lock)).await,
        Response::WaitFor(WaitForResponse)
    );
    yield_until_counts(&handler, "lock", (0, 0, true)).await;

    let first = spawn_wait_for(&handler, "lock", WaitForMode::Lock);
    yield_until_counts(&handler, "lock", (0, 1, true)).await;
    let second = spawn_wait_for(&handler, "lock", WaitForMode::Lock);
    yield_until_counts(&handler, "lock", (0, 2, true)).await;

    assert_eq!(
        handler.handle(wait_for("lock", WaitForMode::Unlock)).await,
        Response::WaitFor(WaitForResponse)
    );
    assert_eq!(
        first.await.expect("first lock"),
        Response::WaitFor(WaitForResponse)
    );
    yield_until_counts(&handler, "lock", (0, 1, true)).await;
    assert!(!second.is_finished());

    assert_eq!(
        handler.handle(wait_for("lock", WaitForMode::Unlock)).await,
        Response::WaitFor(WaitForResponse)
    );
    assert_eq!(
        second.await.expect("second lock"),
        Response::WaitFor(WaitForResponse)
    );
    yield_until_counts(&handler, "lock", (0, 0, true)).await;

    assert_eq!(
        handler.handle(wait_for("lock", WaitForMode::Unlock)).await,
        Response::WaitFor(WaitForResponse)
    );
    yield_until_counts(&handler, "lock", (0, 0, false)).await;
}

#[tokio::test]
async fn wait_for_unlock_on_unlocked_channel_returns_error() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(wait_for("missing", WaitForMode::Unlock))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn wait_for_cancellation_removes_plain_and_lock_waiters() {
    let handler = Arc::new(RequestHandler::new());

    let plain = spawn_wait_for(&handler, "cancel-plain", WaitForMode::Wait);
    yield_until_counts(&handler, "cancel-plain", (1, 0, false)).await;
    plain.abort();
    assert!(plain
        .await
        .expect_err("plain waiter is cancelled")
        .is_cancelled());
    yield_until_counts(&handler, "cancel-plain", (0, 0, false)).await;

    assert_eq!(
        handler
            .handle(wait_for("cancel-lock", WaitForMode::Lock))
            .await,
        Response::WaitFor(WaitForResponse)
    );
    let lock = spawn_wait_for(&handler, "cancel-lock", WaitForMode::Lock);
    yield_until_counts(&handler, "cancel-lock", (0, 1, true)).await;
    lock.abort();
    assert!(lock
        .await
        .expect_err("lock waiter is cancelled")
        .is_cancelled());
    yield_until_counts(&handler, "cancel-lock", (0, 0, true)).await;
}

#[tokio::test]
async fn wait_for_shutdown_releases_plain_and_lock_waiters() {
    let handler = Arc::new(RequestHandler::new());

    assert_eq!(
        handler
            .handle(wait_for("shutdown-lock", WaitForMode::Lock))
            .await,
        Response::WaitFor(WaitForResponse)
    );
    let plain = spawn_wait_for(&handler, "shutdown-plain", WaitForMode::Wait);
    yield_until_counts(&handler, "shutdown-plain", (1, 0, false)).await;
    let lock = spawn_wait_for(&handler, "shutdown-lock", WaitForMode::Lock);
    yield_until_counts(&handler, "shutdown-lock", (0, 1, true)).await;

    handler.shutdown_wait_for_for_test();

    assert!(matches!(
        plain.await.expect("plain waiter"),
        Response::Error(_)
    ));
    assert!(matches!(
        lock.await.expect("lock waiter"),
        Response::Error(_)
    ));
    yield_until_counts(&handler, "shutdown-plain", (0, 0, false)).await;
    yield_until_counts(&handler, "shutdown-lock", (0, 0, false)).await;
}

fn spawn_wait_for(
    handler: &Arc<RequestHandler>,
    channel: &'static str,
    mode: WaitForMode,
) -> tokio::task::JoinHandle<Response> {
    let handler = Arc::clone(handler);
    tokio::spawn(async move { handler.handle(wait_for(channel, mode)).await })
}

async fn yield_until_counts(
    handler: &RequestHandler,
    channel: &str,
    expected: (usize, usize, bool),
) {
    for _ in 0..100 {
        if handler.wait_for_counts(channel) == expected {
            return;
        }
        tokio::task::yield_now().await;
    }

    assert_eq!(handler.wait_for_counts(channel), expected);
}
