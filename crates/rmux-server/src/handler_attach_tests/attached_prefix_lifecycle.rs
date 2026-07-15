use super::*;

#[cfg(windows)]
const WINDOWS_ATTACH_EXIT_TIMEOUT: Duration = Duration::from_secs(20);

#[cfg(unix)]
const PROMPT_NEW_WINDOW_INPUT: &[u8] =
    b"\x02:new-window -- 'printf ISSUE8_WINDOW_READY; sleep 30'\r";

#[cfg(windows)]
const PROMPT_NEW_WINDOW_INPUT: &[u8] =
    b"\x02:new-window -- cmd.exe /d /q /c \"echo ISSUE8_WINDOW_READY & ping -n 30 127.0.0.1 >NUL\"\r";

async fn bind_attached_prompt_test_key(handler: &RequestHandler, key: &str, command: Vec<String>) {
    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: key.to_owned(),
            note: Some("attached prompt target-client regression".to_owned()),
            repeat: false,
            command: Some(command),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)), "{response:?}");
}

#[tokio::test]
async fn attached_prefix_d_dispatches_detach_client() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02d")
        .await
        .expect("prefix d dispatches");

    // Entering and leaving the prefix key table now repaints the status bar
    // (so #{client_prefix} can show a prefix indicator), so the Detach control
    // may be preceded by status-refresh Write frames; scan past them.
    recv_matching_attach_control(&mut control_rx, "prefix d detach", |control| {
        matches!(control, AttachControl::Detach)
    })
    .await;
}

#[tokio::test]
async fn attached_prefix_d_dispatches_detach_client_across_separate_reads() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02")
        .await
        .expect("prefix key input");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"d")
        .await
        .expect("prefix d input");

    recv_matching_attach_control(&mut control_rx, "split prefix d detach", |control| {
        matches!(control, AttachControl::Detach)
    })
    .await;
}

#[tokio::test]
async fn attached_send_prefix_then_does_not_detach() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02")
        .await
        .expect("prefix key input");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02d")
        .await
        .expect("send-prefix then d input");

    while let Ok(control) = control_rx.try_recv() {
        assert!(
            !matches!(control, AttachControl::Detach),
            "C-b C-b d must send a literal prefix followed by d, not detach"
        );
    }
}

#[tokio::test]
async fn attached_prefix_c_creates_window_across_separate_reads() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02")
        .await
        .expect("prefix key input");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"c")
        .await
        .expect("prefix c input");

    assert_eq!(
        active_windows(&handler, &alpha).await,
        "0:0\n1:1\n",
        "C-b c must still create a new window when keys arrive in separate reads"
    );
}

