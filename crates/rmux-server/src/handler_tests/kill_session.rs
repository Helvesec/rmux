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
