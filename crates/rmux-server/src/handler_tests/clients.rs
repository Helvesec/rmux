use super::*;
use crate::control::{ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};
use rmux_core::LifecycleEvent;

#[tokio::test]
async fn attached_client_flags_keep_tmux_order_for_extended_flag_sets() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            requester_pid,
            alpha,
            control_tx,
            crate::outer_terminal::OuterTerminalContext::default().with_client_terminal(
                &rmux_proto::ClientTerminalContext {
                    terminal_features: Vec::new(),
                    utf8: true,
                },
            ),
        )
        .await;

    {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attached client exists");
        active.flags.insert(super::super::ClientFlags::IGNORESIZE);
        active
            .flags
            .insert(super::super::ClientFlags::NO_DETACH_ON_DESTROY);
        active.flags.insert(super::super::ClientFlags::READONLY);
        active.flags.insert(super::super::ClientFlags::ACTIVEPANE);
        active.suspended = true;
    }

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client exists");
    assert_eq!(
        super::super::format_attached_client_flags(active),
        "attached,ignore-size,no-detach-on-destroy,read-only,active-pane,suspended,UTF-8"
    );
}

#[tokio::test]
async fn control_client_flags_keep_tmux_order_for_extended_flag_sets() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (event_tx, _event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let _control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default()
                    .with_client_terminal(&rmux_proto::ClientTerminalContext {
                        terminal_features: Vec::new(),
                        utf8: true,
                    }),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(requester_pid, Some(alpha))
        .await
        .expect("set control session");

    {
        let mut active_control = handler.active_control.lock().await;
        let active = active_control
            .by_pid
            .get_mut(&requester_pid)
            .expect("control client exists");
        active.flags.no_output = true;
        active.flags.wait_exit = true;
        active.flags.pause_after_millis = Some(3_000);
    }

    let active_control = handler.active_control.lock().await;
    let active = active_control
        .by_pid
        .get(&requester_pid)
        .expect("control client exists");
    assert_eq!(
        super::super::format_control_client_flags(active),
        "attached,focused,control-mode,no-output,wait-exit,pause-after=3,UTF-8"
    );
}

#[tokio::test]
async fn refresh_client_control_size_resizes_real_control_session() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (event_tx, mut event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let _control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(requester_pid, Some(alpha.clone()))
        .await
        .expect("set control session");
    while event_rx.try_recv().is_ok() {}

    let mut lifecycle_events = handler.subscribe_lifecycle_events();
    let response = handler
        .dispatch(
            requester_pid,
            Request::RefreshClient(Box::new(rmux_proto::request::RefreshClientRequest {
                target_client: None,
                adjustment: None,
                clear_pan: false,
                pan_left: false,
                pan_right: false,
                pan_up: false,
                pan_down: false,
                status_only: false,
                clipboard_query: false,
                flags: None,
                flags_alias: None,
                subscriptions: Vec::new(),
                subscriptions_format: Vec::new(),
                control_size: Some("100x30".to_owned()),
                colour_report: None,
            })),
        )
        .await
        .response;
    loop {
        match lifecycle_events.try_recv() {
            Ok(event) => handler.dispatch_lifecycle_hook(event).await,
            Err(
                tokio::sync::broadcast::error::TryRecvError::Empty
                | tokio::sync::broadcast::error::TryRecvError::Closed,
            ) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                panic!("lifecycle events lagged during test: {skipped}");
            }
        }
    }

    assert!(matches!(response, Response::RefreshClient(_)));
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window()
            .size(),
        TerminalSize {
            cols: 100,
            rows: 30
        }
    );
    drop(state);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut saw_layout_change = false;
    while tokio::time::Instant::now() < deadline {
        let Some(event) = tokio::time::timeout(Duration::from_millis(100), event_rx.recv())
            .await
            .ok()
            .flatten()
        else {
            continue;
        };
        if matches!(event, ControlServerEvent::Notification(ref line) if line.starts_with("%layout-change "))
        {
            saw_layout_change = true;
            break;
        }
    }
    assert!(
        saw_layout_change,
        "control client should receive a layout-change notification"
    );
}

#[tokio::test]
async fn control_client_flags_without_session_emit_only_control_mode() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let (event_tx, _event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let _control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default()
                    .with_client_terminal(&rmux_proto::ClientTerminalContext {
                        terminal_features: Vec::new(),
                        utf8: true,
                    }),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;

    let active_control = handler.active_control.lock().await;
    let active = active_control
        .by_pid
        .get(&requester_pid)
        .expect("control client exists");
    assert_eq!(
        super::super::format_control_client_flags(active),
        "control-mode"
    );
}