#[tokio::test]
async fn attached_command_prompt_can_chain_choose_tree_overlay() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let prompted = session_name("prompted");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "X".to_owned(),
            note: Some("prompt-then-choose-tree".to_owned()),
            repeat: false,
            command: Some(vec![
                "command-prompt".to_owned(),
                "-p".to_owned(),
                "name:".to_owned(),
                "new-session -d -s '%%' ; choose-tree -Zs".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02X")
        .await
        .expect("prefix X opens command-prompt");
    wait_for_attach_output_containing(&mut control_rx, "name:").await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"prompted\r")
        .await
        .expect("prompt response opens choose-tree");

    let rendered = wait_for_attach_output_containing(&mut control_rx, "sort:").await;
    assert!(
        rendered.contains("alpha") && rendered.contains("prompted"),
        "choose-tree should render both sessions after prompt continuation, got:\n{rendered}"
    );
    {
        let state = handler.state.lock().await;
        assert!(
            state.sessions.session(&prompted).is_some(),
            "prompt continuation should create the requested session"
        );
    }

    handler
        .handle_attached_live_input_for_test(requester_pid, b"q")
        .await
        .expect("q exits chained choose-tree");
    let deadline = tokio::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
    loop {
        {
            let active_attach = handler.active_attach.lock().await;
            let mode_active = active_attach
                .by_pid
                .get(&requester_pid)
                .is_some_and(|active| active.mode_tree.is_some());
            if !mode_active {
                break;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "chained choose-tree did not exit after q"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn attached_foreground_prompts_resolve_explicit_target_client() {
    let handler = RequestHandler::new();
    let owner_pid = u32::MAX - 501;
    let target_pid = u32::MAX - 502;
    let alpha = session_name("attached-prompt-explicit-target");
    let _owner_rx = create_attached_session(&handler, owner_pid, &alpha).await;
    let (target_tx, _target_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(target_pid, alpha.clone(), target_tx)
        .await;

    bind_attached_prompt_test_key(
        &handler,
        "X",
        vec![
            "command-prompt".to_owned(),
            "-t".to_owned(),
            target_pid.to_string(),
            "-p".to_owned(),
            "target-name:".to_owned(),
            "set-option -g -F @targeted-command-prompt '%%:#{client_name}'".to_owned(),
        ],
    )
    .await;
    handler
        .handle_attached_live_input_for_test(owner_pid, b"\x02X")
        .await
        .expect("owner opens command-prompt on target client");
    assert!(!handler.prompt_active(owner_pid).await);
    assert!(handler.prompt_active(target_pid).await);

    handler
        .handle_attached_live_input_for_test(target_pid, b"target-value\r")
        .await
        .expect("target client submits command-prompt");
    wait_for_global_option_value(
        &handler,
        "@targeted-command-prompt",
        &format!(
            "target-value:{}",
            crate::handler::attached_client_name(owner_pid)
        ),
    )
    .await;

    bind_attached_prompt_test_key(
        &handler,
        "Y",
        vec![
            "confirm-before".to_owned(),
            "-t".to_owned(),
            target_pid.to_string(),
            "-p".to_owned(),
            "confirm-target?".to_owned(),
            "set-option -g -F @targeted-confirm-before '#{client_name}'".to_owned(),
        ],
    )
    .await;
    handler
        .handle_attached_live_input_for_test(owner_pid, b"\x02Y")
        .await
        .expect("owner opens confirm-before on target client");
    assert!(!handler.prompt_active(owner_pid).await);
    assert!(handler.prompt_active(target_pid).await);

    handler
        .handle_attached_live_input_for_test(target_pid, b"y")
        .await
        .expect("target client accepts confirm-before");
    wait_for_global_option_value(
        &handler,
        "@targeted-confirm-before",
        &crate::handler::attached_client_name(owner_pid),
    )
    .await;
}

#[tokio::test]
async fn attached_foreground_prompts_reject_unknown_target_client() {
    let handler = RequestHandler::new();
    let owner_pid = u32::MAX - 503;
    let alpha = session_name("attached-prompt-unknown-target");
    let _owner_rx = create_attached_session(&handler, owner_pid, &alpha).await;
    let missing_pid = 999_999_u32;

    bind_attached_prompt_test_key(
        &handler,
        "X",
        vec![
            "command-prompt".to_owned(),
            "-t".to_owned(),
            missing_pid.to_string(),
            "-p".to_owned(),
            "missing:".to_owned(),
            "set-option -g @missing-command-prompt reached".to_owned(),
        ],
    )
    .await;
    let error = handler
        .handle_attached_live_input_for_test(owner_pid, b"\x02X")
        .await
        .expect_err("unknown command-prompt target must fail closed");
    assert!(
        error
            .to_string()
            .contains(&format!("can't find client: {missing_pid}")),
        "unexpected command-prompt error: {error}"
    );
    assert!(!handler.prompt_active(owner_pid).await);

    bind_attached_prompt_test_key(
        &handler,
        "Y",
        vec![
            "confirm-before".to_owned(),
            "-t".to_owned(),
            missing_pid.to_string(),
            "-p".to_owned(),
            "missing?".to_owned(),
            "set-option -g @missing-confirm-before reached".to_owned(),
        ],
    )
    .await;
    let error = handler
        .handle_attached_live_input_for_test(owner_pid, b"\x02Y")
        .await
        .expect_err("unknown confirm-before target must fail closed");
    assert!(
        error
            .to_string()
            .contains(&format!("can't find client: {missing_pid}")),
        "unexpected confirm-before error: {error}"
    );
    assert!(!handler.prompt_active(owner_pid).await);
}

#[tokio::test]
async fn attached_foreground_prompts_without_target_stay_on_binding_owner() {
    let handler = RequestHandler::new();
    let owner_pid = u32::MAX - 504;
    let other_pid = u32::MAX - 505;
    let alpha = session_name("attached-prompt-default-target");
    let _owner_rx = create_attached_session(&handler, owner_pid, &alpha).await;
    let (other_tx, _other_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(other_pid, alpha.clone(), other_tx)
        .await;

    bind_attached_prompt_test_key(
        &handler,
        "X",
        vec![
            "command-prompt".to_owned(),
            "-p".to_owned(),
            "owner-name:".to_owned(),
            "set-option -g @owner-command-prompt '%%'".to_owned(),
        ],
    )
    .await;
    handler
        .handle_attached_live_input_for_test(owner_pid, b"\x02X")
        .await
        .expect("owner opens its command-prompt");
    assert!(handler.prompt_active(owner_pid).await);
    assert!(!handler.prompt_active(other_pid).await);

    handler
        .handle_attached_live_input_for_test(owner_pid, b"owner-value\r")
        .await
        .expect("owner submits command-prompt");
    wait_for_global_option_value(&handler, "@owner-command-prompt", "owner-value").await;

    bind_attached_prompt_test_key(
        &handler,
        "Y",
        vec![
            "confirm-before".to_owned(),
            "-p".to_owned(),
            "confirm-owner?".to_owned(),
            "set-option -g @owner-confirm-before yes".to_owned(),
        ],
    )
    .await;
    handler
        .handle_attached_live_input_for_test(owner_pid, b"\x02Y")
        .await
        .expect("owner opens its confirm-before");
    assert!(handler.prompt_active(owner_pid).await);
    assert!(!handler.prompt_active(other_pid).await);

    handler
        .handle_attached_live_input_for_test(owner_pid, b"y")
        .await
        .expect("owner accepts confirm-before");
    wait_for_global_option_value(&handler, "@owner-confirm-before", "yes").await;
}

#[tokio::test]
async fn targeted_attached_prompt_completion_rejects_replaced_binding_owner() {
    let handler = RequestHandler::new();
    let owner_pid = u32::MAX - 506;
    let target_pid = u32::MAX - 507;
    let alpha = session_name("attached-prompt-replaced-owner");
    let mut original_owner_rx = create_attached_session(&handler, owner_pid, &alpha).await;
    let (target_tx, _target_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(target_pid, alpha.clone(), target_tx)
        .await;

    bind_attached_prompt_test_key(
        &handler,
        "X",
        vec![
            "command-prompt".to_owned(),
            "-t".to_owned(),
            target_pid.to_string(),
            "-p".to_owned(),
            "target-name:".to_owned(),
            "set-option -g @replaced-prompt-owner should-not-run".to_owned(),
        ],
    )
    .await;
    handler
        .handle_attached_live_input_for_test(owner_pid, b"\x02X")
        .await
        .expect("owner opens command-prompt on target client");
    assert!(handler.prompt_active(target_pid).await);

    let (replacement_tx, _replacement_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(owner_pid, alpha, replacement_tx)
        .await;
    recv_matching_attach_control(
        &mut original_owner_rx,
        "original prompt owner replacement",
        |control| matches!(control, AttachControl::Detach),
    )
    .await;

    handler
        .handle_attached_live_input_for_test(target_pid, b"accepted\r")
        .await
        .expect("target client submits after owner replacement");
    assert!(!handler.prompt_active(target_pid).await);
    sleep(Duration::from_millis(100)).await;

    let response = handler
        .handle(Request::ShowOptions(rmux_proto::ShowOptionsRequest {
            scope: rmux_proto::OptionScopeSelector::SessionGlobal,
            name: Some("@replaced-prompt-owner".to_owned()),
            value_only: true,
            include_inherited: false,
            quiet: true,
            include_hooks: false,
        }))
        .await;
    let output = response
        .command_output()
        .expect("quiet show-options returns command output");
    assert_eq!(
        output.stdout(),
        b"",
        "stale owner continuation must fail closed"
    );
}

#[tokio::test]
async fn attached_binding_run_shell_expands_client_name() {
    let handler = RequestHandler::new();
    #[cfg(windows)]
    set_windows_test_shell(&handler).await;
    let requester_pid = u32::MAX - 71;
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let root = std::env::temp_dir().join(format!(
        "rmux-attached-client-name-{}-{requester_pid}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("client-name temp root");
    let output_path = root.join("client-name.txt");
    let shell_command = client_name_file_shell_command(&output_path);

    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "T".to_owned(),
            note: Some("attached-client-name".to_owned()),
            repeat: false,
            command: Some(vec!["run-shell".to_owned(), "-b".to_owned(), shell_command]),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02T")
        .await
        .expect("prefix T dispatches run-shell binding");

    wait_for_file_contents(
        &output_path,
        &crate::handler::attached_client_name(requester_pid),
    )
    .await;
}

#[tokio::test]
async fn attached_binding_new_window_shell_command_expands_client_name() {
    let handler = RequestHandler::new();
    #[cfg(windows)]
    set_windows_test_shell(&handler).await;
    let requester_pid = u32::MAX - 73;
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let root = std::env::temp_dir().join(format!(
        "rmux-attached-new-window-client-name-{}-{requester_pid}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("new-window client-name temp root");
    let output_path = root.join("client-name.txt");
    let pane_command = client_name_file_pane_command(&output_path);

    let mut command = vec!["new-window".to_owned(), "-d".to_owned(), "--".to_owned()];
    command.extend(pane_command);
    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "V".to_owned(),
            note: Some("attached-client-name-new-window".to_owned()),
            repeat: false,
            command: Some(command),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02V")
        .await
        .expect("prefix V dispatches new-window binding");

    let expected_client = crate::handler::attached_client_name(requester_pid);
    #[cfg(unix)]
    wait_for_file_contents(&output_path, &expected_client).await;
    #[cfg(windows)]
    wait_for_pane_lifecycle_command_containing(
        &handler,
        PaneTarget::with_window(alpha.clone(), 1, 0),
        &expected_client,
    )
    .await;
}

#[tokio::test]
async fn attached_binding_split_window_shell_command_expands_client_name() {
    let handler = RequestHandler::new();
    #[cfg(windows)]
    set_windows_test_shell(&handler).await;
    let requester_pid = u32::MAX - 74;
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let root = std::env::temp_dir().join(format!(
        "rmux-attached-split-window-client-name-{}-{requester_pid}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("split-window client-name temp root");
    let output_path = root.join("client-name.txt");
    let pane_command = client_name_file_pane_command(&output_path);

    let mut command = vec!["split-window".to_owned(), "-d".to_owned(), "--".to_owned()];
    command.extend(pane_command);
    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "W".to_owned(),
            note: Some("attached-client-name-split-window".to_owned()),
            repeat: false,
            command: Some(command),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02W")
        .await
        .expect("prefix W dispatches split-window binding");

    let expected_client = crate::handler::attached_client_name(requester_pid);
    #[cfg(unix)]
    wait_for_file_contents(&output_path, &expected_client).await;
    #[cfg(windows)]
    wait_for_pane_lifecycle_command_containing(
        &handler,
        PaneTarget::with_window(alpha.clone(), 0, 1),
        &expected_client,
    )
    .await;
}

#[tokio::test]
async fn attached_binding_set_option_format_expands_client_name() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 75;
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "Y".to_owned(),
            note: Some("attached-client-name-set-option".to_owned()),
            repeat: false,
            command: Some(vec![
                "set-option".to_owned(),
                "-g".to_owned(),
                "-F".to_owned(),
                "@attached-client-context".to_owned(),
                "#{client_name}:#{session_name}:#{pane_index}".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02Y")
        .await
        .expect("prefix Y dispatches set-option binding");

    wait_for_global_option_value(
        &handler,
        "@attached-client-context",
        &format!(
            "{}:alpha:0",
            crate::handler::attached_client_name(requester_pid)
        ),
    )
    .await;
}

#[tokio::test]
async fn attached_binding_source_file_preserves_client_context() {
    let handler = RequestHandler::new();
    #[cfg(windows)]
    set_windows_test_shell(&handler).await;
    let requester_pid = u32::MAX - 76;
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let root = std::env::temp_dir().join(format!(
        "rmux-attached-source-client-context-{}-{requester_pid}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("source-file client context temp root");
    let source_path = root.join("client-context.conf");
    let run_shell_path = root.join("run-shell-client-name.txt");
    #[cfg(unix)]
    let new_window_path = root.join("new-window-client-name.txt");
    #[cfg(unix)]
    let split_window_path = root.join("split-window-client-name.txt");

    let source = format!(
        "set-option -g -F @source-client-context '{}'\n\
         if-shell -F '{}' '{}' '{}'\n\
         run-shell -b {}\n",
        "#{client_name}:#{session_name}:#{pane_index}",
        "#{client_name}",
        "set-buffer -b source-client-if-shell yes",
        "set-buffer -b source-client-if-shell no",
        quote_command_argument(&client_name_file_shell_command(&run_shell_path)),
    );
    #[cfg(unix)]
    let source = format!(
        "{source}new-window -d -- {}\n\
         split-window -d -- {}\n",
        quote_command_arguments(&client_name_file_pane_command(&new_window_path)),
        quote_command_arguments(&client_name_file_pane_command(&split_window_path)),
    );
    std::fs::write(&source_path, source).expect("source-file client context config");

    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "Z".to_owned(),
            note: Some("attached-client-context-source-file".to_owned()),
            repeat: false,
            command: Some(vec![
                "source-file".to_owned(),
                source_path.to_string_lossy().into_owned(),
            ]),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02Z")
        .await
        .expect("prefix Z dispatches source-file binding");

    let expected_client = crate::handler::attached_client_name(requester_pid);
    wait_for_global_option_value(
        &handler,
        "@source-client-context",
        &format!("{expected_client}:alpha:0"),
    )
    .await;
    wait_for_buffer_contents(&handler, "source-client-if-shell", b"yes").await;
    wait_for_file_contents(&run_shell_path, &expected_client).await;
    #[cfg(unix)]
    {
        wait_for_file_contents(&new_window_path, &expected_client).await;
        wait_for_file_contents(&split_window_path, &expected_client).await;
    }
}

#[tokio::test]
async fn attached_binding_two_clients_get_distinct_client_names() {
    let handler = RequestHandler::new();
    #[cfg(windows)]
    set_windows_test_shell(&handler).await;
    let first_pid = u32::MAX - 77;
    let second_pid = u32::MAX - 78;
    let alpha = session_name("alpha");
    let _first_rx = create_attached_session(&handler, first_pid, &alpha).await;
    let (second_tx, _second_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(second_pid, alpha.clone(), second_tx)
        .await;

    let root = std::env::temp_dir().join(format!(
        "rmux-attached-two-client-names-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("two-client name temp root");
    let shell_command = client_name_file_shell_command(&root.join("#{client_name}.txt"));

    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "X".to_owned(),
            note: Some("attached-two-client-names".to_owned()),
            repeat: false,
            command: Some(vec!["run-shell".to_owned(), "-b".to_owned(), shell_command]),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)));

    handler
        .handle_attached_live_input_for_test(first_pid, b"\x02X")
        .await
        .expect("first client dispatches two-client binding");
    handler
        .handle_attached_live_input_for_test(second_pid, b"\x02X")
        .await
        .expect("second client dispatches two-client binding");

    let first_name = crate::handler::attached_client_name(first_pid);
    let second_name = crate::handler::attached_client_name(second_pid);
    assert_ne!(first_name, second_name);
    wait_for_file_contents(&root.join(format!("{first_name}.txt")), &first_name).await;
    wait_for_file_contents(&root.join(format!("{second_name}.txt")), &second_name).await;
}

#[tokio::test]
async fn attached_binding_if_shell_condition_expands_client_name() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 72;
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let buffer_name = "attached-client-name-if-shell";

    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "U".to_owned(),
            note: Some("attached-client-name-if-shell".to_owned()),
            repeat: false,
            command: Some(vec![
                "if-shell".to_owned(),
                "-F".to_owned(),
                "#{client_name}".to_owned(),
                format!("set-buffer -b {buffer_name} yes"),
                format!("set-buffer -b {buffer_name} no"),
            ]),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02U")
        .await
        .expect("prefix U dispatches if-shell binding");

    wait_for_buffer_contents(&handler, buffer_name, b"yes").await;
}

#[tokio::test]
async fn attached_binding_if_shell_branch_expands_client_name() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 73;
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "V".to_owned(),
            note: Some("attached-client-name-if-shell-branch".to_owned()),
            repeat: false,
            command: Some(vec![
                "if-shell".to_owned(),
                "-F".to_owned(),
                "1".to_owned(),
                "set-option -g -F @if-shell-branch-client '#{client_name}'".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02V")
        .await
        .expect("prefix V dispatches if-shell binding");

    wait_for_global_option_value(
        &handler,
        "@if-shell-branch-client",
        &crate::handler::attached_client_name(requester_pid),
    )
    .await;
}

#[tokio::test]
async fn attached_single_switch_queue_completes_after_session_transition() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 78;
    let alpha = session_name("single-switch-alpha");
    let beta = session_name("single-switch-beta");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
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
    let identity = handler.active_attach_identity_for_test(requester_pid).await;
    let commands = handler
        .parse_control_commands(&format!("switch-client -t {beta}"))
        .await
        .expect("single switch-client queue parses");

    crate::handler::with_expected_attach_and_session_identity(
        identity,
        alpha,
        identity.session_id(),
        handler.execute_parsed_commands_for_test(requester_pid, commands),
    )
    .await
    .expect("single switch-client queue must not fail after its final item");

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client remains registered");
    assert_eq!(active.session_name, beta);
}

#[tokio::test]
async fn attached_key_table_only_switch_allows_its_queue_tail() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 82;
    let alpha = session_name("key-table-switch-alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    let identity = handler.active_attach_identity_for_test(requester_pid).await;
    let commands = handler
        .parse_control_commands("switch-client -T root ; new-window -d")
        .await
        .expect("key-table-only switch queue parses");
    crate::handler::with_expected_attach_and_session_identity(
        identity,
        alpha.clone(),
        identity.session_id(),
        handler.execute_parsed_commands_for_test(requester_pid, commands),
    )
    .await
    .expect("key-table-only switch allows its queue tail");

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client remains registered");
    assert_eq!(active.session_name, alpha);
    assert_eq!(active.key_table_name.as_deref(), Some("root"));
    drop(active_attach);
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("attached session survives")
            .windows()
            .len(),
        2,
        "the suffix must continue after a key-table-only switch response"
    );
}

#[tokio::test]
async fn attached_read_only_toggle_switch_allows_a_read_only_queue_tail() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 85;
    let alpha = session_name("read-only-switch-alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    let identity = handler.active_attach_identity_for_test(requester_pid).await;
    let commands = handler
        .parse_control_commands("switch-client -r ; display-message -p queue-tail")
        .await
        .expect("read-only switch queue parses");
    crate::handler::with_expected_attach_and_session_identity(
        identity,
        alpha,
        identity.session_id(),
        handler.execute_parsed_commands_for_test(requester_pid, commands),
    )
    .await
    .expect("read-only switch allows a read-only queue tail");

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client remains registered");
    assert!(
        active
            .flags
            .contains(crate::client_flags::ClientFlags::READONLY),
        "switch-client -r must still toggle the client flag"
    );
}

#[tokio::test]
async fn attached_binding_switch_client_rebases_its_command_queue() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 74;
    let alpha = session_name("binding-switch-alpha");
    let beta = session_name("binding-switch-beta");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
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

    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "W".to_owned(),
            note: Some("switch-client-queue".to_owned()),
            repeat: false,
            command: Some(vec![
                "switch-client".to_owned(),
                "-t".to_owned(),
                beta.to_string(),
                ";".to_owned(),
                "new-window".to_owned(),
                "-d".to_owned(),
                ";".to_owned(),
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "switch-tail".to_owned(),
                "done".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02W")
        .await
        .expect("prefix W dispatches switch-client queue");

    wait_for_buffer_contents(&handler, "switch-tail", b"done").await;
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client remains registered");
    assert_eq!(active.session_name, beta);
    drop(active_attach);
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("source session survives")
            .windows()
            .len(),
        1,
        "implicit suffix commands must stop targeting the source session"
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .expect("switched session survives")
            .windows()
            .len(),
        2,
        "implicit suffix commands must use the switched session cursor"
    );
}

#[tokio::test]
async fn attached_switch_rebases_wrappers_but_preserves_suffix_targets() {
    for entry_path in [
        "source-file",
        "if-shell",
        "run-shell",
        "run-shell-suffix-target",
    ] {
        let handler = RequestHandler::new();
        let requester_pid = match entry_path {
            "source-file" => u32::MAX - 90,
            "if-shell" => u32::MAX - 91,
            "run-shell" => u32::MAX - 92,
            "run-shell-suffix-target" => u32::MAX - 88,
            _ => unreachable!("enumerated entry path"),
        };
        let alpha = session_name(&format!("explicit-{entry_path}-alpha"));
        let beta = session_name(&format!("explicit-{entry_path}-beta"));
        let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
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

        let suffix_has_explicit_target = entry_path == "run-shell-suffix-target";
        let nested = if suffix_has_explicit_target {
            format!("switch-client -t {beta} ; new-window -d -t {alpha}")
        } else {
            format!("switch-client -t {beta} ; new-window -d")
        };
        let command = match entry_path {
            "source-file" => {
                let source_path = std::env::temp_dir().join(format!(
                    "rmux-explicit-source-target-{}-{requester_pid}.conf",
                    std::process::id()
                ));
                std::fs::write(&source_path, &nested).expect("explicit source-file fixture");
                format!(
                    "source-file -t {alpha}:0.0 {}",
                    quote_command_argument(&source_path.to_string_lossy())
                )
            }
            "if-shell" => format!("if-shell -F -t {alpha}:0.0 1 {{ {nested} }}"),
            "run-shell" | "run-shell-suffix-target" => format!(
                "run-shell -C -t {alpha}:0.0 {}",
                quote_command_argument(&nested)
            ),
            _ => unreachable!("enumerated entry path"),
        };
        let identity = handler.active_attach_identity_for_test(requester_pid).await;
        let commands = handler
            .parse_control_commands(&command)
            .await
            .expect("explicit nested queue parses");

        crate::handler::with_expected_attach_and_session_identity(
            identity,
            alpha.clone(),
            identity.session_id(),
            handler.execute_parsed_commands_for_test(requester_pid, commands),
        )
        .await
        .expect("nested queue completes after attached switch");

        let active_attach = handler.active_attach.lock().await;
        let expected_alpha_windows = if suffix_has_explicit_target { 2 } else { 1 };
        let expected_beta_windows = if suffix_has_explicit_target { 1 } else { 2 };
        assert_eq!(
            active_attach
                .by_pid
                .get(&requester_pid)
                .expect("attached client remains registered")
                .session_name,
            beta,
            "{entry_path} switch must still move the attached client"
        );
        drop(active_attach);
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&alpha)
                .expect("explicit target session survives")
                .windows()
                .len(),
            expected_alpha_windows,
            "{entry_path} must honor the suffix command's effective target"
        );
        assert_eq!(
            state
                .sessions
                .session(&beta)
                .expect("switched session survives")
                .windows()
                .len(),
            expected_beta_windows,
            "{entry_path} must distinguish wrapper context from a suffix command target"
        );
    }
}

#[tokio::test]
async fn attached_run_shell_inherited_target_rebases_after_switch() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 89;
    let alpha = session_name("inherited-run-shell-alpha");
    let beta = session_name("inherited-run-shell-beta");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
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

    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "W".to_owned(),
            note: Some("inherited run-shell target rebase".to_owned()),
            repeat: false,
            command: Some(vec![
                "run-shell".to_owned(),
                "-C".to_owned(),
                format!("switch-client -t {beta} ; new-window -d"),
            ]),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)), "{response:?}");

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02W")
        .await
        .expect("inherited run-shell binding dispatches");

    let active_attach = handler.active_attach.lock().await;
    assert_eq!(
        active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client remains registered")
            .session_name,
        beta
    );
    drop(active_attach);
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("source session survives")
            .windows()
            .len(),
        1,
        "an inherited run-shell target must not pin the queue tail to the source session"
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .expect("switched session survives")
            .windows()
            .len(),
        2,
        "the run-shell queue tail must follow the switched attached cursor"
    );
}

#[tokio::test]
async fn attached_attach_session_rebases_every_queue_entry_path() {
    for entry_path in ["direct", "source-file", "if-shell"] {
        let handler = RequestHandler::new();
        let requester_pid = match entry_path {
            "direct" => u32::MAX - 93,
            "source-file" => u32::MAX - 94,
            "if-shell" => u32::MAX - 95,
            _ => unreachable!("enumerated entry path"),
        };
        let alpha = session_name(&format!("attach-{entry_path}-alpha"));
        let beta = session_name(&format!("attach-{entry_path}-beta"));
        let buffer_name = format!("attach-{entry_path}-tail");
        let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
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

        let nested = format!("attach-session -t {beta} ; set-buffer -b {buffer_name} done");
        let command = match entry_path {
            "direct" => nested,
            "source-file" => {
                let source_path = std::env::temp_dir().join(format!(
                    "rmux-attached-attach-session-{}-{requester_pid}.conf",
                    std::process::id()
                ));
                std::fs::write(&source_path, &nested).expect("attach-session source fixture");
                format!(
                    "source-file {}",
                    quote_command_argument(&source_path.to_string_lossy())
                )
            }
            "if-shell" => format!("if-shell -F 1 {{ {nested} }}"),
            _ => unreachable!("enumerated entry path"),
        };
        let identity = handler.active_attach_identity_for_test(requester_pid).await;
        let commands = handler
            .parse_control_commands(&command)
            .await
            .expect("attach-session queue parses");

        crate::handler::with_expected_attach_and_session_identity(
            identity,
            alpha,
            identity.session_id(),
            handler.execute_parsed_commands_for_test(requester_pid, commands),
        )
        .await
        .expect("attach-session queue must continue after its session transition");

        wait_for_buffer_contents(&handler, &buffer_name, b"done").await;
        let active_attach = handler.active_attach.lock().await;
        assert_eq!(
            active_attach
                .by_pid
                .get(&requester_pid)
                .expect("attached client remains registered")
                .session_name,
            beta,
            "{entry_path} attach-session must move the attached client"
        );
    }
}

#[tokio::test]
async fn attached_switch_response_race_fails_closed_before_queue_continuation() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 79;
    let alpha = session_name("switch-race-alpha");
    let beta = session_name("switch-race-beta");
    let gamma = session_name("switch-race-gamma");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    for session in [&beta, &gamma] {
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

    let identity = handler.active_attach_identity_for_test(requester_pid).await;
    let pause = handler.install_attached_queue_switch_response_pause(identity);
    let commands = handler
        .parse_control_commands(&format!(
            "switch-client -c {requester_pid} -t {beta} ; new-window -d"
        ))
        .await
        .expect("racing switch-client queue parses");
    let queue_handler = handler.clone();
    let queue_alpha = alpha.clone();
    let queue = tokio::spawn(async move {
        crate::handler::with_expected_attach_and_session_identity(
            identity,
            queue_alpha,
            identity.session_id(),
            queue_handler.execute_parsed_commands_for_test(requester_pid, commands),
        )
        .await
    });

    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, pause.reached.notified())
        .await
        .expect("first switch reaches response correlation pause");
    let raced = handler
        .handle(Request::SwitchClient(SwitchClientRequest {
            target: gamma.clone(),
        }))
        .await;
    assert!(matches!(raced, Response::SwitchClient(_)), "{raced:?}");
    pause.release.notify_one();

    let error = queue
        .await
        .expect("racing queue task joins")
        .expect_err("stale beta response must fail closed");
    assert!(
        matches!(
            error,
            RmuxError::Server(ref message)
                if message.contains("switch-client response no longer matches")
        ),
        "unexpected fail-close error: {error:?}"
    );
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client remains registered");
    assert_eq!(active.session_name, gamma);
    drop(active_attach);
    let state = handler.state.lock().await;
    for session in [&alpha, &beta, &gamma] {
        assert_eq!(
            state
                .sessions
                .session(session)
                .expect("race session survives")
                .windows()
                .len(),
            1,
            "the queue suffix must not run after a stale switch response"
        );
    }
}

#[tokio::test]
async fn attached_same_session_switch_race_uses_the_committed_pane_target() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 83;
    let alpha = session_name("same-session-switch-race");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: rmux_proto::SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    let (pane_zero_id, pane_one_id) = {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&alpha)
            .expect("race session exists")
            .window_at(0)
            .expect("race window exists");
        (
            window.pane(0).expect("pane zero exists").id(),
            window.pane(1).expect("pane one exists").id(),
        )
    };

    let identity = handler.active_attach_identity_for_test(requester_pid).await;
    let pause = handler.install_attached_queue_switch_response_pause(identity);
    let commands = handler
        .parse_control_commands(&format!(
            "switch-client -c {requester_pid} -t {alpha}:0.0 ; kill-pane"
        ))
        .await
        .expect("same-session racing queue parses");
    let queue_handler = handler.clone();
    let queue_alpha = alpha.clone();
    let queue = tokio::spawn(async move {
        crate::handler::with_expected_attach_and_session_identity(
            identity,
            queue_alpha,
            identity.session_id(),
            queue_handler.execute_parsed_commands_for_test(requester_pid, commands),
        )
        .await
    });

    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, pause.reached.notified())
        .await
        .expect("first pane switch reaches response correlation pause");
    let raced = handler
        .handle(Request::SwitchClientExt3(Box::new(
            rmux_proto::request::SwitchClientExt3Request {
                target_client: Some(requester_pid.to_string()),
                target: Some(format!("{alpha}:0.1")),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: false,
            },
        )))
        .await;
    assert!(matches!(raced, Response::SwitchClient(_)), "{raced:?}");
    pause.release.notify_one();
    queue
        .await
        .expect("same-session racing queue task joins")
        .expect("same-session selection change does not invalidate the committed target");

    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&alpha)
        .expect("race session survives")
        .window_at(0)
        .expect("race window survives");
    assert!(
        window.panes().iter().all(|pane| pane.id() != pane_zero_id),
        "the queue tail must act on the pane committed by its own switch"
    );
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>(),
        vec![pane_one_id],
        "a later same-session switch must not redirect the earlier queue tail"
    );
}

#[tokio::test]
async fn attached_switch_rebase_rejects_a_stale_committed_pane_identity() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 84;
    let alpha = session_name("stale-committed-pane");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let identity = handler.active_attach_identity_for_test(requester_pid).await;
    let (window_id, pane_id) = {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&alpha)
            .expect("stale-target session exists")
            .window_at(0)
            .expect("stale-target window exists");
        (
            window.id(),
            window.pane(0).expect("stale-target pane exists").id(),
        )
    };
    let stale = crate::handler::attach_support::AttachedSwitchCommittedTarget {
        target: PaneTarget::new(alpha.clone(), 0),
        session_id: identity.session_id(),
        window_id,
        pane_id: rmux_proto::PaneId::new(pane_id.as_u32().saturating_add(1)),
    };
    let error = crate::handler::with_expected_attach_and_session_identity(
        identity,
        alpha.clone(),
        identity.session_id(),
        crate::handler::rebase_expected_attach_session_after_switch(
            &handler,
            requester_pid,
            crate::handler::client_support::SwitchManagedClientIdentity::Attach {
                pid: requester_pid,
                attach_id: identity.attach_id(),
            },
            &alpha,
            Some(stale),
        ),
    )
    .await
    .expect_err("a reused pane slot with a different stable identity must fail closed");
    assert!(
        matches!(
            error,
            RmuxError::Server(ref message)
                if message.contains("switch-client response no longer matches")
        ),
        "unexpected stale-target error: {error:?}"
    );
}

