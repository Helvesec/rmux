use super::*;

#[tokio::test]
async fn runtime_execution_conditional_display_retains_cli_context() {
    let handler = RequestHandler::new();
    create_background_identity_session(&handler, session_name("r2-entry")).await;
    run_hook_probe_queue(&handler,
        "set-hook -g after-display-message 'set-option -gF @entry \"#{hook_arguments}|#{hook_argument_0}|#{hook_flag_t}\"'",
    ).await;
    for (arguments, expected) in [
        (
            vec!["-p", "-t", "r2-entry", "READY"],
            "-p -t r2-entry READY|-p|r2-entry\n",
        ),
        (vec!["-lp", "READY"], "-lp READY|-lp|\n"),
        (vec!["-l", "-p", "READY"], "-l -p READY|-l|\n"),
        (vec!["-v", "-p", "READY"], "-v -p READY|-v|\n"),
        (vec!["-a"], "-a|-a|\n"),
        (vec!["-p", "READY"], "||\n"),
        (vec!["-p", "-F", "-t"], "||\n"),
        (vec!["-p", "--", "-l"], "||\n"),
        (vec!["-p", "-d", "1", "-N", "-C", "READY"], "||\n"),
    ] {
        let mut argv = vec!["display-message"];
        argv.extend(arguments);
        let actual = execution_result(handler.handle(execution_request(&argv)).await);
        assert!(
            matches!(
                actual,
                RuntimeCommandExecutionResponse::Complete {
                    error: None,
                    exit_status: 0,
                    ..
                }
            ),
            "{argv:?}: {actual:?}"
        );
        assert_eq!(
            run_hook_probe_queue(&handler, "show-options -gqv @entry").await,
            expected.as_bytes(),
            "{argv:?}"
        );
    }
}

#[tokio::test]
async fn runtime_execution_conditional_display_cancels_shutdown_but_direct_drains() {
    use std::future::Future;
    use std::task::Poll;
    use tokio::time::{advance, timeout, Duration};

    tokio::time::pause();
    for queued in [true, false] {
        let handler = RequestHandler::new();
        create_background_identity_session(&handler, session_name("r2-entry")).await;
        run_hook_probe_queue(
            &handler,
            "set-hook -g after-display-message 'run-shell -d7'",
        )
        .await;
        set_alias(
            &handler,
            if queued {
                "list-sessions=display-message -p -t r2-entry READY"
            } else {
                "list-sessions=display-message -p READY"
            },
        )
        .await;
        let mut execution = Box::pin(handler.handle(execution_request(&["list-sessions"])));
        std::future::poll_fn(|cx| {
            assert!(
                execution.as_mut().poll(cx).is_pending(),
                "reaches delayed hook"
            );
            Poll::Ready(())
        })
        .await;
        handler.close_normal_request_admission();
        if queued {
            let result = timeout(Duration::from_millis(100), execution)
                .await
                .expect("queued display must cancel promptly");
            assert!(matches!(
                execution_result(result),
                RuntimeCommandExecutionResponse::Complete {
                    error: Some(_),
                    exit_status: 1,
                    ..
                }
            ));
        } else {
            advance(Duration::from_secs(6)).await;
            std::future::poll_fn(|cx| {
                assert!(
                    execution.as_mut().poll(cx).is_pending(),
                    "direct hook drains past shell grace"
                );
                Poll::Ready(())
            })
            .await;
            advance(Duration::from_secs(1)).await;
            assert!(matches!(
                execution_result(execution.await),
                RuntimeCommandExecutionResponse::Complete {
                    error: None,
                    exit_status: 0,
                    ..
                }
            ));
        }
    }
}

#[tokio::test]
async fn runtime_execution_new_window_k_declines_before_replacement() {
    let handler = RequestHandler::new();
    create_background_identity_session(&handler, session_name("r2-entry")).await;
    // Pin the name: the automatic name follows the starting shell's foreground
    // process and is not a mutation of the declined command.
    run_hook_probe_queue(&handler, "rename-window -t r2-entry:0 r2-stable").await;
    let before = run_hook_probe_queue(
        &handler,
        "list-windows -t r2-entry -F '#{window_id}|#{window_name}'",
    )
    .await;
    assert_eq!(before, b"@0|r2-stable\n");
    for flags in [vec!["-k", "-d"], vec!["-dk"], vec!["-S", "-d"], vec!["-Sd"]] {
        let mut arguments = vec!["new-window"];
        arguments.extend(flags);
        arguments.extend(["-t", "r2-entry:0", "-n", "unexpected"]);
        assert_eq!(
            execution_result(handler.handle(execution_request(&arguments)).await),
            RuntimeCommandExecutionResponse::Fallback
        );
        assert_eq!(
            run_hook_probe_queue(
                &handler,
                "list-windows -t r2-entry -F '#{window_id}|#{window_name}'"
            )
            .await,
            before
        );
    }
}
