use super::*;

fn empty_split(alpha: &SessionName, keep: Option<bool>) -> Request {
    Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
        target: SplitWindowTarget::Pane(PaneTarget::with_window(alpha.clone(), 0, 0)),
        direction: SplitDirection::Vertical,
        before: false,
        environment: None,
        command: None,
        process_command: None,
        start_directory: None,
        keep_alive_on_exit: keep,
        detached: true,
        size: None,
        preserve_zoom: false,
        full_size: false,
        stdin_payload: Some(b"empty-pane-payload\n".to_vec()),
    }))
}

async fn split(handler: &RequestHandler, alpha: &SessionName, keep: Option<bool>) -> PaneTarget {
    match handler.handle(empty_split(alpha, keep)).await {
        Response::SplitWindow(response) => response.pane,
        other => panic!("empty split: {other:?}"),
    }
}

#[tokio::test]
async fn split_empty_has_output_without_a_process_or_forced_keep() {
    let handler = RequestHandler::new();
    let alpha = session_name("split-empty");
    create_session(&handler, &alpha).await;
    let target = split(&handler, &alpha, None).await;
    let state = handler.state.lock().await;
    let id = state
        .sessions
        .session(&alpha)
        .unwrap()
        .window_at(0)
        .unwrap()
        .pane(1)
        .unwrap()
        .id();
    assert_eq!(state.pane_pid_in_window(&alpha, 0, 1).unwrap(), 0);
    assert!(
        state.clone_pane_master(&alpha, 0, 1).is_err(),
        "no real or dummy PTY"
    );
    assert!(!state.pane_is_dead(&alpha, id));
    assert_eq!(state.pane_exit_metadata(&alpha, id), None);
    let lifecycle = state.pane_lifecycle(id).unwrap();
    assert_eq!(lifecycle.process, PaneLifecycleProcessState::Empty);
    assert_eq!(lifecycle.process_command(), None);
    assert_eq!(lifecycle.exit_state, None);
    assert!(state.has_pane_output(id.as_u32()));
    assert!(state.transcript_handle(&target).is_ok());
    assert_eq!(
        state
            .options
            .resolve_for_pane(&alpha, 0, 1, OptionName::RemainOnExit),
        Some("off")
    );
}

#[tokio::test]
async fn split_empty_preserves_explicit_boolean_keep_without_fixing_sc2() {
    let handler = RequestHandler::new();
    let alpha = session_name("split-empty-keep");
    create_session(&handler, &alpha).await;
    split(&handler, &alpha, Some(true)).await;
    let state = handler.state.lock().await;
    assert_eq!(state.pane_pid_in_window(&alpha, 0, 1).unwrap(), 0);
    assert_eq!(
        state
            .options
            .resolve_for_pane(&alpha, 0, 1, OptionName::RemainOnExit),
        Some("on")
    );
}

#[tokio::test]
async fn split_empty_resize_failure_rolls_back_output_options_and_layout() {
    let handler = RequestHandler::new();
    let alpha = session_name("split-empty-rollback");
    create_session(&handler, &alpha).await;
    let before = {
        let mut state = handler.state.lock().await;
        state.fail_next_resize_for_test();
        state.pane_output_ids()
    };
    assert!(matches!(
        handler.handle(empty_split(&alpha, Some(true))).await,
        Response::Error(_)
    ));
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .unwrap()
            .window_at(0)
            .unwrap()
            .pane_count(),
        1
    );
    assert_eq!(state.pane_output_ids(), before);
    drop(state);
    let target = split(&handler, &alpha, None).await;
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .options
            .resolve_for_pane(&alpha, 0, target.pane_index(), OptionName::RemainOnExit),
        Some("off")
    );
}

#[tokio::test]
async fn split_empty_respawn_without_kill_creates_first_process_and_new_generation() {
    let handler = RequestHandler::new();
    let alpha = session_name("split-empty-respawn");
    create_session(&handler, &alpha).await;
    let target = split(&handler, &alpha, None).await;
    let (id, generation) = {
        let state = handler.state.lock().await;
        let id = state
            .sessions
            .session(&alpha)
            .unwrap()
            .window_at(0)
            .unwrap()
            .pane(1)
            .unwrap()
            .id();
        (id, state.pane_lifecycle(id).unwrap().generation)
    };
    assert!(matches!(
        handler
            .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
                target: target.clone(),
                kill: false,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
            })))
            .await,
        Response::RespawnPane(_)
    ));
    let state = handler.state.lock().await;
    assert!(state.pane_pid_in_window(&alpha, 0, 1).unwrap() > 0);
    assert!(state.clone_pane_master(&alpha, 0, 1).is_ok());
    let lifecycle = state.pane_lifecycle(id).unwrap();
    assert!(matches!(
        lifecycle.process,
        PaneLifecycleProcessState::Running { .. }
    ));
    assert!(lifecycle.generation > generation);
    assert!(!state.pane_is_empty(id));
}

#[tokio::test]
async fn split_empty_removal_retires_output_and_lifecycle() {
    let handler = RequestHandler::new();
    let alpha = session_name("split-empty-kill");
    create_session(&handler, &alpha).await;
    let target = split(&handler, &alpha, None).await;
    let id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .unwrap()
            .window_at(0)
            .unwrap()
            .pane(1)
            .unwrap()
            .id()
    };
    assert!(matches!(
        handler
            .handle(Request::KillPane(KillPaneRequest {
                target,
                kill_all_except: false
            }))
            .await,
        Response::KillPane(_)
    ));
    let state = handler.state.lock().await;
    assert!(!state.has_pane_output(id.as_u32()));
    assert!(state.pane_lifecycle(id).is_none());
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .unwrap()
            .window_at(0)
            .unwrap()
            .pane_count(),
        1
    );
}

#[tokio::test]
async fn split_empty_seeds_theme_sources_before_its_first_query() {
    let handler = RequestHandler::new();
    let alpha = session_name("split-empty-theme");
    create_session(&handler, &alpha).await;
    let pid = 938175;
    let (tx, _rx) = mpsc::unbounded_channel();
    handler
        .register_attach_with_client_name(pid, "/dev/sc1-theme".into(), alpha.clone(), tx)
        .await;
    handler
        .handle_attached_live_input_for_test(pid, b"\x1b[?997;2n")
        .await
        .unwrap();
    let target = split(&handler, &alpha, None).await;
    let transcript = handler
        .state
        .lock()
        .await
        .transcript_handle(&target)
        .unwrap();
    assert_eq!(
        transcript
            .lock()
            .append_bytes_with_effects(b"\x1b[?996n")
            .replies,
        b"\x1b[?997;2n"
    );
    assert!(handler
        .state
        .lock()
        .await
        .clone_pane_master(&alpha, 0, 1)
        .is_err());
}