#[tokio::test]
async fn attached_switch_for_other_client_preserves_requester_queue_cursor() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 80;
    let other_pid = u32::MAX - 81;
    let alpha = session_name("other-switch-alpha");
    let beta = session_name("other-switch-beta");
    let gamma = session_name("other-switch-gamma");
    let delta = session_name("other-switch-delta");
    let _requester_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    for session in [&beta, &gamma, &delta] {
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
    let (other_tx, _other_rx) = mpsc::unbounded_channel();
    let other_attach_id = handler
        .register_attach(other_pid, gamma.clone(), other_tx)
        .await;

    let identity = handler.active_attach_identity_for_test(requester_pid).await;
    let commands = handler
        .parse_control_commands(&format!(
            "switch-client -c {other_pid} -t {delta} ; new-window -d"
        ))
        .await
        .expect("other-client switch queue parses");
    let context = crate::handler::scripting_support::QueueExecutionContext::without_caller_cwd()
        .with_implicit_current_target(Some(rmux_proto::Target::Pane(PaneTarget::new(
            beta.clone(),
            0,
        ))));
    crate::handler::with_expected_attach_and_session_identity(
        identity,
        alpha.clone(),
        identity.session_id(),
        handler.execute_parsed_commands(requester_pid, commands, context),
    )
    .await
    .expect("switching another client preserves the requester queue");

    let active_attach = handler.active_attach.lock().await;
    assert_eq!(
        active_attach
            .by_pid
            .get(&requester_pid)
            .expect("requester remains attached")
            .session_name,
        alpha
    );
    assert_eq!(
        active_attach
            .by_pid
            .get(&other_pid)
            .expect("other client remains attached")
            .session_name,
        delta
    );
    drop(active_attach);
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("requester session survives")
            .windows()
            .len(),
        1,
        "-c other-client must not rebase the requester onto its attached session"
    );
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .expect("captured queue target survives")
            .windows()
            .len(),
        2,
        "the suffix must retain the requester's pre-switch queue target"
    );
    assert_eq!(
        state
            .sessions
            .session(&delta)
            .expect("other client target survives")
            .windows()
            .len(),
        1,
        "the other client's switch target must not become the requester queue target"
    );
    drop(state);

    let reswitched = handler
        .handle(Request::SwitchClientExt3(Box::new(
            rmux_proto::request::SwitchClientExt3Request {
                target_client: Some(other_pid.to_string()),
                target: Some(gamma.to_string()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: false,
            },
        )))
        .await;
    assert!(matches!(reswitched, Response::SwitchClient(_)));
    let stale_other_response = crate::handler::with_expected_attach_and_session_identity(
        identity,
        alpha,
        identity.session_id(),
        crate::handler::rebase_expected_attach_session_after_switch(
            &handler,
            requester_pid,
            crate::handler::client_support::SwitchManagedClientIdentity::Attach {
                pid: other_pid,
                attach_id: other_attach_id,
            },
            &delta,
            None,
        ),
    )
    .await
    .expect("another client's later switch must not fail the requester queue");
    assert!(
        stale_other_response.is_none(),
        "another client must never rebase the requester queue"
    );
}

