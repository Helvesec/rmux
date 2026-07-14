use super::*;

#[cfg(unix)]
fn quiet_kill_session_command() -> Vec<String> {
    ["/bin/sh", "-c", "sleep 60"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[cfg(windows)]
fn quiet_kill_session_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
    ]
}

async fn create_quiet_kill_session(handler: &RequestHandler, name: &str) -> SessionName {
    let session = session_name(name);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: None,
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_kill_session_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)));
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::new(session.clone(), 0))
        .await;
    session
}

async fn create_grouped_kill_session(
    handler: &RequestHandler,
    name: &str,
    group_target: &SessionName,
) -> SessionName {
    let session = session_name(name);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: None,
            environment: None,
            group_target: Some(group_target.clone()),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    session
}

#[tokio::test]
async fn kill_session_is_idempotent_for_missing_sessions() {
    let handler = RequestHandler::new();
    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name("missing"),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::SessionNotFound("missing".to_owned()),
        })
    );
}

#[tokio::test]
async fn has_session_resolves_unique_prefix_matches() {
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
            .handle(Request::HasSession(HasSessionRequest {
                target: session_name("alp"),
            }))
            .await,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );
    assert_eq!(
        handler
            .handle(Request::HasSession(HasSessionRequest {
                target: session_name("missing"),
            }))
            .await,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: false })
    );
}

#[tokio::test]
async fn kill_session_all_except_target_preserves_only_the_resolved_target() {
    let handler = RequestHandler::new();
    for name in ["alpha", "beta", "gamma"] {
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name(name),
                detached: true,
                size: None,
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));
    }

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name("bet"),
            kill_all_except_target: true,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    for (target, exists) in [("alpha", false), ("beta", true), ("gamma", false)] {
        assert_eq!(
            handler
                .handle(Request::HasSession(HasSessionRequest {
                    target: session_name(target),
                }))
                .await,
            Response::HasSession(rmux_proto::HasSessionResponse { exists })
        );
    }
}

#[tokio::test]
async fn concurrent_group_owner_kills_rekey_live_subscription_to_final_owner() {
    let handler = RequestHandler::new();
    let owner = create_quiet_kill_session(&handler, "subscription-rekey-a").await;
    let peer = create_grouped_kill_session(&handler, "subscription-rekey-b", &owner).await;
    let survivor = create_grouped_kill_session(&handler, "subscription-rekey-c", &owner).await;
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&owner)
            .and_then(rmux_core::Session::active_pane_id)
            .expect("group owner has an active pane")
    };
    let subscribed = handler
        .handle_subscribe_pane_output_ref(
            4244,
            rmux_proto::SubscribePaneOutputRefRequest {
                target: rmux_proto::PaneTargetRef::by_id(owner.clone(), pane_id),
                start: rmux_proto::PaneOutputSubscriptionStart::Now,
            },
        )
        .await;
    let Response::SubscribePaneOutput(subscribed) = subscribed else {
        panic!("live grouped pane should accept subscription: {subscribed:?}");
    };

    let pause = handler.install_kill_session_subscription_rekey_pause(owner.clone());
    let first_handler = handler.clone();
    let first_owner = owner.clone();
    let first = tokio::spawn(async move {
        first_handler
            .handle(Request::KillSession(KillSessionRequest {
                target: first_owner,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await
    });
    pause.reached.notified().await;

    let second_handler = handler.clone();
    let second_peer = peer.clone();
    let second = tokio::spawn(async move {
        second_handler
            .handle(Request::KillSession(KillSessionRequest {
                target: second_peer,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await
    });
    tokio::task::yield_now().await;
    assert!(
        !second.is_finished(),
        "the next owner transfer must wait until the first subscription rekey commits"
    );

    pause.release.notify_one();
    let first_response = first.await.expect("first kill-session task joins");
    let second_response = second.await.expect("second kill-session task joins");
    assert!(
        matches!(first_response, Response::KillSession(_)),
        "{first_response:?}"
    );
    assert!(
        matches!(second_response, Response::KillSession(_)),
        "{second_response:?}"
    );
    let subscription_key = handler
        .pane_output_subscription_key_for_test(subscribed.subscription_id)
        .expect("live subscription remains registered");
    assert_eq!(subscription_key.runtime_session_name(), &survivor);
}

#[tokio::test]
async fn kill_session_clear_alerts_preserves_the_resolved_session() {
    let handler = RequestHandler::new();
    let alpha = create_quiet_kill_session(&handler, "alpha").await;

    {
        let mut state = handler.state.lock().await;
        let session = state.sessions.session_mut(&alpha).expect("session exists");
        session
            .window_at_mut(0)
            .expect("window exists")
            .queue_alerts(WINDOW_ALERTFLAGS);
        assert!(session.add_winlink_alert_flags(0, WINLINK_ALERTFLAGS));
    }

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name("alp"),
            kill_all_except_target: false,
            clear_alerts: true,
            kill_group: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session survives");
    assert_eq!(
        session.window_at(0).expect("window exists").alert_flags(),
        rmux_core::AlertFlags::empty()
    );
    assert_eq!(
        session.winlink_alert_flags(0),
        rmux_core::AlertFlags::empty()
    );
}

#[tokio::test]
async fn kill_session_last_session_requests_shutdown() {
    let handler = RequestHandler::new();
    let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session_name("alpha"))
            .and_then(|session| session.active_pane_id())
            .expect("new session has an active pane")
    };
    assert_eq!(
        handler.observe_pane_snapshot_revision(pane_id, 1, std::time::Instant::now()),
        Some(1)
    );

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    assert!(
        handler.request_shutdown_if_pending(),
        "last-session kill should queue shutdown after the response is ready"
    );
    assert_eq!(handler.last_emitted_pane_snapshot_revision(pane_id), None);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), shutdown_rx)
            .await
            .expect("last-session kill should request shutdown")
            .is_ok(),
        "shutdown receiver should complete cleanly"
    );
}

