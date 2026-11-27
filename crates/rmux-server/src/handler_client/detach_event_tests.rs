use super::*;

#[tokio::test]
async fn l06_detach_exec_event_order_is_unique() {
    // tmux 3.7b measured 2026-09-30: explicit detach (including -E)
    // publishes one layout before one departure; transport loss reverses them.
    assert_attach_exec_order(false).await;
    for cause in ["session", "target", "eof"] {
        assert_control_detach_order(cause).await;
    }
}

#[tokio::test]
async fn closing_refresh_cannot_publish_a_second_detach() {
    assert_attach_exec_order(true).await;
}

async fn assert_attach_exec_order(refresh_while_closing: bool) {
    let handler = RequestHandler::new();
    let session = session_name("detach-client-notify");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    set_window_size_policy(&handler, &session, "largest").await;

    let detaching_pid = std::process::id();
    let surviving_pid = detaching_pid.saturating_add(1);
    let detaching_size = TerminalSize {
        cols: 101,
        rows: 41,
    };
    let surviving_size = TerminalSize { cols: 60, rows: 20 };
    let (attach_id, _attach_events) =
        register_attached_client(&handler, detaching_pid, &session, detaching_size).await;
    let (_control_id, mut control_events) =
        register_control_client_with_id(&handler, surviving_pid, &session).await;
    let response = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(surviving_pid, surviving_size),
        )))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(
        session_size(&handler, &session).await,
        attached_content_size(detaching_size)
    );
    let layout_prefix = format!(
        "%layout-change @{} ",
        active_window_id(&handler, &session).await
    );
    settle_control_notifications(&mut control_events).await;

    // `detach-client` sends the terminal control message and then reconciles;
    // the client is only unregistered when its connection finishes, so the
    // reconcile has to see the departure itself.
    let response = handler
        .handle(Request::DetachClientExt(
            rmux_proto::DetachClientExtRequest {
                target_client: None,
                all_other_clients: false,
                target_session: None,
                kill_on_detach: false,
                exec_command: Some("exit 17".to_owned()),
            },
        ))
        .await;
    assert!(
        matches!(response, Response::DetachClient(_)),
        "{response:?}"
    );

    assert_eq!(
        session_size(&handler, &session).await,
        surviving_size,
        "the session must fall back to its surviving control client's geometry"
    );
    if refresh_while_closing {
        // Force the recorded interleaving: detach has latched closing and
        // published its departure, but the transport has not finished yet.
        handler
            .refresh_attached_client(detaching_pid, &session)
            .await;
    }
    handler.finish_attach(detaching_pid, attach_id).await;
    handler.finish_attach(detaching_pid, attach_id).await;
    let mut lines =
        collect_control_notifications_through(&mut control_events, "%client-detached ").await;
    lines.extend(settle_control_notifications(&mut control_events).await);
    let ordered: Vec<_> = lines
        .iter()
        .filter_map(|line| {
            if line.starts_with(&layout_prefix) {
                Some("layout")
            } else if line.starts_with("%client-detached ") {
                Some("detached")
            } else {
                None
            }
        })
        .collect();
    assert_eq!(ordered, ["layout", "detached"], "{lines:?}");
    let layout_index = lines
        .iter()
        .position(|line| line.starts_with(&layout_prefix))
        .expect("the surviving control client must be told the window shrank");
    assert_eq!(
        layout_change_geometry(&lines[layout_index]).as_deref(),
        Some("60x20"),
        "{:?}",
        lines[layout_index]
    );
}