#[tokio::test]
async fn attached_binding_allows_an_explicit_cross_session_target() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 75;
    let alpha = session_name("binding-cross-alpha");
    let beta = session_name("binding-cross-beta");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
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

    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "Y".to_owned(),
            note: Some("explicit-cross-session-target".to_owned()),
            repeat: false,
            command: Some(vec![
                "kill-session".to_owned(),
                "-t".to_owned(),
                beta.to_string(),
                ";".to_owned(),
                "set-buffer".to_owned(),
                "-b".to_owned(),
                "cross-tail".to_owned(),
                "done".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)));

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02Y")
        .await
        .expect("prefix Y dispatches cross-session queue");

    wait_for_buffer_contents(&handler, "cross-tail", b"done").await;
    let state = handler.state.lock().await;
    assert!(state.sessions.contains_session(&alpha));
    assert!(!state.sessions.contains_session(&beta));
}

#[tokio::test]
async fn attached_command_prompt_switch_client_rebases_its_continuation() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 76;
    let alpha = session_name("prompt-switch-alpha");
    let beta = session_name("prompt-switch-beta");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
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

    let input = format!(
        "\x02:switch-client -t {} ; set-buffer -b prompt-switch-tail done\r",
        beta
    );
    handler
        .handle_attached_live_input_for_test(requester_pid, input.as_bytes())
        .await
        .expect("attached command prompt accepts switch-client queue");

    wait_for_buffer_contents(&handler, "prompt-switch-tail", b"done").await;
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client remains registered");
    assert_eq!(active.session_name, beta);
}

