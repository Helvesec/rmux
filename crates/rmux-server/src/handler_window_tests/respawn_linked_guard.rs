use super::*;

async fn create_linked_respawn_family(
    handler: &RequestHandler,
) -> (SessionName, SessionName, SessionName) {
    let owner = session_name("respawn-linked-guard-owner");
    let alias1 = session_name("respawn-linked-guard-alias1");
    let alias2 = session_name("respawn-linked-guard-alias2");
    create_session(handler, owner.as_str()).await;
    create_session(handler, alias1.as_str()).await;
    create_session(handler, alias2.as_str()).await;

    for alias in [&alias1, &alias2] {
        let response = handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(owner.clone(), 0),
                target: WindowTarget::with_window(alias.clone(), 0),
                after: false,
                before: false,
                kill_destination: true,
                detached: true,
            }))
            .await;
        assert!(
            matches!(response, Response::LinkWindow(_)),
            "expected linked alias setup, got {response:?}"
        );
    }
    handler.wait_for_initial_panes_for_test().await;

    (owner, alias1, alias2)
}

fn linked_targets(
    owner: &SessionName,
    alias1: &SessionName,
    alias2: &SessionName,
) -> [WindowTarget; 3] {
    [
        WindowTarget::with_window(owner.clone(), 0),
        WindowTarget::with_window(alias1.clone(), 0),
        WindowTarget::with_window(alias2.clone(), 0),
    ]
}

async fn append_linked_marker(handler: &RequestHandler, session_name: &SessionName, marker: &[u8]) {
    let mut state = handler.state.lock().await;
    state
        .append_bytes_to_pane_transcript_for_test(session_name, 0, 0, marker)
        .expect("linked marker transcript append succeeds");
}

async fn capture_pane_print(handler: &RequestHandler, target: PaneTarget) -> String {
    let response = handler
        .handle(Request::CapturePane(Box::new(
            rmux_proto::CapturePaneRequest {
                target,
                start: None,
                end: None,
                print: true,
                buffer_name: None,
                alternate: false,
                escape_ansi: false,
                escape_sequences: false,
                include_format: false,
                hyperlinks: false,
                line_numbers: false,
                join_wrapped: false,
                use_mode_screen: false,
                preserve_trailing_spaces: false,
                do_not_trim_spaces: false,
                pending_input: false,
                quiet: false,
                start_is_absolute: false,
                end_is_absolute: false,
            },
        )))
        .await;
    let Response::CapturePane(response) = response else {
        panic!("expected capture-pane response, got {response:?}");
    };
    let output = response
        .command_output()
        .expect("capture-pane -p should return command output");
    String::from_utf8(output.stdout().to_vec()).expect("capture-pane stdout is utf-8")
}

async fn pane_pid(handler: &RequestHandler, target: &WindowTarget) -> u32 {
    let state = handler.state.lock().await;
    state
        .pane_pid_in_window(target.session_name(), target.window_index(), 0)
        .expect("linked pane pid is available")
}

async fn respawn_window(handler: &RequestHandler, target: WindowTarget, kill: bool) -> Response {
    handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target,
            kill,
            start_directory: None,
            environment: None,
            command: Some(quiet_window_test_command()),
        })))
        .await
}

#[tokio::test]
async fn respawn_window_without_kill_rejects_active_linked_window_from_each_alias() {
    let handler = RequestHandler::new();
    let (owner, alias1, alias2) = create_linked_respawn_family(&handler).await;
    let owner_target = WindowTarget::with_window(owner.clone(), 0);
    let owner_pane = PaneTarget::with_window(owner.clone(), 0, 0);

    append_linked_marker(&handler, &owner, b"respawn-linked-old").await;
    let initial_pid = pane_pid(&handler, &owner_target).await;
    let initial_capture = capture_pane_print(&handler, owner_pane.clone()).await;
    assert!(
        initial_capture.contains("respawn-linked-old"),
        "expected linked marker in capture, got {initial_capture:?}"
    );

    for target in linked_targets(&owner, &alias1, &alias2) {
        let response = respawn_window(&handler, target.clone(), false).await;
        assert!(
            matches!(&response, Response::Error(error) if error.error.to_string().contains("still active")),
            "expected still-active error for {target}, got {response:?}"
        );
        assert_eq!(
            pane_pid(&handler, &owner_target).await,
            initial_pid,
            "respawn-window without -k must preserve the shared runtime from {target}"
        );
        assert_eq!(
            capture_pane_print(&handler, owner_pane.clone()).await,
            initial_capture,
            "respawn-window without -k must preserve pane contents from {target}"
        );
    }
}

#[tokio::test]
async fn respawn_window_with_kill_restarts_active_linked_window_from_each_alias() {
    let handler = RequestHandler::new();
    let (owner, alias1, alias2) = create_linked_respawn_family(&handler).await;
    let owner_target = WindowTarget::with_window(owner.clone(), 0);

    for target in linked_targets(&owner, &alias1, &alias2) {
        let before_pid = pane_pid(&handler, &owner_target).await;
        let response = respawn_window(&handler, target.clone(), true).await;
        assert!(
            matches!(&response, Response::RespawnWindow(result) if result.target == target),
            "expected respawn-window -k success for {target}, got {response:?}"
        );
        handler.wait_for_initial_panes_for_test().await;

        let after_pid = pane_pid(&handler, &owner_target).await;
        assert_ne!(
            after_pid, before_pid,
            "respawn-window -k must restart the shared runtime from {target}"
        );
        for linked_target in linked_targets(&owner, &alias1, &alias2) {
            assert_eq!(
                pane_pid(&handler, &linked_target).await,
                after_pid,
                "linked target {linked_target} must resolve the restarted runtime from {target}"
            );
        }
    }
}