async fn assert_control_detach_order(cause: &str) {
    let handler = RequestHandler::new();
    let source = session_name("l06-control-source");
    let peer = session_name("l06-control-peer");
    create_session(&handler, source.clone(), INITIAL_SIZE).await;
    create_session(&handler, peer.clone(), INITIAL_SIZE).await;
    let response = handler
        .handle(Request::LinkWindow(rmux_proto::LinkWindowRequest {
            source: WindowTarget::with_window(source.clone(), 0),
            target: WindowTarget::with_window(peer.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: false,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    set_window_size_policy(&handler, &source, "largest").await;
    let survivor_pid = 92600;
    let departing_pid = survivor_pid + 1;
    let (_survivor_id, mut events) =
        register_control_client_with_id(&handler, survivor_pid, &peer).await;
    let (departing_id, _departing_events) =
        register_control_client_with_id(&handler, departing_pid, &source).await;
    for (pid, size) in [
        (survivor_pid, TerminalSize { cols: 60, rows: 20 }),
        (
            departing_pid,
            TerminalSize {
                cols: 101,
                rows: 41,
            },
        ),
    ] {
        let response = handler
            .handle(Request::RefreshClient(Box::new(
                refresh_client_size_request(pid, size),
            )))
            .await;
        assert!(
            matches!(response, Response::RefreshClient(_)),
            "{response:?}"
        );
    }
    assert_eq!(
        session_size(&handler, &peer).await,
        TerminalSize {
            cols: 101,
            rows: 41
        }
    );
    settle_control_notifications(&mut events).await;
    if cause != "eof" {
        let response = handler
            .handle(Request::DetachClientExt(
                rmux_proto::DetachClientExtRequest {
                    target_client: (cause == "target").then(|| departing_pid.to_string()),
                    target_session: (cause == "session").then(|| source.clone()),
                    all_other_clients: false,
                    kill_on_detach: false,
                    exec_command: None,
                },
            ))
            .await;
        assert!(
            matches!(response, Response::DetachClient(_)),
            "{cause}: {response:?}"
        );
    }
    handler.finish_control(departing_pid, departing_id).await;
    handler.finish_control(departing_pid, departing_id).await;
    assert_eq!(
        session_size(&handler, &peer).await,
        TerminalSize { cols: 60, rows: 20 },
        "{cause}"
    );
    let mut lines = collect_control_notifications_through(&mut events, "%client-detached ").await;
    lines.extend(settle_control_notifications(&mut events).await);
    let order: Vec<_> = lines
        .iter()
        .filter_map(|line| {
            if line.starts_with("%layout-change ") {
                Some("layout")
            } else if line.starts_with("%client-detached ") {
                Some("detached")
            } else {
                None
            }
        })
        .collect();
    let expected = if cause == "eof" {
        ["detached", "layout"]
    } else {
        ["layout", "detached"]
    };
    assert_eq!(order, expected, "{cause}: {lines:?}");
    assert_eq!(handler.control_session_name(survivor_pid).await, Some(peer));
}

#[tokio::test]
async fn closing_work_keeps_one_departure_owner() {
    for (work, detach) in [
        ("client", "explicit"),
        ("session", "session"),
        ("stream", "target"),
        ("input", "others"),
        ("captured-client", "exec"),
        ("captured-session", "identity"),
        ("base", "explicit"),
        ("cleanup", "target"),
    ] {
        let handler = Arc::new(RequestHandler::new());
        let session = session_name(&format!("closing-work-{work}"));
        create_session(&handler, session.clone(), INITIAL_SIZE).await;
        let pid = std::process::id();
        let (attach_id, _controls) =
            register_attached_client(&handler, pid, &session, INITIAL_SIZE).await;
        let identity = handler.active_attach_identity(pid).await.unwrap();
        if work == "stream" {
            handler
                .active_attach
                .lock()
                .await
                .by_pid
                .get_mut(&pid)
                .unwrap()
                .render_stream = true;
        }
        let mut events = handler.subscribe_lifecycle_events();
        let captured = if work.starts_with("captured-") {
            let pause = crate::handler::attach_support::install_generic_render_delivery_pause(pid);
            let task_handler = Arc::clone(&handler);
            let task_session = session.clone();
            let task = tokio::spawn(async move {
                if work == "captured-client" {
                    task_handler
                        .refresh_attached_client(pid, &task_session)
                        .await;
                } else {
                    task_handler.refresh_attached_session(&task_session).await;
                }
            });
            tokio::time::timeout(Duration::from_secs(5), pause.reached.notified())
                .await
                .expect("refresh reached the delivery boundary");
            Some((pause, task))
        } else {
            None
        };
        let keep_pid = pid.saturating_add(1);
        let _survivor = if detach == "others" {
            Some(register_attached_client(&handler, keep_pid, &session, INITIAL_SIZE).await)
        } else {
            None
        };
        let response = if detach == "identity" {
            handler.handle_detach_client_for_identity(identity).await
        } else {
            handler
                .handle_detach_client_ext(
                    pid,
                    rmux_proto::DetachClientExtRequest {
                        target_client: match detach {
                            "target" => Some(pid.to_string()),
                            "others" => Some(keep_pid.to_string()),
                            _ => None,
                        },
                        all_other_clients: detach == "others",
                        target_session: (detach == "session").then(|| session.clone()),
                        kill_on_detach: false,
                        exec_command: (detach == "exec").then(|| "exit 17".to_owned()),
                    },
                )
                .await
        };
        assert!(
            matches!(response, Response::DetachClient(_)),
            "{work}: {response:?}"
        );
        if let Some((pause, task)) = captured {
            pause.release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap();
        } else {
            match work {
                "client" => handler.refresh_attached_client(pid, &session).await,
                "session" | "stream" => handler.refresh_attached_session(&session).await,
                "input" => {
                    handler
                        .mark_attached_session_interactive_input(&session)
                        .await
                }
                "base" => {
                    handler
                        .refresh_attached_client_base_only(pid, &session)
                        .await
                }
                "cleanup" => {
                    assert_eq!(
                        handler
                            .remove_attached_clients_for_session(&session, vec![identity])
                            .await,
                        [pid]
                    );
                }
                _ => unreachable!(),
            }
        }
        if work != "cleanup" {
            assert!(
                handler.active_attach.lock().await.by_pid.contains_key(&pid),
                "{work}: closing work must leave cleanup to finish_attach"
            );
        }
        handler.finish_attach(pid, attach_id).await;
        handler.finish_attach(pid, attach_id).await;
        let departures = std::iter::from_fn(|| events.try_recv().ok()).filter(|event| {
            matches!(&event.event, rmux_core::LifecycleEvent::ClientDetached { session_name, .. } if session_name == &session)
        }).count();
        assert_eq!(
            departures, 1,
            "{work}/{detach}: departure must have one owner"
        );
    }
}

#[tokio::test]
async fn failed_delivery_and_deferred_finish_keep_their_departure() {
    for cause in ["closed", "preclosed", "full", "deferred"] {
        let handler = RequestHandler::new();
        let session = session_name(&format!("failed-refresh-{cause}"));
        create_session(&handler, session.clone(), INITIAL_SIZE).await;
        let pid = std::process::id();
        let (attach_id, controls) =
            register_attached_client(&handler, pid, &session, INITIAL_SIZE).await;
        let identity = handler.active_attach_identity(pid).await.unwrap();
        let mut events = handler.subscribe_lifecycle_events();
        match cause {
            "closed" => {
                drop(controls);
                handler.refresh_attached_client(pid, &session).await;
            }
            "preclosed" => {
                let sender = handler
                    .active_attach
                    .lock()
                    .await
                    .by_pid
                    .get(&pid)
                    .unwrap()
                    .control_tx
                    .clone();
                drop(controls);
                assert!(sender
                    .send(crate::pane_io::AttachControl::InteractiveInput)
                    .is_err());
                handler
                    .prune_stale_attached_clients_for_session(&session)
                    .await;
            }
            "full" => {
                handler
                    .active_attach
                    .lock()
                    .await
                    .by_pid
                    .get(&pid)
                    .unwrap()
                    .control_backlog
                    .store(
                        crate::handler::attach_support::ATTACH_CONTROL_BACKLOG_LIMIT,
                        Ordering::SeqCst,
                    );
                handler
                    .mark_attached_session_interactive_input(&session)
                    .await;
            }
            "deferred" => {
                let mut attached = handler.active_attach.lock().await;
                let active = attached.by_pid.get_mut(&pid).unwrap();
                active.closing.store(true, Ordering::SeqCst);
                active.emit_detached_on_finish = true;
                drop(attached);
                handler
                    .remove_attached_clients_for_session(&session, vec![identity])
                    .await;
            }
            _ => unreachable!(),
        }
        handler.finish_attach(pid, attach_id).await;
        handler.finish_attach(pid, attach_id).await;
        let departures = std::iter::from_fn(|| events.try_recv().ok()).filter(|event| {
            matches!(&event.event, rmux_core::LifecycleEvent::ClientDetached { session_name, .. } if session_name == &session)
        }).count();
        assert_eq!(
            departures, 1,
            "{cause}: actual delivery loss must still publish departure"
        );
    }
}