#[tokio::test]
async fn exit_empty_shutdown_is_cancelled_when_a_new_session_starts_first() {
    let handler = RequestHandler::new();
    let (shutdown_handle, mut shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );

    let recreated = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("beta"),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(recreated, Response::NewSession(_)));
    assert!(
        !handler.request_shutdown_if_pending(),
        "stale exit-empty shutdown must not stop a newly non-empty server"
    );
    tokio::time::timeout(Duration::from_millis(50), &mut shutdown_rx)
        .await
        .expect_err("stale exit-empty shutdown should be cancelled");
}

#[tokio::test]
async fn exit_empty_shutdown_retries_after_state_lock_contention() {
    let handler = RequestHandler::new();
    let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );

    let state = handler.state.lock().await;
    assert!(
        !handler.request_shutdown_if_pending(),
        "state lock contention should defer exit-empty shutdown"
    );
    drop(state);

    tokio::time::timeout(Duration::from_secs(2), shutdown_rx)
        .await
        .expect("deferred exit-empty shutdown should be retried")
        .expect("shutdown receiver should complete cleanly");
}

#[tokio::test]
async fn exit_empty_shutdown_waits_for_last_session_control_cleanup() {
    let handler = RequestHandler::new();
    let (shutdown_handle, mut shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);
    let alpha = session_name("exit-empty-control-alpha");
    let requester_pid = 42_461;

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
    let session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .expect("session exists")
        .id();
    let (event_tx, mut event_rx) = mpsc::channel(1);
    let closing = Arc::new(AtomicBool::new(false));
    let control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: rmux_proto::ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            event_tx,
            Arc::clone(&closing),
        )
        .await;
    handler
        .set_control_session_identity(requester_pid, alpha.clone(), session_id)
        .await
        .expect("control attaches to the last session");
    assert!(matches!(
        event_rx.try_recv(),
        Ok(crate::control::ControlServerEvent::SessionChanged(Some(ref session_name)))
            if session_name == &alpha
    ));

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    assert!(
        !handler.request_shutdown_if_pending(),
        "bound control cleanup defers rather than cancels exit-empty"
    );
    tokio::time::timeout(Duration::from_millis(20), &mut shutdown_rx)
        .await
        .expect_err("shutdown waits for exact control cleanup");

    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(200), event_rx.recv())
            .await
            .expect("last-session lifecycle closes the control"),
        Some(crate::control::ControlServerEvent::Exit(_))
    ));
    assert!(closing.load(std::sync::atomic::Ordering::SeqCst));
    handler.finish_control(requester_pid, control_id).await;

    tokio::time::timeout(Duration::from_millis(200), shutdown_rx)
        .await
        .expect("finish-control re-evaluates pending exit-empty")
        .expect("shutdown receiver completes cleanly");
}

