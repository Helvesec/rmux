use super::*;

#[tokio::test]
async fn attached_mode_tree_acceptance_uses_mode_before_prefix_or_pty() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: alpha.clone(),
                name: Some("w1".to_owned()),
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

    let commands = handler
        .parse_control_commands("choose-tree -Zw")
        .await
        .expect("choose-tree parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, commands)
        .await
        .expect("choose-tree activates mode-tree");

    assert_eq!(
        pane_mode_status(&handler, &alpha).await,
        "1:tree-mode::\n0:::\n"
    );
    assert_eq!(
        display_target_format(
            &handler,
            PaneTarget::new(alpha.clone(), 0),
            "#{window_name}|#{pane_in_mode}|#{pane_mode}"
        )
        .await,
        "[tmux]|1|tree-mode\n"
    );
    {
        let state = handler.state.lock().await;
        let window_name = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.name().map(str::to_owned));
        assert_eq!(window_name.as_deref(), Some("[tmux]"));
    }
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x0e\r")
        .await
        .expect("mode-tree accepts attached keys");

    assert_eq!(active_windows(&handler, &alpha).await, "0:0\n1:1\n");
    assert_eq!(pane_mode_status(&handler, &alpha).await, "0:::\n0:::\n");
}

#[tokio::test]
async fn attached_compact_prefix_wq_uses_choose_tree_before_the_following_key() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: alpha.clone(),
                name: Some("w1".to_owned()),
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

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02wq")
        .await
        .expect("compact choose-tree close input");

    assert_eq!(pane_mode_status(&handler, &alpha).await, "0:::\n0:::\n");
    assert_eq!(
        display_target_format(
            &handler,
            PaneTarget::new(alpha.clone(), 0),
            "#{window_name}|#{pane_in_mode}|#{pane_mode}"
        )
        .await,
        default_shell_pane_status()
    );
}

#[tokio::test]
async fn attached_mode_tree_consumes_lone_escape_after_input_timeout() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("mode-tree-escape");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    let commands = handler
        .parse_control_commands("choose-tree -Zw")
        .await
        .expect("choose-tree parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, commands)
        .await
        .expect("choose-tree activates mode-tree");
    assert_eq!(pane_mode_status(&handler, &alpha).await, "1:tree-mode::\n");

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b")
        .await
        .expect("fragmented escape waits for the timeout");
    assert_eq!(pending_input, b"\x1b");

    let forwarded = handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("escape timeout closes mode-tree");

    assert!(!forwarded);
    assert!(pending_input.is_empty());
    assert_eq!(pane_mode_status(&handler, &alpha).await, "0:::\n");
}

#[tokio::test]
async fn attached_compact_prefix_tq_uses_clock_mode_before_the_following_key() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02tq")
        .await
        .expect("compact clock-mode close input");

    assert_eq!(pane_mode_status(&handler, &alpha).await, "0:::\n");
    assert_eq!(
        display_target_format(
            &handler,
            PaneTarget::new(alpha.clone(), 0),
            "#{window_name}|#{pane_in_mode}|#{pane_mode}"
        )
        .await,
        default_shell_pane_status()
    );
}