#[tokio::test]
async fn attached_command_prompt_attach_session_rebases_its_continuation() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 97;
    let alpha = session_name("prompt-attach-alpha");
    let beta = session_name("prompt-attach-beta");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
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

    let input = format!(
        "\x02:attach-session -t {} ; set-buffer -b prompt-attach-tail done\r",
        beta
    );
    handler
        .handle_attached_live_input_for_test(requester_pid, input.as_bytes())
        .await
        .expect("attached command prompt accepts attach-session queue");

    wait_for_buffer_contents(&handler, "prompt-attach-tail", b"done").await;
    let active_attach = handler.active_attach.lock().await;
    assert_eq!(
        active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client remains registered")
            .session_name,
        beta
    );
}

#[tokio::test]
async fn attached_command_prompt_renames_current_session() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02:rename-session beta\r")
        .await
        .expect("prefix command prompt input");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let state = handler.state.lock().await;
        if state.sessions.contains_session(&beta) {
            assert!(!state.sessions.contains_session(&alpha));
            break;
        }
        drop(state);
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for command prompt rename-session"
        );
        sleep(Duration::from_millis(25)).await;
    }

    let frame = wait_for_switch_frame_containing(&mut control_rx, "[beta]").await;
    assert!(
        !frame.contains("[alpha]"),
        "renamed session status must not keep old name: {frame:?}"
    );
}

