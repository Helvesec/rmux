use super::*;

#[tokio::test]
async fn client_theme_hook_kill_writes_committed_focus_in_without_recursive_hook_writers() {
    let handler = RequestHandler::new();
    let alpha = session_name("focus-theme-hook-kill");
    let requester_pid = std::process::id();
    create_send_keys_test_session(&handler, &alpha).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");

    let removed = PaneTarget::with_window(alpha.clone(), 0, 0);
    let survivor_before = PaneTarget::with_window(alpha.clone(), 0, 1);
    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: removed.clone(),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");
    enable_focus_mode(&handler, &removed).await;
    enable_focus_mode(&handler, &survivor_before).await;

    let (removed_pane_id, survivor_pane_id) = {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .expect("focus window exists");
        (
            window.pane(0).expect("removed pane exists").id(),
            window.pane(1).expect("survivor exists").id(),
        )
    };
    let _control_rx = register_focused_attach(&handler, requester_pid, &alpha).await;
    set_global_focus_hook(
        &handler,
        HookName::ClientDarkTheme,
        &format!("kill-pane -t {removed_pane_id}"),
    )
    .await;
    set_global_focus_hook(&handler, HookName::PaneFocusOut, "send-keys -l WRONG-OUT").await;
    set_global_focus_hook(&handler, HookName::PaneFocusIn, "send-keys -l WRONG-IN").await;

    let lifecycle_dispatch = handler
        .take_lifecycle_dispatch_receiver()
        .expect("lifecycle dispatch activates once");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let lifecycle_handler = handler.clone();
    let lifecycle_task = tokio::spawn(async move {
        lifecycle_handler
            .consume_lifecycle_hooks(lifecycle_dispatch, shutdown_rx)
            .await;
    });
    let mut lifecycle = handler.subscribe_lifecycle_events();
    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&removed);
        state.start_pane_input_capture_for_test(&survivor_before);
    }

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[?997;1n")
        .await
        .expect("client theme hook completes");

    let survivor_after = PaneTarget::with_window(alpha.clone(), 0, 0);
    {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .expect("window survives hook kill");
        assert_eq!(window.pane(0).map(|pane| pane.id()), Some(survivor_pane_id));
        assert_ne!(window.pane(0).map(|pane| pane.id()), Some(removed_pane_id));
        assert_eq!(
            state.pane_input_capture_for_test(&survivor_after),
            Some(b"\x1b[I".to_vec()),
            "the committed FOC effect must not inherit the removed command target identity"
        );
    }

    let focus_events = std::iter::from_fn(|| lifecycle.try_recv().ok())
        .map(|event| event.event)
        .filter(|event| {
            matches!(
                event,
                LifecycleEvent::PaneFocusOut { .. } | LifecycleEvent::PaneFocusIn { .. }
            )
        })
        .collect::<Vec<_>>();
    assert!(
        focus_events.is_empty(),
        "an internal focus effect must not recursively publish pane hooks: {focus_events:?}"
    );

    let _ = shutdown_tx.send(());
    lifecycle_task.await.expect("lifecycle task joins");
}