#[tokio::test]
async fn exit_empty_shutdown_is_cancelled_by_live_unattached_control() {
    let handler = RequestHandler::new();
    let (shutdown_handle, mut shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);
    let alpha = session_name("exit-empty-unattached-control-alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
    let (event_tx, _event_rx) = mpsc::channel(1);
    let control_id = handler
        .register_control_with_closing(
            42_462,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: rmux_proto::ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    assert!(
        !handler.request_shutdown_if_pending(),
        "a live unattached control makes exit-empty stale"
    );
    handler.finish_control(42_462, control_id).await;
    tokio::time::timeout(Duration::from_millis(100), &mut shutdown_rx)
        .await
        .expect_err("a cancelled exit-empty request must not revive on control cleanup");
}

#[tokio::test]
async fn exit_empty_does_not_downgrade_pending_kill_server_shutdown() {
    let handler = RequestHandler::new();
    let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let kill_server = handler
        .handle(Request::KillServer(rmux_proto::KillServerRequest))
        .await;
    assert!(matches!(kill_server, Response::KillServer(_)));

    let kill_session = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert_eq!(
        kill_session,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );

    let recreated = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("beta"),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(recreated, Response::NewSession(_)));
    assert!(
        handler.request_shutdown_if_pending(),
        "explicit kill-server must not become a cancellable exit-empty shutdown"
    );
    tokio::time::timeout(Duration::from_millis(50), shutdown_rx)
        .await
        .expect("kill-server should still request shutdown")
        .expect("shutdown receiver should complete cleanly");
}

#[tokio::test]
async fn kill_session_last_session_respects_exit_empty_off() {
    let handler = RequestHandler::new();
    let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);

    let set_exit_empty = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::ExitEmpty,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_exit_empty, Response::SetOption(_)));

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), shutdown_rx)
            .await
            .is_err(),
        "kill-session should respect exit-empty=off"
    );
}

#[tokio::test]
async fn kill_session_last_session_exits_attached_clients_before_shutdown() {
    let handler = RequestHandler::new();
    let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attached client exists");
        active.last_session = Some(alpha.clone());
    }

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    assert!(matches!(control_rx.try_recv(), Ok(AttachControl::Exited)));
    let active_attach = handler.active_attach.lock().await;
    assert!(
        active_attach.by_pid.is_empty(),
        "attached clients should be gone before shutdown is requested"
    );
    assert!(
        active_attach.active_client_by_window.is_empty(),
        "killed sessions must drop latest-client window state"
    );
    drop(active_attach);
    assert!(
        handler.request_shutdown_if_pending(),
        "last-session kill should queue shutdown after exiting clients"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), shutdown_rx)
            .await
            .expect("last-session kill should request shutdown")
            .is_ok(),
        "shutdown receiver should complete cleanly"
    );
}

#[tokio::test]
async fn kill_session_all_except_target_does_not_request_shutdown_while_target_survives() {
    let handler = RequestHandler::new();
    let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);

    for name in ["alpha", "beta"] {
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name(name),
                detached: true,
                size: None,
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));
    }

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name("beta"),
            kill_all_except_target: true,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), shutdown_rx)
            .await
            .is_err(),
        "kill-session -a should not request shutdown while the target session remains"
    );
}