#[tokio::test]
async fn attached_command_prompt_can_create_window_from_same_read() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, PROMPT_NEW_WINDOW_INPUT)
        .await
        .expect("prefix command prompt input");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let windows = active_windows(&handler, &alpha).await;
        if windows == "0:0\n1:1\n" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for prompt-created window, got {windows:?}"
        );
        sleep(Duration::from_millis(25)).await;
    }

    let target = PaneTarget::with_window(alpha.clone(), 1, 0);
    wait_for_capture_containing(
        &handler,
        target,
        "ISSUE8_WINDOW_READY",
        "prompt-created window should publish its first output",
    )
    .await;
    handler.refresh_attached_session(&alpha).await;

    let frame = wait_for_attach_output_containing(&mut control_rx, "ISSUE8_WINDOW_READY").await;
    assert!(
        frame.contains("ISSUE8_WINDOW_READY"),
        "prompt-created window must render its first output, got {frame:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn attached_exit_notifies_after_command_prompt_rename_session() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02:rename-session beta\r")
        .await
        .expect("prefix command prompt input");

    let _ = wait_for_switch_frame_containing(&mut control_rx, "[beta]").await;
    prepare_attached_shell_prompt(&handler, &PaneTarget::new(beta.clone(), 0)).await;
    drain_attach_controls(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"exit\r")
        .await
        .expect("exit input after rename-session");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match control_rx.recv().await {
                Some(AttachControl::Exited) => break,
                Some(_) => {}
                None => panic!("attach control channel closed before exit notification"),
            }
        }
    })
    .await
    .expect("timed out waiting for attach exit notification after renamed exit");
    wait_for_session_removed(&handler, &beta).await;
}