#[tokio::test]
async fn detach_client_target_session_detaches_control_clients() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    for session_name in [&alpha, &beta] {
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: None,
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::NewSession(_)));
    }

    let mut event_receivers = Vec::new();
    for (pid, session_name) in [(101, &alpha), (102, &alpha), (201, &beta)] {
        let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        let _control_id = handler
            .register_control_with_closing(
                pid,
                ControlModeUpgrade {
                    initial_command_count: 0,
                    mode: ControlMode::Plain,
                    terminal_context: crate::outer_terminal::OuterTerminalContext::default()
                        .with_client_terminal(&rmux_proto::ClientTerminalContext {
                            terminal_features: Vec::new(),
                            utf8: true,
                        }),
                },
                event_tx,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        handler
            .set_control_session(pid, Some(session_name.clone()))
            .await
            .expect("control session set");
        event_receivers.push(event_rx);
    }

    let response = handler
        .handle(Request::DetachClientExt(
            rmux_proto::DetachClientExtRequest {
                target_client: None,
                all_other_clients: false,
                target_session: Some(alpha),
                kill_on_detach: false,
                exec_command: None,
            },
        ))
        .await;
    assert_eq!(
        response,
        Response::DetachClient(rmux_proto::DetachClientResponse)
    );

    let active_control = handler.active_control.lock().await;
    assert!(!active_control.by_pid.contains_key(&101));
    assert!(!active_control.by_pid.contains_key(&102));
    assert!(active_control.by_pid.contains_key(&201));
}

