use super::*;

#[tokio::test]
async fn runtime_execution_preserves_list_windows_all_cli_queue_metadata() {
    let handler = RequestHandler::new();
    create_background_identity_session(&handler, session_name("r2-lists")).await;
    run_hook_probe_queue(&handler,
        "set-hook -g after-list-windows 'set-option -gF @r2hook \"#{hook_arguments}|#{hook_argument_0}|#{hook_flag_a}\"'",
    ).await;
    for (alias, canonical) in [
        ("list-sessions=list-windows -a", vec!["list-windows", "-a"]),
        (
            "list-sessions=list-windows -ra -F '#{window_index}'",
            vec!["list-windows", "-a", "-F", "#{window_index}", "-r"],
        ),
        (
            "list-sessions=list-windows -F old -F '' -a -t r2-lists",
            vec!["list-windows", "-a", "-t", "r2-lists", "-F", ""],
        ),
    ] {
        let mut source = match source_file_request(
            vec![rmux_proto::INTERNAL_LIST_WINDOWS_ALL_EXECUTION_PATH.to_owned()],
            None,
        ) {
            Request::SourceFile(source) => source,
            _ => unreachable!(),
        };
        source.stdin = Some(
            encode_internal_runtime_command_arguments(
                &canonical.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            )
            .unwrap(),
        );
        let full = handler.handle(Request::SourceFile(source)).await;
        assert!(
            matches!(full, Response::SourceFile(ref response) if response.exit_status().is_none()),
            "full CLI setup: {full:?}"
        );
        let expected = run_hook_probe_queue(&handler, "show-options -gqv @r2hook").await;
        assert!(
            expected.starts_with(b"-a"),
            "full CLI queue exposes arguments: {expected:?}"
        );
        set_alias(&handler, alias).await;
        let actual = execution_result(handler.handle(execution_request(&["list-sessions"])).await);
        assert!(
            matches!(
                actual,
                RuntimeCommandExecutionResponse::Complete {
                    error: None,
                    exit_status: 0,
                    ..
                }
            ),
            "{alias}: {actual:?}"
        );
        assert_eq!(
            run_hook_probe_queue(&handler, "show-options -gqv @r2hook").await,
            expected,
            "{alias}"
        );
    }
    // Ordinary list-windows and list-panes -a use direct typed requests.
    for (command, hook, arguments) in [
        (
            "list-windows",
            "after-list-windows",
            vec!["list-windows", "-t", "r2-lists"],
        ),
        ("list-panes", "after-list-panes", vec!["list-panes", "-a"]),
    ] {
        run_hook_probe_queue(&handler, &format!("set-hook -g {hook} 'set-option -gF @r2hook \"#{{hook_arguments}}|#{{hook_argument_0}}|#{{hook_flag_a}}\"'")).await;
        let actual = execution_result(handler.handle(execution_request(&arguments)).await);
        assert!(
            matches!(
                actual,
                RuntimeCommandExecutionResponse::Complete {
                    error: None,
                    exit_status: 0,
                    ..
                }
            ),
            "{command}: {actual:?}"
        );
        assert_eq!(
            run_hook_probe_queue(&handler, "show-options -gqv @r2hook").await,
            b"||\n",
            "direct {command}"
        );
    }
}

async fn prompt_metadata(command: &str, input: &[u8]) {
    let handler = RequestHandler::new();
    let pid = std::process::id();
    let session = session_name("r2-prompt");
    create_background_identity_session(&handler, session.clone()).await;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    handler.register_attach(pid, session, tx).await;
    run_hook_probe_queue(&handler,
        "set-hook -g after-set-option 'set-option -gF @r2hook \"#{hook_arguments}|#{hook_argument_0}|#{hook_flag_g}\"'",
    ).await;
    run_hook_probe_queue(&handler, "set-option -g status off").await;
    let expected = run_hook_probe_queue(&handler, "show-options -gqv @r2hook").await;
    assert_eq!(expected, b"-g status off|-g|status\n");
    for background in [false, true] {
        handler
            .handle(execution_request(&["set-option", "-g", "status", "on"]))
            .await;
        assert_eq!(
            run_hook_probe_queue(&handler, "show-options -gqv @r2hook").await,
            b"||\n"
        );
        set_alias(
            &handler,
            &format!(
                "list-sessions={command} {} 'set-option -g status off'",
                if background { "-b" } else { "" }
            ),
        )
        .await;
        let owned = handler.clone();
        let task =
            tokio::spawn(async move { owned.handle(execution_request(&["list-sessions"])).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !handler.prompt_active(pid).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("setup: runtime offer opens an attached prompt");
        handler
            .handle_attached_live_input_for_test(pid, input)
            .await
            .expect("prompt accepts input");
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("prompt command completes")
            .expect("prompt task joins");
        assert!(matches!(
            execution_result(result),
            RuntimeCommandExecutionResponse::Complete {
                error: None,
                exit_status: 0,
                ..
            }
        ));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while handler.background_task_running_for_test("rmux-prompt-finish") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("completion callback finishes");
        assert_eq!(
            run_hook_probe_queue(&handler, "show-options -gqv @r2hook").await,
            expected,
            "{command}, background={background}"
        );
    }
    handler
        .handle(execution_request(&["set-option", "-g", "status", "on"]))
        .await;
    assert_eq!(
        run_hook_probe_queue(&handler, "show-options -gqv @r2hook").await,
        b"||\n"
    );
}

#[tokio::test]
async fn runtime_execution_preserves_command_prompt_completion_metadata() {
    prompt_metadata("command-prompt", b"ok\r").await;
}

#[tokio::test]
async fn runtime_execution_preserves_confirm_before_completion_metadata() {
    prompt_metadata("confirm-before", b"y").await;
}