#[cfg(windows)]
#[tokio::test]
async fn attached_windows_input_exits_after_command_prompt_rename_session() {
    // Windows consoles do not make byte 0x04 a reliable EOF signal, so this
    // uses a controlled line protocol to verify the post-rename attach target.
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let mut control_rx =
        create_line_exiting_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02:rename-session beta\r")
        .await
        .expect("prefix command prompt input");

    let _ = wait_for_switch_frame_containing(&mut control_rx, "[beta]").await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b"RMUX_EXIT\r\n")
        .await
        .expect("Windows exit input after rename-session");

    tokio::time::timeout(WINDOWS_ATTACH_EXIT_TIMEOUT, async {
        loop {
            match control_rx.recv().await {
                Some(AttachControl::Exited) => break,
                Some(_) => {}
                None => panic!("attach control channel closed before exit notification"),
            }
        }
    })
    .await
    .expect("timed out waiting for attach exit notification after renamed Windows input");
    wait_for_session_removed(&handler, &beta).await;
}

#[tokio::test]
async fn attached_session_status_updates_after_external_rename() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    let renamed = handler
        .handle(Request::RenameSession(rmux_proto::RenameSessionRequest {
            target: alpha.clone(),
            new_name: beta.clone(),
        }))
        .await;
    assert!(matches!(renamed, Response::RenameSession(_)));

    let frame = wait_for_switch_frame_containing(&mut control_rx, "[beta]").await;
    assert!(
        !frame.contains("[alpha]"),
        "externally renamed session status must not keep old name: {frame:?}"
    );
}

#[tokio::test]
async fn attached_prefix_confirm_accepts_following_key_in_same_read_after_split() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02%")
        .await
        .expect("prefix split input");
    wait_for_active_panes(&handler, &alpha, "0:0\n1:1\n").await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02xy")
        .await
        .expect("prefix confirm input");
    wait_for_active_panes(&handler, &alpha, "0:1\n").await;
}

#[tokio::test]
async fn attached_kill_last_pane_exits_the_session() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    let killed = handler
        .handle(Request::KillPane(rmux_proto::KillPaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            kill_all_except: false,
        }))
        .await;
    assert_eq!(
        killed,
        Response::KillPane(rmux_proto::KillPaneResponse {
            target: PaneTarget::new(alpha.clone(), 0),
            window_destroyed: true,
        })
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match control_rx.recv().await {
                Some(AttachControl::Exited) => break,
                Some(_) => {}
                None => panic!("attach control channel closed before exit notification"),
            }
        }
    })
    .await
    .expect("timed out waiting for attach exit notification");
    wait_for_session_removed(&handler, &alpha).await;
}

