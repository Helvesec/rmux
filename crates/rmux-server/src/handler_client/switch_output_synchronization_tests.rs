use super::*;

#[tokio::test]
async fn atomicity_session_split_and_window_use_quiet_live_commands() {
    let handler = RequestHandler::new();
    let alpha = session_name("switch-quiet-startup-alpha");
    create_session(&handler, alpha.clone()).await;
    create_second_pane(&handler, &alpha).await;
    let window_index = create_runtime_window(&handler, &alpha).await;
    let mut state = handler.state.lock().await;
    for (window, pane) in [(0, 0), (0, 1), (window_index, 0)] {
        let pane_id = state
            .sessions
            .session(&alpha)
            .unwrap()
            .pane_id_in_window(window, pane)
            .unwrap();
        assert_eq!(
            state.pane_lifecycle(pane_id).unwrap().process_command(),
            Some(&rmux_proto::ProcessCommand::Shell(
                crate::test_shell::stdin_discard_command()
            )),
            "fixture pane {window}.{pane} must not start an interactive shell"
        );
        assert!(state
            .clone_pane_master_if_alive(&alpha, window, pane)
            .is_ok());
    }
}

#[tokio::test]
async fn split_reader_refresh_outlives_initial_startup_completion() {
    delayed_reader_refresh(false).await;
}

#[tokio::test]
async fn window_reader_refresh_outlives_initial_startup_completion() {
    delayed_reader_refresh(true).await;
}

async fn delayed_reader_refresh(from_window: bool) {
    let handler = RequestHandler::new();
    let alpha = session_name("switch-reader-refresh-alpha");
    create_session(&handler, alpha.clone()).await;
    create_second_pane(&handler, &alpha).await;
    let source_window = if from_window {
        create_runtime_window(&handler, &alpha).await
    } else {
        // Leave the first split inactive, so its output requests a base redraw.
        create_second_pane(&handler, &alpha).await;
        0
    };
    let source_pane = if from_window { 0 } else { 1 };
    let source_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .unwrap()
        .pane_id_in_window(source_window, source_pane)
        .unwrap();
    let observer_pid = std::process::id();
    let (observer_tx, mut observer_rx) = mpsc::unbounded_channel();
    let observer_id = handler
        .register_attach(observer_pid, alpha.clone(), observer_tx)
        .await;
    // Native resize repaints can arrive even from the quiet child. Establish
    // the same boundary as the failure fixtures before draining controls.
    let dispatch = handler.pane_alert_dispatch.lock().await;
    open_zoomed_choose_tree(&handler, observer_pid).await;
    while observer_rx.try_recv().is_ok() {}
    let snapshot =
        capture_mode_tree_preservation_snapshot(&handler, &alpha, observer_pid, observer_id).await;

    // Exercise the actual reader callback and coalesced dispatcher, with no
    // switch request and no initial-session startup producer. Hold dispatch
    // across the old completion wait and queue drain to force the gap.
    handler
        .pane_alert_callback()
        .alert(crate::pane_io::PaneAlertEvent {
            session_name: alpha.clone(),
            pane_id: source_id,
            bell_count: 0,
            title_changed: false,
            title_change: None,
            path_changed: false,
            clipboard_set: false,
            clipboard_writes: Vec::new(),
            clipboard_queries: Vec::new(),
            mouse_mode_changed: false,
            alternate_mode_changed: false,
            queue_activity_alert: false,
            generation: None,
        });
    handler.wait_for_initial_pane_tasks_for_test().await;
    handler.wait_for_initial_panes_for_test().await;
    assert!(observer_rx.try_recv().is_err());
    assert_mode_tree_preserved(&handler, &snapshot, &mut observer_rx).await;
    drop(dispatch);

    let control = tokio::time::timeout(std::time::Duration::from_secs(2), observer_rx.recv())
        .await
        .expect("reader refresh delivered")
        .expect("observer remains attached");
    let crate::pane_io::AttachControl::Switch(target) = control else {
        panic!("reader refresh must emit its base Switch: {control:?}");
    };
    let target = target.into_target();
    assert_eq!(target.session_name, alpha);
    assert_eq!(
        target.persistent_overlay_state_id,
        Some(snapshot.mode_tree_state_id)
    );
    // The base Switch is attributed above, not accepted by atomicity tests.
    // Join the dispatch critical section before checking its trailing overlays.
    let _dispatch = handler.pane_alert_dispatch.lock().await;
    assert_mode_tree_preserved(&handler, &snapshot, &mut observer_rx).await;
}
