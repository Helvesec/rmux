use super::*;

// tmux 3.7b, R2-271, 2026-10-25: refusal stops its own command group;
// a parent group continues. Background run-shell -C has no client and
// new-session creates a detached session (D294 measured contract).
#[tokio::test]
async fn run_shell_deferred_attach_direct_request() {
    exercise_deferred_attach(false).await;
}

#[tokio::test]
async fn run_shell_deferred_attach_queued_request() {
    exercise_deferred_attach(true).await;
}

async fn dispatch(
    handler: &RequestHandler,
    queued: bool,
    background: bool,
    command: &str,
) -> Result<(), String> {
    if queued {
        let invocation = format!(
            "run-shell {} -C {}",
            if background { "-b" } else { "" },
            command_quote(command)
        );
        let parsed = CommandParser::new()
            .parse(&invocation)
            .expect("valid queue");
        handler
            .execute_parsed_commands_for_test(std::process::id(), parsed)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    } else {
        let response = run_nested_commands(handler, command.to_owned(), background, None).await;
        match response {
            Response::Error(error) => Err(error.error.to_string()),
            Response::RunShell(_) => Ok(()),
            other => panic!("unexpected run-shell response: {other:?}"),
        }
    }
}

async fn exercise_deferred_attach(queued: bool) {
    let handler = RequestHandler::new();
    create_named_session(&handler, "deferred-alpha").await;

    for background in [false, true] {
        for (index, command) in [
            "if-shell -F 0 { attach-session -t deferred-alpha } { set-buffer -b proof OK }",
            "if-shell -F 0 'attach-session -t deferred-alpha' 'set-buffer -b proof OK'",
            "if-shell -F 1 { set-buffer -b proof OK } { attach-session -t deferred-alpha }",
            "if-shell -F 1 'set-buffer -b proof OK' 'attach-session -t deferred-alpha'",
            "bind-key -T deferred x { attach-session -t deferred-alpha } ; set-buffer -b proof OK",
            "bind-key -T deferred y 'attach-session -t deferred-alpha' ; set-buffer -b proof OK",
            "set-hook -g client-attached { attach-session -t deferred-alpha } ; set-buffer -b proof OK",
            "set-hook -g client-attached 'attach-session -t deferred-alpha' ; set-buffer -b proof OK",
        ]
        .iter()
        .enumerate()
        {
            let buffer = format!("proof-{background}-{index}");
            let command = command.replace("-b proof ", &format!("-b {buffer} "));
            let result = dispatch(&handler, queued, background, &command).await;
            assert!(result.is_ok(), "{command:?}: {result:?}");
            wait_for_named_buffer(&handler, &buffer, b"OK").await;
            wait_for_detached_request_count(&handler, 0).await;
        }

        let stored = handler
            .execute_parsed_commands_for_test(
                std::process::id(),
                CommandParser::new()
                    .parse("list-keys -T deferred ; show-hooks -g")
                    .expect("stored definitions query"),
            )
            .await
            .expect("stored definitions remain queryable");
        let text = String::from_utf8_lossy(stored.stdout());
        assert!(text.contains("attach-session"), "{text}");
        assert!(text.contains("client-attached"), "{text}");

        for (index, command) in [
            "attach-session -t deferred-alpha",
            "if-shell -F 1 { attach-session -t deferred-alpha }",
            "if-shell -F 1 'attach-session -t deferred-alpha'",
            "if-shell -F 0 { set-buffer -b unreachable bad } { attach-session -t deferred-alpha }",
            "if-shell -b -F 1 { attach-session -t deferred-alpha }",
            "run-shell -C 'attach-session -t deferred-alpha'",
            "new-session -s deferred-refused",
            "if-shell -F 1 { new-session -s deferred-refused }",
        ]
        .iter()
        .enumerate()
        {
            // The before/after markers distinguish local refusal from parent
            // continuation and the clientless background queue (R2-271).
            let before = format!("before-{background}-{index}");
            let after = format!("after-{background}-{index}");
            let command =
                format!("set-buffer -b {before} yes ; {command} ; set-buffer -b {after} bad");
            let result = dispatch(&handler, queued, background, &command).await;
            if !background && !command.contains("if-shell -b") {
                assert!(
                    result
                        .as_ref()
                        .is_err_and(|error| error.contains("open terminal failed: not a terminal")),
                    "{command:?}: {result:?}"
                );
            }
            wait_for_named_buffer(&handler, &before, b"yes").await;
            wait_for_detached_request_count(&handler, 0).await;
            // if-shell -b inserts independent work, so its parent may continue.
            if !command.contains("if-shell -b") {
                // R2-271: clean-request-{background}-{index} and
                // sourcefile-{background}-selected-{index}; confirmed by
                // default-matrix-{background}-selected-{index}.
                // Old None -> bad: foreground 1,2,3,5,7; background 0,1,2,3,5,6,7.
                // Foreground 0,6 stay None; index 4 remains outside this assertion.
                let expected = if background || matches!(index, 1 | 2 | 3 | 5 | 7) {
                    Some(b"bad".as_slice())
                } else {
                    None
                };
                assert_named_buffer(&handler, &after, expected).await;
            }
            // R2-271: clean-request-true-{6,7}, sourcefile-true-selected-{6,7},
            // default-matrix-true-selected-{6,7}: old absent -> present (detached).
            // All other session-presence expectations remain absent.
            let creates_session = background && matches!(index, 6 | 7);
            assert_eq!(
                handler
                    .state
                    .lock()
                    .await
                    .sessions
                    .contains_session(&session_name("deferred-refused")),
                creates_session,
                "R2-271 session presence: background={background}, index={index}"
            );
            if creates_session {
                // The same R2-271 background 6/7 inventories show zero clients.
                assert_eq!(
                    handler
                        .attached_count(&session_name("deferred-refused"))
                        .await,
                    0
                );
                // R2-271 isolates new-session cases: restore the measured empty name.
                execute_test_command(&handler, "kill-session -t deferred-refused").await;
            }
        }
    }
}