#[tokio::test]
async fn kill_session_group_selectors_fail_closed_when_target_name_is_recreated() {
    let handler = RequestHandler::new();
    let alpha = create_quiet_kill_session(&handler, "kill-all-identity-alpha").await;
    let beta = create_quiet_kill_session(&handler, "kill-all-identity-beta").await;
    let gamma = create_quiet_kill_session(&handler, "kill-all-identity-gamma").await;
    let beta_attach_pid = 41_001;
    let (beta_control_tx, mut beta_control_rx) = mpsc::unbounded_channel();
    let _beta_attach_id = handler
        .register_attach(beta_attach_pid, beta.clone(), beta_control_tx)
        .await;
    let pause = handler.install_kill_session_selection_identity_pause(alpha.clone());

    let kill_handler = handler.clone();
    let kill_alpha = alpha.clone();
    let kill_all_except = tokio::spawn(async move {
        kill_handler
            .handle(Request::KillSession(KillSessionRequest {
                target: kill_alpha,
                kill_all_except_target: true,
                clear_alerts: false,
                kill_group: false,
            }))
            .await
    });

    pause.reached.notified().await;
    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    let recreated = create_quiet_kill_session(&handler, alpha.as_str()).await;
    pause.release.notify_one();

    let response = kill_all_except.await.expect("kill task joins");
    assert_eq!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::SessionNotFound(alpha.to_string()),
        })
    );
    for session in [recreated, beta, gamma] {
        wait_for_session_state(&handler, session, true).await;
    }
    while let Ok(control) = beta_control_rx.try_recv() {
        assert!(
            !matches!(control, AttachControl::Exited),
            "fail-closed kill-session -a must not exit a surviving victim client"
        );
    }
    assert_eq!(
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&beta_attach_pid)
            .map(|active| active.session_name.clone()),
        Some(session_name("kill-all-identity-beta"))
    );

    let handler = RequestHandler::new();
    let alpha = create_quiet_kill_session(&handler, "kill-group-identity-alpha").await;
    let beta = session_name("kill-group-identity-beta");
    let grouped = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(beta.clone()),
            working_directory: None,
            detached: true,
            size: None,
            environment: None,
            group_target: Some(alpha.clone()),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(grouped, Response::NewSession(_)), "{grouped:?}");
    let keeper = create_quiet_kill_session(&handler, "kill-group-identity-keeper").await;
    let beta_attach_pid = 41_002;
    let (beta_control_tx, mut beta_control_rx) = mpsc::unbounded_channel();
    let _beta_attach_id = handler
        .register_attach(beta_attach_pid, beta.clone(), beta_control_tx)
        .await;
    let pause = handler.install_kill_session_selection_identity_pause(alpha.clone());

    let kill_handler = handler.clone();
    let kill_alpha = alpha.clone();
    let kill_group = tokio::spawn(async move {
        kill_handler
            .handle(Request::KillSession(KillSessionRequest {
                target: kill_alpha,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: true,
            }))
            .await
    });

    pause.reached.notified().await;
    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    let recreated = create_quiet_kill_session(&handler, alpha.as_str()).await;
    pause.release.notify_one();

    let response = kill_group.await.expect("kill task joins");
    assert_eq!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::SessionNotFound(alpha.to_string()),
        })
    );
    for session in [recreated, beta, keeper] {
        wait_for_session_state(&handler, session, true).await;
    }
    while let Ok(control) = beta_control_rx.try_recv() {
        assert!(
            !matches!(control, AttachControl::Exited),
            "fail-closed kill-session -g must not exit a surviving group client"
        );
    }
    assert_eq!(
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&beta_attach_pid)
            .map(|active| active.session_name.clone()),
        Some(session_name("kill-group-identity-beta"))
    );
}

