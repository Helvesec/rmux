use super::*;

#[cfg(windows)]
const WINDOWS_ATTACH_EXIT_TIMEOUT: Duration = Duration::from_secs(20);

#[cfg(unix)]
const PROMPT_NEW_WINDOW_INPUT: &[u8] =
    b"\x02:new-window -- 'printf ISSUE8_WINDOW_READY; sleep 30'\r";

#[cfg(windows)]
const PROMPT_NEW_WINDOW_INPUT: &[u8] =
    b"\x02:new-window -- cmd.exe /d /q /c \"echo ISSUE8_WINDOW_READY & ping -n 30 127.0.0.1 >NUL\"\r";

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
            let frame = String::from_utf8(target.render_frame).expect("render frame is utf-8");
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
