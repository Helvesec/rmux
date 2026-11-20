use super::*;

#[tokio::test]
async fn startup_completion_wait_includes_original_session_overlay_refresh() {
    let handler = RequestHandler::new();
    let alpha = session_name("switch-startup-refresh-alpha");
    create_session(&handler, alpha.clone()).await;
    create_second_pane(&handler, &alpha).await;
    let observer_pid = std::process::id();
    let (observer_tx, mut observer_rx) = mpsc::unbounded_channel();
    let observer_id = handler
        .register_attach(observer_pid, alpha.clone(), observer_tx)
        .await;
    open_zoomed_choose_tree(&handler, observer_pid).await;
    let _reader_dispatch = handler.pane_alert_dispatch.lock().await;
    while observer_rx.try_recv().is_ok() {}
    let snapshot =
        capture_mode_tree_preservation_snapshot(&handler, &alpha, observer_pid, observer_id).await;
    let session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .unwrap()
        .id();

    // Hold the same identity refresh called after Windows leaves Starting.
    // No switch request runs: every control below has this known producer.
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let refresh_handler = handler.clone();
    let refresh_alpha = alpha.clone();
    let task = tokio::spawn(async move {
        release_rx.await.expect("release startup refresh");
        refresh_handler
            .refresh_attached_session_for_session_identity(&refresh_alpha, session_id)
            .await;
    });
    handler.initial_pane_tasks.lock().unwrap().push(task);
    let mut completion = std::pin::pin!(handler.wait_for_initial_pane_tasks_for_test());
    let pending = std::future::poll_fn(|cx| {
        std::task::Poll::Ready(std::future::Future::poll(completion.as_mut(), cx).is_pending())
    })
    .await;
    assert!(
        pending,
        "fixture must not proceed while startup refresh is held"
    );
    assert!(observer_rx.try_recv().is_err());
    release_tx.send(()).expect("startup refresh still waiting");
    completion.await;

    let control = observer_rx
        .try_recv()
        .expect("startup base refresh delivered");
    let crate::pane_io::AttachControl::Switch(target) = control else {
        panic!("startup refresh did not emit its base Switch: {control:?}");
    };
    let target = target.into_target();
    assert_eq!(target.session_name, alpha);
    assert_eq!(
        target.persistent_overlay_state_id,
        Some(snapshot.mode_tree_state_id)
    );
    // Only the proven startup base control is consumed above. All remaining
    // controls must satisfy the unchanged mode/zoom/overlay causality checks.
    assert_mode_tree_preserved(&handler, &snapshot, &mut observer_rx).await;
    handler.wait_for_initial_pane_tasks_for_test().await;
    assert!(observer_rx.try_recv().is_err());
}
