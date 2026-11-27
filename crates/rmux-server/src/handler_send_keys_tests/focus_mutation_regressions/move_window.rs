use super::*;

#[tokio::test]
async fn focus_move_window_drains_before_link_and_selection_tickets() {
    let handler = RequestHandler::new();
    let source_session = session_name("focus-move-window-source");
    let destination_session = session_name("focus-move-window-destination");
    let requester_pid = std::process::id();
    create_send_keys_test_session(&handler, &source_session).await;
    create_send_keys_test_session(&handler, &destination_session).await;
    let source_survivor = add_detached_window(&handler, &source_session).await;
    let source = PaneTarget::with_window(source_session.clone(), 0, 0);
    let survivor =
        PaneTarget::with_window(source_session.clone(), source_survivor.window_index(), 0);
    let moved_source = PaneTarget::with_window(destination_session.clone(), 1, 0);
    enable_focus_mode(&handler, &source).await;
    enable_focus_mode(&handler, &survivor).await;
    let _control_rx = register_focused_attach(&handler, requester_pid, &source_session).await;
    let mut lifecycle = handler.subscribe_lifecycle_events();
    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&source);
        state.start_pane_input_capture_for_test(&survivor);
        state.start_pane_input_capture_for_test(&moved_source);
    }

    let response = tokio::time::timeout(
        Duration::from_secs(2),
        handler.handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(source_session.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(destination_session, 1)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        })),
    )
    .await
    .expect("focus move-window must drain before its reserved lifecycle effects");
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(&moved_source),
            Some(b"\x1b[O".to_vec())
        );
        assert_eq!(
            state.pane_input_capture_for_test(&survivor),
            Some(b"\x1b[I".to_vec())
        );
    }
    let events = focus_and_window_events(&mut lifecycle);
    assert!(
        matches!(
            events.as_slice(),
            [
                LifecycleEvent::PaneFocusOut { .. },
                LifecycleEvent::PaneFocusIn { .. },
                ..
            ]
        ),
        "{events:?}"
    );
    assert!(
        events.iter().skip(2).any(|event| matches!(
            event,
            LifecycleEvent::WindowLinked { .. }
                | LifecycleEvent::WindowUnlinked { .. }
                | LifecycleEvent::SessionWindowChanged { .. }
        )),
        "move-window must publish a lifecycle witness after focus: {events:?}"
    );

    let witness = tokio::time::timeout(
        Duration::from_secs(2),
        add_detached_window(&handler, &source_session),
    )
    .await
    .expect("the lifecycle queue must accept the command after move-window");
    assert_eq!(witness.session_name(), &source_session);
}