#[tokio::test]
async fn detach_client_target_session_preserves_reregistered_attached_client() {
    let handler = RequestHandler::new();
    let alpha = session_name("detach-target-generation-alpha");
    let beta = session_name("detach-target-generation-beta");

    for session_name in [&alpha, &beta] {
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: None,
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    }

    let attach_pid = 91_347;
    let (old_tx, _old_rx) = mpsc::unbounded_channel();
    let old_id = handler
        .register_attach(attach_pid, alpha.clone(), old_tx)
        .await;
    let pause = super::super::attach_support::install_attach_control_identity_pause(attach_pid);

    let detach_handler = handler.clone();
    let detach_alpha = alpha.clone();
    let detach = tokio::spawn(async move {
        detach_handler
            .handle(Request::DetachClientExt(
                rmux_proto::DetachClientExtRequest {
                    target_client: None,
                    all_other_clients: false,
                    target_session: Some(detach_alpha),
                    kill_on_detach: false,
                    exec_command: None,
                },
            ))
            .await
    });

    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("detach reaches its final client identity check");
    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    let replacement_id = handler
        .register_attach(attach_pid, beta.clone(), replacement_tx)
        .await;
    assert_ne!(replacement_id, old_id);
    pause.release.notify_one();

    assert_eq!(
        detach.await.expect("detach task joins"),
        Response::DetachClient(rmux_proto::DetachClientResponse)
    );
    assert!(matches!(
        replacement_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    let active_attach = handler.active_attach.lock().await;
    let replacement = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("replacement attach survives");
    assert_eq!(replacement.id, replacement_id);
    assert_eq!(replacement.session_name, beta);
    assert!(!replacement
        .closing
        .load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn detach_client_target_session_preserves_control_registered_after_snapshot() {
    let handler = RequestHandler::new();
    let alpha = session_name("detach-target-late-control-alpha");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");

    let attach_pid = 91_349;
    let (attach_tx, _attach_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(attach_pid, alpha.clone(), attach_tx)
        .await;
    let pause = super::super::attach_support::install_attach_control_identity_pause(attach_pid);
    let detach_handler = handler.clone();
    let detach_alpha = alpha.clone();
    let detach = tokio::spawn(async move {
        detach_handler
            .handle(Request::DetachClientExt(
                rmux_proto::DetachClientExtRequest {
                    target_client: None,
                    all_other_clients: false,
                    target_session: Some(detach_alpha),
                    kill_on_detach: false,
                    exec_command: None,
                },
            ))
            .await
    });

    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("detach reaches the attach identity check");
    let control_pid = 91_350;
    let (control_id, mut control_rx) =
        register_control_test_client(&handler, control_pid, &alpha).await;
    pause.release.notify_one();

    assert_eq!(
        detach.await.expect("detach task joins"),
        Response::DetachClient(rmux_proto::DetachClientResponse)
    );
    assert!(
        std::iter::from_fn(|| control_rx.try_recv().ok())
            .all(|event| !matches!(event, ControlServerEvent::Exit(_))),
        "control registered after the snapshot must not receive Exit"
    );
    let active_control = handler.active_control.lock().await;
    let control = active_control
        .by_pid
        .get(&control_pid)
        .expect("control registered after the snapshot survives");
    assert_eq!(control.id, control_id);
    assert_eq!(control.session_name.as_ref(), Some(&alpha));
    assert!(!control.closing.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn detach_client_target_session_preserves_control_on_recreated_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("detach-target-recreated-control-alpha");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    let old_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("old session exists")
        .id();

    let attach_pid = 91_351;
    let (attach_tx, _attach_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(attach_pid, alpha.clone(), attach_tx)
        .await;
    let old_control_pid = 91_352;
    let (_old_control_id, _old_control_rx) =
        register_control_test_client(&handler, old_control_pid, &alpha).await;
    let pause = super::super::attach_support::install_attach_control_identity_pause(attach_pid);
    let detach_handler = handler.clone();
    let detach_alpha = alpha.clone();
    let detach = tokio::spawn(async move {
        detach_handler
            .handle(Request::DetachClientExt(
                rmux_proto::DetachClientExtRequest {
                    target_client: None,
                    all_other_clients: false,
                    target_session: Some(detach_alpha),
                    kill_on_detach: false,
                    exec_command: None,
                },
            ))
            .await
    });

    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("detach reaches the attach identity check");
    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    let recreated = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(
        matches!(recreated, Response::NewSession(_)),
        "{recreated:?}"
    );
    let new_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("recreated session exists")
        .id();
    assert_ne!(new_session_id, old_session_id);
    let new_control_pid = 91_353;
    let (new_control_id, mut new_control_rx) =
        register_control_test_client(&handler, new_control_pid, &alpha).await;
    pause.release.notify_one();

    assert_eq!(
        detach.await.expect("detach task joins"),
        Response::DetachClient(rmux_proto::DetachClientResponse)
    );
    assert!(
        std::iter::from_fn(|| new_control_rx.try_recv().ok())
            .all(|event| !matches!(event, ControlServerEvent::Exit(_))),
        "control on the recreated session must not receive Exit"
    );
    let active_control = handler.active_control.lock().await;
    let new_control = active_control
        .by_pid
        .get(&new_control_pid)
        .expect("control on the recreated session survives");
    assert_eq!(new_control.id, new_control_id);
    assert_eq!(new_control.session_id, Some(new_session_id));
    assert!(!new_control
        .closing
        .load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn detach_client_target_session_tracks_a_renamed_session_identity() {
    let handler = RequestHandler::new();
    let alpha = session_name("detach-target-rename-alpha");
    let beta = session_name("detach-target-rename-beta");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    let session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("session exists")
        .id();

    let attach_pid = 91_354;
    let (attach_tx, _attach_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(attach_pid, alpha.clone(), attach_tx)
        .await;
    let control_pid = 91_355;
    let (control_id, mut control_rx) =
        register_control_test_client(&handler, control_pid, &alpha).await;
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let pause = super::super::attach_support::install_attach_control_identity_pause(attach_pid);
    let detach_handler = handler.clone();
    let detach_alpha = alpha.clone();
    let detach = tokio::spawn(async move {
        detach_handler
            .handle(Request::DetachClientExt(
                rmux_proto::DetachClientExtRequest {
                    target_client: None,
                    all_other_clients: false,
                    target_session: Some(detach_alpha),
                    kill_on_detach: false,
                    exec_command: None,
                },
            ))
            .await
    });

    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("detach reaches the attach identity check");
    let renamed = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: alpha,
            new_name: beta.clone(),
        }))
        .await;
    assert!(matches!(renamed, Response::RenameSession(_)), "{renamed:?}");
    pause.release.notify_one();

    assert_eq!(
        detach.await.expect("detach task joins"),
        Response::DetachClient(rmux_proto::DetachClientResponse)
    );
    assert!(std::iter::from_fn(|| control_rx.try_recv().ok())
        .any(|event| matches!(event, ControlServerEvent::Exit(None))));
    let active_control = handler.active_control.lock().await;
    assert!(!active_control.by_pid.contains_key(&control_pid));
    drop(active_control);
    let detached = std::iter::from_fn(|| lifecycle.try_recv().ok())
        .find(|event| {
            matches!(
                &event.event,
                LifecycleEvent::ClientDetached {
                    session_name,
                    client_name: Some(client_name),
                } if session_name == &beta && client_name == &control_pid.to_string()
            )
        })
        .expect("renamed control publishes client-detached");
    assert_eq!(detached.control_session_identity, Some(session_id));
    handler.finish_control(control_pid, control_id).await;
}

#[tokio::test]
async fn managed_client_actions_fail_closed_when_a_pid_is_reregistered() {
    let handler = RequestHandler::new();
    let alpha = session_name("managed-client-generation-alpha");
    let beta = session_name("managed-client-generation-beta");
    for session_name in [&alpha, &beta] {
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: None,
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    }

    let control_pid = 91_348;
    let (old_control_id, _old_control_rx) =
        register_control_test_client(&handler, control_pid, &alpha).await;
    let pause = super::super::client_support::install_managed_client_resolution_pause(control_pid);
    let detach_handler = handler.clone();
    let detach = tokio::spawn(async move {
        detach_handler
            .handle(Request::DetachClientExt(
                rmux_proto::DetachClientExtRequest {
                    target_client: Some(control_pid.to_string()),
                    all_other_clients: false,
                    target_session: None,
                    kill_on_detach: false,
                    exec_command: None,
                },
            ))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("detach resolves the original control identity");
    let (replacement_control_id, mut replacement_control_rx) =
        register_control_test_client(&handler, control_pid, &beta).await;
    assert_ne!(replacement_control_id, old_control_id);
    while replacement_control_rx.try_recv().is_ok() {}
    pause.release.notify_one();
    assert!(matches!(
        detach.await.expect("detach task joins"),
        Response::Error(_)
    ));
    assert!(matches!(
        replacement_control_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    let pause = super::super::client_support::install_managed_client_resolution_pause(control_pid);
    let refresh_handler = handler.clone();
    let refresh = tokio::spawn(async move {
        refresh_handler
            .handle(Request::RefreshClient(Box::new(refresh_client_request(
                control_pid,
            ))))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("refresh resolves the original control identity");
    let (third_control_id, mut third_control_rx) =
        register_control_test_client(&handler, control_pid, &alpha).await;
    assert_ne!(third_control_id, replacement_control_id);
    while third_control_rx.try_recv().is_ok() {}
    pause.release.notify_one();
    assert!(matches!(
        refresh.await.expect("refresh task joins"),
        Response::Error(_)
    ));
    assert!(matches!(
        third_control_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    let attach_pid = 91_349;
    let (old_attach_tx, _old_attach_rx) = mpsc::unbounded_channel();
    let old_attach_id = handler
        .register_attach(attach_pid, alpha.clone(), old_attach_tx)
        .await;
    let pause = super::super::client_support::install_managed_client_resolution_pause(attach_pid);
    let detach_handler = handler.clone();
    let detach = tokio::spawn(async move {
        detach_handler
            .handle(Request::DetachClientExt(
                rmux_proto::DetachClientExtRequest {
                    target_client: Some(attach_pid.to_string()),
                    all_other_clients: false,
                    target_session: None,
                    kill_on_detach: false,
                    exec_command: None,
                },
            ))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("detach resolves the original attach identity");
    let (replacement_attach_tx, mut replacement_attach_rx) = mpsc::unbounded_channel();
    let replacement_attach_id = handler
        .register_attach(attach_pid, beta.clone(), replacement_attach_tx)
        .await;
    assert_ne!(replacement_attach_id, old_attach_id);
    while replacement_attach_rx.try_recv().is_ok() {}
    pause.release.notify_one();
    assert!(matches!(
        detach.await.expect("detach task joins"),
        Response::Error(_)
    ));
    assert!(matches!(
        replacement_attach_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    let pause = super::super::client_support::install_managed_client_resolution_pause(attach_pid);
    let refresh_handler = handler.clone();
    let refresh = tokio::spawn(async move {
        refresh_handler
            .handle(Request::RefreshClient(Box::new(refresh_client_request(
                attach_pid,
            ))))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("refresh resolves the original attach identity");
    let (third_attach_tx, mut third_attach_rx) = mpsc::unbounded_channel();
    let third_attach_id = handler
        .register_attach(attach_pid, alpha.clone(), third_attach_tx)
        .await;
    assert_ne!(third_attach_id, replacement_attach_id);
    while third_attach_rx.try_recv().is_ok() {}
    pause.release.notify_one();
    assert!(matches!(
        refresh.await.expect("refresh task joins"),
        Response::Error(_)
    ));
    assert!(matches!(
        third_attach_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    let pause = super::super::client_support::install_managed_client_resolution_pause(attach_pid);
    let show_handler = handler.clone();
    let show = tokio::spawn(async move {
        show_handler
            .handle(Request::ShowMessages(rmux_proto::ShowMessagesRequest {
                jobs: false,
                terminals: true,
                target_client: Some(attach_pid.to_string()),
            }))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("show-messages resolves the original attach identity");
    let (fourth_attach_tx, _fourth_attach_rx) = mpsc::unbounded_channel();
    let fourth_attach_id = handler
        .register_attach(attach_pid, beta.clone(), fourth_attach_tx)
        .await;
    assert_ne!(fourth_attach_id, third_attach_id);
    pause.release.notify_one();
    let Response::ShowMessages(response) = show.await.expect("show-messages task joins") else {
        panic!("show-messages should fail closed with an empty listing");
    };
    assert!(
        response.output.stdout().is_empty(),
        "show-messages must not expose the replacement attached client"
    );

    let active_control = handler.active_control.lock().await;
    assert_eq!(
        active_control
            .by_pid
            .get(&control_pid)
            .expect("latest control registration survives")
            .id,
        third_control_id
    );
    drop(active_control);
    let active_attach = handler.active_attach.lock().await;
    assert_eq!(
        active_attach
            .by_pid
            .get(&attach_pid)
            .expect("latest attach registration survives")
            .id,
        fourth_attach_id
    );
}

async fn register_control_test_client(
    handler: &RequestHandler,
    control_pid: u32,
    session_name: &SessionName,
) -> (u64, mpsc::Receiver<ControlServerEvent>) {
    let (event_tx, mut event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let control_id = handler
        .register_control_with_closing(
            control_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(control_pid, Some(session_name.clone()))
        .await
        .expect("set control session");
    while event_rx.try_recv().is_ok() {}
    (control_id, event_rx)
}

fn refresh_client_request(target_pid: u32) -> rmux_proto::request::RefreshClientRequest {
    rmux_proto::request::RefreshClientRequest {
        target_client: Some(target_pid.to_string()),
        adjustment: None,
        clear_pan: false,
        pan_left: false,
        pan_right: false,
        pan_up: false,
        pan_down: false,
        status_only: false,
        clipboard_query: false,
        flags: None,
        flags_alias: None,
        subscriptions: Vec::new(),
        subscriptions_format: Vec::new(),
        control_size: None,
        colour_report: None,
    }
}

#[tokio::test]
async fn control_mode_attach_session_tracks_the_control_clients_session() {
    let handler = RequestHandler::new();
    let requester_pid = 301;
    let alpha = session_name("alpha");

    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)));

    let (event_tx, _event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let _control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default()
                    .with_client_terminal(&rmux_proto::ClientTerminalContext {
                        terminal_features: Vec::new(),
                        utf8: true,
                    }),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;

    let commands = parse_command_string("attach-session -t $0").expect("command parses");
    let result = handler
        .execute_control_commands(requester_pid, commands)
        .await;
    assert_eq!(result.error, None);

    let active_control = handler.active_control.lock().await;
    let active = active_control
        .by_pid
        .get(&requester_pid)
        .expect("control client remains registered");
    assert_eq!(active.session_name.as_ref(), Some(&alpha));
}

#[tokio::test]
async fn list_clients_exposes_pid_and_tty_format_variables_for_attached_clients() {
    let handler = RequestHandler::new();
    let socket_path = "/tmp/rmux-list-clients-format.sock";
    handler.set_socket_path(socket_path);
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let config_files = "/tmp/rmux-list-clients.conf";
    handler
        .state
        .lock()
        .await
        .set_startup_config_files(&[config_files.to_owned()]);

    let response = handler
        .handle(Request::ListClients(Box::new(
            rmux_proto::ListClientsRequest {
                format: Some(
                    "#{client_name}|#{client_pid}|#{client_tty}|#{client_session}|#{socket_path}|#{config_files}"
                        .to_owned(),
                ),
                target_session: None,
                filter: Some(format!("#{{==:#{{socket_path}},{socket_path}}}")),
                sort_order: None,
                reversed: false,
            },
        )))
        .await;
    let Response::ListClients(response) = response else {
        panic!("expected list-clients response");
    };
    let output = String::from_utf8(response.output.stdout().to_vec()).expect("utf-8");
    let line = output.lines().next().expect("client line");
    let parts = line.split('|').collect::<Vec<_>>();
    assert_eq!(parts.len(), 6);
    assert_eq!(parts[1], requester_pid.to_string());
    assert_eq!(parts[3], "alpha");
    assert_eq!(parts[4], socket_path);
    assert_eq!(parts[5], config_files);
    #[cfg(unix)]
    assert!(!parts[2].is_empty(), "client_tty should be populated");
    #[cfg(windows)]
    assert_eq!(parts[2], "");
}

#[tokio::test]
async fn list_clients_exposes_effective_attached_key_table_and_prefix_state() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    assert_eq!(
        list_client_prefix_state(&handler).await,
        "0|root\n",
        "idle attached clients should report the root key table"
    );

    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha),
            option: OptionName::KeyTable,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)));
    assert_eq!(
        list_client_prefix_state(&handler).await,
        "0|off\n",
        "idle clients should report the configured effective key table"
    );

    handler
        .set_attached_key_table(
            requester_pid,
            Some("prefix".to_owned()),
            Some(std::time::Instant::now()),
        )
        .await
        .expect("prefix table should be tracked");

    assert_eq!(
        list_client_prefix_state(&handler).await,
        "1|prefix\n",
        "prefix-active attached clients should report the prefix table"
    );

    handler
        .set_attached_key_table(requester_pid, None, None)
        .await
        .expect("clearing the transient key table should succeed");
    assert_eq!(
        list_client_prefix_state(&handler).await,
        "0|off\n",
        "clearing a transient table should reveal the configured table again"
    );
}

async fn list_client_prefix_state(handler: &RequestHandler) -> String {
    let response = handler
        .handle(Request::ListClients(Box::new(
            rmux_proto::ListClientsRequest {
                format: Some("#{client_prefix}|#{client_key_table}".to_owned()),
                target_session: None,
                filter: None,
                sort_order: None,
                reversed: false,
            },
        )))
        .await;
    let Response::ListClients(response) = response else {
        panic!("expected list-clients response");
    };
    String::from_utf8(response.output.stdout().to_vec()).expect("utf-8")
}

#[tokio::test]
async fn attach_session_returns_an_upgrade_response_for_existing_sessions() {
    let handler = RequestHandler::new();
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,

            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    assert_eq!(
        handler
            .handle(Request::AttachSession(rmux_proto::AttachSessionRequest {
                target: session_name("alpha"),
            }))
            .await,
        Response::AttachSession(rmux_proto::AttachSessionResponse {
            session_name: session_name("alpha"),
        })
    );
}

#[tokio::test]
async fn attach_session_dispatch_populates_the_upgrade_field() {
    let handler = RequestHandler::new();
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,

            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let outcome = handler
        .dispatch(
            std::process::id(),
            Request::AttachSession(rmux_proto::AttachSessionRequest {
                target: session_name("alpha"),
            }),
        )
        .await;

    assert!(
        matches!(outcome.response, Response::AttachSession(_)),
        "response should be AttachSession"
    );
    assert!(
        outcome.attach.is_some(),
        "dispatch must populate the attach upgrade field"
    );
}

#[tokio::test]
async fn attach_session_to_missing_session_returns_session_not_found() {
    let handler = RequestHandler::new();

    let outcome = handler
        .dispatch(
            std::process::id(),
            Request::AttachSession(rmux_proto::AttachSessionRequest {
                target: session_name("missing"),
            }),
        )
        .await;

    assert_eq!(
        outcome.response,
        Response::Error(ErrorResponse {
            error: RmuxError::SessionNotFound("missing".to_owned()),
        })
    );
    assert!(
        outcome.attach.is_none(),
        "attach field must be None for missing sessions"
    );
}

#[tokio::test]
async fn switch_client_requires_an_attached_client() {
    let handler = RequestHandler::new();
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,

            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    assert_eq!(
        handler
            .handle(Request::SwitchClient(rmux_proto::SwitchClientRequest {
                target: session_name("alpha"),
            }))
            .await,
        Response::Error(rmux_proto::ErrorResponse {
            error: RmuxError::Message("no current client".to_owned()),
        })
    );
}

#[tokio::test]
async fn detach_client_requires_an_attached_client() {
    let handler = RequestHandler::new();

    assert_eq!(
        handler
            .handle(Request::DetachClient(rmux_proto::DetachClientRequest))
            .await,
        Response::Error(rmux_proto::ErrorResponse {
            error: RmuxError::Server("detach-client requires an attached client".to_owned()),
        })
    );
}