async fn wait_for_buffer_contents(handler: &RequestHandler, name: &str, expected: &[u8]) {
    let deadline = tokio::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
    loop {
        let response = handler
            .handle(Request::ShowBuffer(rmux_proto::ShowBufferRequest {
                name: Some(name.to_owned()),
            }))
            .await;
        if let Some(output) = response.command_output() {
            if output.stdout() == expected {
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for buffer {name:?} to contain {expected:?}; last response: {response:?}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_global_option_value(handler: &RequestHandler, name: &str, expected: &str) {
    let deadline = tokio::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
    let expected_stdout = format!("{expected}\n").into_bytes();
    loop {
        let response = handler
            .handle(Request::ShowOptions(rmux_proto::ShowOptionsRequest {
                scope: rmux_proto::OptionScopeSelector::SessionGlobal,
                name: Some(name.to_owned()),
                value_only: true,
                include_inherited: false,
                quiet: false,
                include_hooks: false,
            }))
            .await;
        if let Some(output) = response.command_output() {
            if output.stdout() == expected_stdout {
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for option {name:?} to be {expected:?}; last response: {response:?}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_active_panes(handler: &RequestHandler, session: &SessionName, expected: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let panes = active_panes(handler, session).await;
        if panes == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for active panes {expected:?}, got {panes:?}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_file_contents(path: &Path, expected: &str) {
    let deadline = tokio::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
    loop {
        match std::fs::read_to_string(path) {
            Ok(contents) if contents == expected => return,
            Ok(contents) => assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {path:?} to contain {expected:?}; got {contents:?}"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {path:?} to be written"
            ),
            Err(error) => panic!("failed reading {path:?}: {error}"),
        }
        sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(windows)]
async fn wait_for_pane_lifecycle_command_containing(
    handler: &RequestHandler,
    target: PaneTarget,
    expected: &str,
) {
    let deadline = tokio::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
    let mut last_command = None;
    loop {
        {
            let state = handler.state.lock().await;
            let command = state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .and_then(|window| window.pane(target.pane_index()))
                .and_then(|pane| state.pane_lifecycle(pane.id()))
                .and_then(|lifecycle| lifecycle.command().map(|command| command.to_vec()));
            if let Some(command) = command {
                if command.iter().any(|argument| argument.contains(expected)) {
                    return;
                }
                last_command = Some(command);
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for pane {target:?} lifecycle command to contain {expected:?}; last command: {last_command:?}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

fn quote_command_argument(value: &str) -> String {
    crate::test_shell::command_quote(value)
}

fn quote_command_arguments(values: &[String]) -> String {
    values
        .iter()
        .map(|value| quote_command_argument(value))
        .collect::<Vec<_>>()
        .join(" ")
}

fn client_name_file_shell_command(path: &Path) -> String {
    #[cfg(unix)]
    {
        format!(
            "printf %s \"#{{client_name}}\" > {}",
            crate::test_shell::sh_quote_path(path)
        )
    }

    #[cfg(windows)]
    {
        format!(
            "[IO.File]::WriteAllText({}, '#{{client_name}}', [Text.UTF8Encoding]::new($false))",
            crate::test_shell::powershell_quote_path(path),
        )
    }
}

fn client_name_file_pane_command(path: &Path) -> Vec<String> {
    #[cfg(unix)]
    {
        vec![client_name_file_shell_command(path)]
    }

    #[cfg(windows)]
    {
        vec![
            windows_powershell_path(),
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            "& { param([string]$path, [string]$value) [IO.File]::WriteAllText($path, $value, [Text.UTF8Encoding]::new($false)) }".to_owned(),
            path.display().to_string(),
            "#{client_name}".to_owned(),
        ]
    }
}

#[cfg(windows)]
async fn set_windows_test_shell(handler: &RequestHandler) {
    let mut state = handler.state.lock().await;
    state
        .options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            windows_powershell_path(),
            SetOptionMode::Replace,
        )
        .expect("Windows test default-shell is valid");
}

#[cfg(windows)]
fn windows_powershell_path() -> String {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    std::path::PathBuf::from(system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
        .to_string_lossy()
        .into_owned()
}

async fn wait_for_switch_frame_containing(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    expected: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let control = match tokio::time::timeout(
            remaining.min(Duration::from_millis(250)),
            control_rx.recv(),
        )
        .await
        {
            Ok(Some(control)) => control,
            Ok(None) => panic!("attach refresh channel closed"),
            Err(_) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "timed out after {:?} waiting for attach frame containing {expected:?}",
                    ATTACH_LIFECYCLE_TIMEOUT
                );
                continue;
            }
        };
        if let AttachControl::Switch(target) = control {
            let frame = String::from_utf8(target.into_target().render_frame)
                .expect("render frame is utf-8");
            if frame.contains(expected) {
                return frame;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after {:?} waiting for attach frame containing {expected:?}",
            ATTACH_LIFECYCLE_TIMEOUT
        );
    }
}

async fn wait_for_attach_output_containing(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    expected: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
    let mut seen = String::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let control = match tokio::time::timeout(
            remaining.min(Duration::from_millis(250)),
            control_rx.recv(),
        )
        .await
        {
            Ok(Some(control)) => control,
            Ok(None) => panic!("attach refresh channel closed"),
            Err(_) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "timed out after {:?} waiting for attach output containing {expected:?}; saw {seen:?}",
                    ATTACH_LIFECYCLE_TIMEOUT
                );
                continue;
            }
        };
        match control {
            AttachControl::Switch(target) => {
                let target = target.into_target();
                seen.push_str(&String::from_utf8_lossy(&target.render_frame));
            }
            AttachControl::Overlay(frame) => {
                seen.push_str(&String::from_utf8_lossy(&frame.frame));
            }
            AttachControl::Write(bytes) => {
                seen.push_str(&String::from_utf8_lossy(&bytes));
            }
            _ => {}
        }
        if seen.contains(expected) {
            return seen;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after {:?} waiting for attach output containing {expected:?}; saw {seen:?}",
            ATTACH_LIFECYCLE_TIMEOUT
        );
    }
}

#[tokio::test]
async fn attached_resize_resizes_session_and_refreshes_status_frame() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_resize(
            requester_pid,
            TerminalSize {
                cols: 132,
                rows: 43,
            },
        )
        .await
        .expect("attached resize succeeds");

    {
        let client_size = {
            let active_attach = handler.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&requester_pid)
                .expect("attached client is tracked")
                .client_size
        };
        let state = handler.state.lock().await;
        let size = state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window()
            .size();
        assert_eq!(
            client_size,
            TerminalSize {
                cols: 132,
                rows: 43
            }
        );
        assert_eq!(
            size,
            TerminalSize {
                cols: 132,
                rows: 43
            }
        );
    }
    assert_eq!(
        pane_terminal_size(&handler, &alpha, 0, 0).await,
        TerminalSize {
            cols: 132,
            rows: 42
        }
    );
    let frame = recv_render_frame(&mut control_rx, "resize refresh").await;
    assert!(
        frame.contains("[alpha]"),
        "resize should redraw status for the attached client, got {frame:?}"
    );
}

#[tokio::test]
async fn attached_refresh_renders_each_client_at_its_own_size() {
    let handler = RequestHandler::new();
    let local_pid = 101;
    let browser_pid = 202;
    let alpha = session_name("alpha");
    let mut local_rx = create_attached_session(&handler, local_pid, &alpha).await;
    let (browser_tx, mut browser_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(browser_pid, alpha.clone(), browser_tx)
        .await;

    handler
        .handle_attached_resize(
            browser_pid,
            TerminalSize {
                cols: 132,
                rows: 43,
            },
        )
        .await
        .expect("browser resize succeeds");

    let local_frame = recv_render_frame(&mut local_rx, "local refresh").await;
    let browser_frame = recv_render_frame(&mut browser_rx, "browser refresh").await;
    assert!(
        local_frame.contains("\x1b[24;1H"),
        "local attach must keep a 24-row status line, got {local_frame:?}"
    );
    assert!(
        !local_frame.contains("\x1b[43;1H"),
        "local attach must not receive browser-sized redraws, got {local_frame:?}"
    );
    assert!(
        browser_frame.contains("\x1b[43;1H"),
        "browser attach should render at the browser-requested height, got {browser_frame:?}"
    );

    handler
        .refresh_attached_client_status(local_pid, &alpha)
        .await
        .expect("status refresh succeeds");
    let local_status = match recv_attach_control(&mut local_rx, "local status refresh").await {
        AttachControl::Write(bytes) => String::from_utf8(bytes).expect("status is utf-8"),
        other => panic!("expected status write, got {other:?}"),
    };
    assert!(
        local_status.contains("\x1b[24;1H"),
        "periodic status refresh must keep the local client height, got {local_status:?}"
    );
    assert!(
        !local_status.contains("\x1b[43;1H"),
        "periodic status refresh must not use the browser height, got {local_status:?}"
    );
}

#[tokio::test]
async fn attached_resize_ignores_zero_sized_terminal_reports() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_resize(requester_pid, TerminalSize { cols: 0, rows: 0 })
        .await
        .expect("zero-sized resize is ignored");

    let (client_size, session_size) = {
        let active_attach = handler.active_attach.lock().await;
        let client_size = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client is tracked")
            .client_size;
        drop(active_attach);

        let state = handler.state.lock().await;
        let session_size = state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window()
            .size();
        (client_size, session_size)
    };

    assert_eq!(client_size, TerminalSize { cols: 80, rows: 24 });
    assert_eq!(session_size, TerminalSize { cols: 80, rows: 24 });
    assert!(
        control_rx.try_recv().is_err(),
        "ignored zero-sized resize must not emit a refresh frame"
    );
}
