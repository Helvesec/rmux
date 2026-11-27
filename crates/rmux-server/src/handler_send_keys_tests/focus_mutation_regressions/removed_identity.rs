use super::*;

#[tokio::test]
async fn removed_focus_out_identity_cannot_send_keys_to_reindexed_survivor() {
    let handler = RequestHandler::new();
    let alpha = session_name("focus-removed-pane-identity");
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
    let (removed_pane_id, surviving_pane_id) = {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .expect("focus identity window exists");
        (
            window.pane(0).expect("removed pane exists").id(),
            window.pane(1).expect("surviving pane exists").id(),
        )
    };
    set_global_focus_hook(
        &handler,
        HookName::PaneFocusOut,
        "set-buffer -b removed-log logged",
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
    let _control_rx = register_focused_attach(&handler, requester_pid, &alpha).await;
    let mut lifecycle = handler.subscribe_lifecycle_events();
    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&removed);
        state.start_pane_input_capture_for_test(&survivor_before);
    }

    let response = handler
        .handle(Request::KillPane(rmux_proto::KillPaneRequest {
            target: removed.clone(),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    let survivor_after = PaneTarget::with_window(alpha.clone(), 0, 0);
    {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .expect("window survives focused pane removal");
        assert_eq!(
            window.pane(0).map(|pane| pane.id()),
            Some(surviving_pane_id)
        );
        assert_ne!(window.pane(0).map(|pane| pane.id()), Some(removed_pane_id));
        assert_eq!(
            state.pane_input_capture_for_test(&survivor_after),
            Some(b"\x1b[I".to_vec()),
            "the historical focus-out hook must not target the pane reusing slot zero"
        );
    }

    let focus_events = std::iter::from_fn(|| lifecycle.try_recv().ok())
        .filter(|event| {
            matches!(
                event.event,
                LifecycleEvent::PaneFocusIn { .. } | LifecycleEvent::PaneFocusOut { .. }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(focus_events.len(), 2, "{focus_events:?}");
    let focus_out = &focus_events[0];
    assert_eq!(
        focus_out.event,
        LifecycleEvent::PaneFocusOut {
            target: removed.clone()
        }
    );
    assert!(
        focus_out.formats.iter().any(|(name, value)| {
            name == "hook_pane" && value == &format!("%{}", removed_pane_id.as_u32())
        }),
        "pane-focus-out must preserve historical hook_pane: {:?}",
        focus_out.formats
    );
    assert_buffer(&handler, "removed-log", "logged").await;
    assert!(
        focus_out
            .formats
            .iter()
            .any(|(name, value)| { name == "pane_id" && value == &removed_pane_id.to_string() }),
        "pane-focus-out must preserve historical pane_id: {:?}",
        focus_out.formats
    );

    let _ = shutdown_tx.send(());
    lifecycle_task.await.expect("lifecycle task joins");
}

#[tokio::test]
async fn removed_focus_out_generation_cannot_send_keys_to_respawned_runtime() {
    assert_removed_generation_input(false).await;
}

#[tokio::test]
async fn removed_focus_out_generation_cannot_send_keys_to_focus_enabled_respawned_runtime() {
    assert_removed_generation_input(true).await;
}

async fn assert_removed_generation_input(seed_replacement_focus: bool) {
    let handler = RequestHandler::new();
    let alpha = session_name("focus-removed-generation-identity");
    let requester_pid = std::process::id();
    create_send_keys_test_session(&handler, &alpha).await;
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    enable_focus_mode(&handler, &target).await;
    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&target);
    }
    trace_focus_capture(&handler, &target, "before-attach").await;
    let _control_rx = register_focused_attach(&handler, requester_pid, &alpha).await;
    trace_focus_capture(&handler, &target, "after-attach").await;
    let (pane_id, old_generation) = {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .expect("pane exists before respawn")
            .id();
        (
            pane_id,
            state.pane_output_generation_for_target(&target, pane_id),
        )
    };
    set_global_focus_hook(
        &handler,
        HookName::PaneFocusOut,
        "set-buffer -b generation-log logged",
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
    // Keep the attach bytes as well: no capture is cleared at the respawn
    // boundary. Attribution distinguishes the old attach from the new runtime.
    let respawn_started = std::time::SystemTime::now();
    trace_focus_capture(&handler, &target, "before-respawn").await;
    let request = rmux_proto::RespawnPaneRequest {
        target: target.clone(),
        kill: true,
        start_directory: None,
        environment: None,
        command: Some(quiet_pane_command()),
        process_command: None,
    };
    let response = if seed_replacement_focus {
        // Exercise the same respawn/focus publication boundary with DEC 1004
        // explicitly enabled in the NEW transcript. This portable witness
        // models startup output arriving before reconciliation; it is not a
        // claim that native ConPTY has been exercised on this host.
        let effects = {
            let mut state = handler.state.lock().await;
            state
                .respawn_pane(
                    request,
                    &handler.socket_path(),
                    None,
                    Some(handler.pane_alert_callback()),
                    Some(handler.pane_exit_callback()),
                    |_, _| {},
                )
                .expect("controlled respawn succeeds");
            handler.record_pane_respawn_boundary(pane_id);
            state.retire_respawned_lifecycle_panes(&[pane_id]);
            state
                .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1004h")
                .expect("new generation enables focus reports");
            let mut active = handler.active_attach.lock().await;
            crate::handler::focus_support::prepare_server_focus_effects(&mut state, &mut active)
        };
        effects
            .execute(&handler)
            .await
            .expect("controlled focus effects complete");
        handler.refresh_attached_session(&alpha).await;
        Response::RespawnPane(rmux_proto::RespawnPaneResponse {
            target: target.clone(),
        })
    } else {
        handler
            .handle(Request::RespawnPane(Box::new(request)))
            .await
    };
    trace_focus_capture(&handler, &target, "after-respawn").await;
    assert!(matches!(response, Response::RespawnPane(_)), "{response:?}");
    {
        let state = handler.state.lock().await;
        let new_generation = state.pane_output_generation_for_target(&target, pane_id);
        assert!(
            new_generation > old_generation,
            "respawn creates a distinct output generation"
        );
        let focus = state.pane_focus_input_capture_for_test(&target);
        let (attach, replacement) = focus.split_first().expect("initial attach emits focus-in");
        assert_eq!(attach.pane_id, pane_id);
        assert_eq!(attach.generation, old_generation);
        assert_eq!(attach.bytes, b"\x1b[I");
        assert!(attach.at <= respawn_started);
        assert!(
            replacement.len() <= 1,
            "one new-generation focus-in edge: {focus:?}"
        );
        if seed_replacement_focus {
            assert_eq!(
                replacement.len(),
                1,
                "focus-enabled replacement gets its report"
            );
        }
        for report in replacement {
            assert_eq!(report.pane_id, pane_id);
            assert_eq!(report.generation, new_generation);
            assert_eq!(report.bytes, b"\x1b[I");
            assert!(report.at >= respawn_started);
        }
        // The old hook's empty-write assertion excludes only attributed,
        // generation-checked reports, never byte patterns or arbitrary prefixes.
        assert_eq!(
            state.pane_non_focus_input_capture_for_test(&target),
            Some(Vec::new()),
            "the old generation's PO hook cannot write to the respawned runtime"
        );
    }
    let focus_events = std::iter::from_fn(|| lifecycle.try_recv().ok())
        .filter(|event| {
            matches!(
                event.event,
                LifecycleEvent::PaneFocusIn { .. } | LifecycleEvent::PaneFocusOut { .. }
            )
        })
        .collect::<Vec<_>>();
    assert!(
        matches!(
            focus_events.as_slice(),
            [
                QueuedLifecycleEvent {
                    event: LifecycleEvent::PaneFocusOut { .. },
                    ..
                },
                QueuedLifecycleEvent {
                    event: LifecycleEvent::PaneFocusIn { .. },
                    ..
                }
            ]
        ),
        "{focus_events:?}"
    );
    assert!(focus_events[0]
        .formats
        .iter()
        .any(|(name, value)| { name == "pane_id" && value == &pane_id.to_string() }));
    assert_buffer(&handler, "generation-log", "logged").await;

    // Identical bytes from an ordinary writer must NOT be excused as focus.
    // This also proves the empty check above would retain a hook's send-keys.
    let sent = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(target.clone()),
            keys: vec!["\x1b[I".to_owned()],
            literal: true,
            expand_formats: false,
            hex: false,
            dispatch_key_table: false,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await;
    assert!(matches!(sent, Response::SendKeys(_)), "{sent:?}");
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_non_focus_input_capture_for_test(&target),
            Some(b"\x1b[I".to_vec())
        );
    }

    let _ = shutdown_tx.send(());
    lifecycle_task.await.expect("lifecycle task joins");
}