#[tokio::test]
async fn kill_session_group_selectors_follow_a_renamed_victim_identity() {
    let handler = RequestHandler::new();
    let alpha = create_quiet_kill_session(&handler, "kill-all-rename-alpha").await;
    let beta = create_quiet_kill_session(&handler, "kill-all-rename-beta").await;
    let gamma = create_quiet_kill_session(&handler, "kill-all-rename-gamma").await;
    let renamed = session_name("kill-all-rename-delta");
    let beta_attach_pid = 41_003;
    let (beta_control_tx, mut beta_control_rx) = mpsc::unbounded_channel();
    let _beta_attach_id = handler
        .register_attach(beta_attach_pid, beta.clone(), beta_control_tx)
        .await;
    let pause = handler.install_kill_session_selection_identity_pause(alpha.clone());

    let kill_handler = handler.clone();
    let kill_alpha = alpha.clone();
    let kill_all_except = tokio::spawn(async move {
        kill_handler
            .handle(Request::KillSession(KillSessionRequest {
                target: kill_alpha,
                kill_all_except_target: true,
                clear_alerts: false,
                kill_group: false,
            }))
            .await
    });

    pause.reached.notified().await;
    let rename = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: beta,
            new_name: renamed.clone(),
        }))
        .await;
    assert!(matches!(rename, Response::RenameSession(_)), "{rename:?}");
    pause.release.notify_one();

    let response = kill_all_except.await.expect("kill task joins");
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
    wait_for_session_state(&handler, alpha, true).await;
    wait_for_session_state(&handler, renamed, false).await;
    wait_for_session_state(&handler, gamma, false).await;
    let mut beta_exited = false;
    while let Ok(control) = beta_control_rx.try_recv() {
        beta_exited |= matches!(control, AttachControl::Exited);
    }
    assert!(
        beta_exited,
        "kill-session -a must exit the renamed victim client"
    );
    assert!(!handler
        .active_attach
        .lock()
        .await
        .by_pid
        .contains_key(&beta_attach_pid));

    let handler = RequestHandler::new();
    let alpha = create_quiet_kill_session(&handler, "kill-group-rename-alpha").await;
    let beta = session_name("kill-group-rename-beta");
    let grouped = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(beta.clone()),
            working_directory: None,
            detached: true,
            size: None,
            environment: None,
            group_target: Some(alpha.clone()),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(grouped, Response::NewSession(_)), "{grouped:?}");
    let keeper = create_quiet_kill_session(&handler, "kill-group-rename-keeper").await;
    let renamed = session_name("kill-group-rename-delta");
    let beta_attach_pid = 41_004;
    let (beta_control_tx, mut beta_control_rx) = mpsc::unbounded_channel();
    let _beta_attach_id = handler
        .register_attach(beta_attach_pid, beta.clone(), beta_control_tx)
        .await;
    let pause = handler.install_kill_session_selection_identity_pause(alpha.clone());

    let kill_handler = handler.clone();
    let kill_alpha = alpha.clone();
    let kill_group = tokio::spawn(async move {
        kill_handler
            .handle(Request::KillSession(KillSessionRequest {
                target: kill_alpha,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: true,
            }))
            .await
    });

    pause.reached.notified().await;
    let rename = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: beta,
            new_name: renamed.clone(),
        }))
        .await;
    assert!(matches!(rename, Response::RenameSession(_)), "{rename:?}");
    pause.release.notify_one();

    let response = kill_group.await.expect("kill task joins");
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
    wait_for_session_state(&handler, alpha, false).await;
    wait_for_session_state(&handler, renamed, false).await;
    wait_for_session_state(&handler, keeper, true).await;
    let mut beta_exited = false;
    while let Ok(control) = beta_control_rx.try_recv() {
        beta_exited |= matches!(control, AttachControl::Exited);
    }
    assert!(
        beta_exited,
        "kill-session -g must exit the renamed group client"
    );
    assert!(!handler
        .active_attach
        .lock()
        .await
        .by_pid
        .contains_key(&beta_attach_pid));
}

async fn wait_for_session_state(
    handler: &RequestHandler,
    session_name: SessionName,
    expected: bool,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let exists = handler
            .handle(Request::HasSession(HasSessionRequest {
                target: session_name.clone(),
            }))
            .await;
        if exists == Response::HasSession(rmux_proto::HasSessionResponse { exists: expected }) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session {session_name} did not reach exists={expected}; last response: {exists:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn kill_session_clear_alerts_does_not_request_shutdown() {
    let handler = RequestHandler::new();
    let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: true,
            kill_group: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), shutdown_rx)
            .await
            .is_err(),
        "kill-session -C should not request shutdown while the session survives"
    );
}

#[tokio::test]
async fn kill_session_explicit_id_follows_concurrent_rename_and_preserves_old_name_homonym() {
    let handler = RequestHandler::new();
    let original = create_quiet_kill_session(&handler, "kill-id-rename-original").await;
    let original_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&original)
        .expect("created session exists")
        .id();
    let stable_target = SessionName::new(original_id.to_string()).expect("session id is a target");
    let renamed = session_name("kill-id-rename-current");
    let pause = handler.install_kill_session_selection_identity_pause(original.clone());

    let kill_handler = handler.clone();
    let kill = tokio::spawn(async move {
        kill_handler
            .handle(Request::KillSession(KillSessionRequest {
                target: stable_target,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await
    });

    pause.reached.notified().await;
    let rename = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: original.clone(),
            new_name: renamed.clone(),
        }))
        .await;
    assert!(matches!(rename, Response::RenameSession(_)), "{rename:?}");
    let homonym = create_quiet_kill_session(&handler, original.as_str()).await;
    pause.release.notify_one();

    assert_eq!(
        kill.await.expect("kill task joins"),
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    wait_for_session_state(&handler, renamed, false).await;
    wait_for_session_state(&handler, homonym, true).await;
}