#[tokio::test]
async fn focus_new_window_override_drains_before_move_effect_tickets() {
    let handler = RequestHandler::new();
    let alpha = session_name("focus-new-window-override");
    let requester_pid = std::process::id();
    create_send_keys_test_session(&handler, &alpha).await;
    let removed = PaneTarget::with_window(alpha.clone(), 0, 0);
    enable_focus_mode(&handler, &removed).await;
    let _control_rx = register_focused_attach(&handler, requester_pid, &alpha).await;
    let removed_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .expect("override destination exists before replacement")
            .id()
    };
    // This fixture observes focus/lifecycle publication, not startup output.
    // Park the next pane before it exists: ConPTY itself enables DEC 1004,
    // independently of the child command. Keep the real replacement writer.
    let _replacement_output_held = {
        let state = handler.state.lock().await;
        let guard = state.pane_output_read_demand.acquire_attached_client();
        guard.set_pane_off(removed_pane_id.as_u32() + 1, true);
        let mut attaches = handler.active_attach.lock().await;
        // The fixture's attach has no rendering task. Its unrestricted read
        // demand would otherwise override the pane-off guard just installed.
        attaches
            .by_pid
            .get_mut(&requester_pid)
            .expect("focused fixture attach exists")
            ._read_demand =
            crate::pane_io::PaneOutputReadDemandRegistry::default().acquire_unrestricted();
        guard
    };
    set_global_focus_hook(
        &handler,
        HookName::PaneFocusOut,
        "set-buffer -b override-log logged",
    )
    .await;
    append_global_focus_hook(&handler, HookName::PaneFocusOut, "send-keys -l WRONG").await;
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
    let replacement_route = PaneTarget::with_window(alpha.clone(), 1, 0);
    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&removed);
        state.start_pane_input_capture_for_test(&replacement_route);
    }

    let response = tokio::time::timeout(
        Duration::from_secs(2),
        handler.execute_hook_command_with_context(
            requester_pid,
            &format!("new-window -k -t {}:0", alpha.as_str()),
            Some(Target::Pane(removed.clone())),
        ),
    )
    .await
    .expect("new-window -k must drain focus before its internal move effects");
    assert!(response.is_ok(), "{response:?}");

    let (replacement_pane_id, replacement_output) = {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .expect("replacement occupies the removed window slot")
            .id();
        assert_ne!(pane_id, removed_pane_id);
        assert!(!state.pane_is_empty(pane_id));
        let output = state
            .pane_output_for_target(&alpha, 0, 0)
            .expect("replacement has its own output");
        (pane_id, output)
    };
    // The empty-input assertion below requires a replacement whose native
    // startup has not enabled DEC 1004. Check the actual reader admission,
    // including on Unix where the shell need not request focus reporting.
    let mut replacement_read =
        Box::pin(replacement_output.wait_for_reader_demand(replacement_pane_id.as_u32(), None));
    std::future::poll_fn(|cx| {
        assert!(
            std::future::Future::poll(replacement_read.as_mut(), cx).is_pending(),
            "fixture must hold replacement startup output before focus reconciliation"
        );
        std::task::Poll::Ready(())
    })
    .await;

    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(&removed),
            Some(Vec::new()),
            "the removed runtime has no writer and the replacement did not prearm mode 1004"
        );
        assert_eq!(
            state.pane_input_capture_for_test(&replacement_route),
            Some(Vec::new()),
            "the removed runtime hook cannot target the replacement window"
        );
    }
    let queued = std::iter::from_fn(|| lifecycle.try_recv().ok())
        .filter(|event| {
            matches!(
                event.event,
                LifecycleEvent::PaneFocusIn { .. }
                    | LifecycleEvent::PaneFocusOut { .. }
                    | LifecycleEvent::WindowLinked { .. }
                    | LifecycleEvent::WindowUnlinked { .. }
                    | LifecycleEvent::SessionWindowChanged { .. }
            )
        })
        .collect::<Vec<_>>();
    let events = queued
        .iter()
        .map(|event| event.event.clone())
        .collect::<Vec<_>>();
    assert!(
        matches!(
            events.as_slice(),
            [
                LifecycleEvent::PaneFocusOut { .. },
                LifecycleEvent::PaneFocusIn { .. },
                ..
            ]
        ),
        "{events:?}"
    );
    assert!(
        events.iter().skip(2).any(|event| matches!(
            event,
            LifecycleEvent::WindowLinked { .. }
                | LifecycleEvent::WindowUnlinked { .. }
                | LifecycleEvent::SessionWindowChanged { .. }
        )),
        "override must publish its move lifecycle after focus: {events:?}"
    );
    let focus_out = queued
        .iter()
        .find(|event| matches!(event.event, LifecycleEvent::PaneFocusOut { .. }))
        .expect("override publishes historical pane-focus-out");
    assert!(
        focus_out.formats.iter().any(|(name, value)| {
            name == "hook_pane" && value == &format!("%{}", removed_pane_id.as_u32())
        }),
        "override PO keeps the removed PaneId: {:?}",
        focus_out.formats
    );
    assert_buffer(&handler, "override-log", "logged").await;
    handler
        .execute_hook_command_with_context(
            requester_pid,
            &format!("send-keys -t {replacement_pane_id} -l LIVE"),
            None,
        )
        .await
        .expect("the replacement still accepts an explicitly addressed live command");
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .pane_input_capture_for_test(&removed),
        Some(b"LIVE".to_vec()),
        "holding output must not suppress input to the replacement"
    );
    let _ = shutdown_tx.send(());
    lifecycle_task.await.expect("lifecycle task joins");
}

#[tokio::test]
async fn focus_move_window_without_contribution_and_rejected_move_publish_no_delta() {
    let handler = RequestHandler::new();
    let alpha = session_name("focus-move-window-unfocused");
    let beta = session_name("focus-move-window-unfocused-destination");
    let requester_pid = std::process::id();
    create_send_keys_test_session(&handler, &alpha).await;
    create_send_keys_test_session(&handler, &beta).await;
    let survivor_window = add_detached_window(&handler, &alpha).await;
    let removed = PaneTarget::with_window(alpha.clone(), 0, 0);
    enable_focus_mode(&handler, &removed).await;
    let _control_rx = register_focused_attach(&handler, requester_pid, &alpha).await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[O")
        .await
        .expect("outer focus-out removes the contribution");
    let mut lifecycle = handler.subscribe_lifecycle_events();

    let rejected = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 99)),
            target: MoveWindowTarget::Session(beta.clone()),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(rejected, Response::Error(_)), "{rejected:?}");
    assert!(
        focus_and_window_events(&mut lifecycle).is_empty(),
        "a rejected move must not publish lifecycle or focus"
    );

    let moved = tokio::time::timeout(
        Duration::from_secs(2),
        handler.handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha, 0)),
            target: MoveWindowTarget::Session(beta),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        })),
    )
    .await
    .expect("move-window without a focus contribution must remain bounded");
    assert!(matches!(moved, Response::MoveWindow(_)), "{moved:?}");
    assert!(
        focus_and_window_events(&mut lifecycle)
            .iter()
            .all(|event| !matches!(
                event,
                LifecycleEvent::PaneFocusIn { .. } | LifecycleEvent::PaneFocusOut { .. }
            )),
        "an unfocused attach contributes no pane delta"
    );
    assert_eq!(survivor_window.window_index(), 1);
}
