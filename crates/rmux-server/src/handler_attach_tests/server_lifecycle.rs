use super::*;

#[tokio::test]
async fn lock_client_emits_lock_control_before_refresh() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha, control_tx)
        .await;

    let response = handler
        .handle(Request::LockClient(rmux_proto::LockClientRequest {
            target_client: "=".to_owned(),
        }))
        .await;
    assert_eq!(
        response,
        Response::LockClient(rmux_proto::LockClientResponse {
            target_client: "=".to_owned(),
        })
    );

    match control_rx.recv().await {
        Some(AttachControl::LockShellCommand(command)) => assert!(!command.command().is_empty()),
        Some(other) => panic!("expected lock control, got {other:?}"),
        None => panic!("attach control closed"),
    }
}

#[tokio::test]
async fn lock_server_skips_already_suspended_clients() {
    let handler = RequestHandler::new();
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

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(std::process::id(), alpha, control_tx)
        .await;

    let first_lock = handler
        .handle(Request::LockServer(rmux_proto::LockServerRequest))
        .await;
    assert!(matches!(first_lock, Response::LockServer(_)));
    recv_matching_attach_control(
        &mut control_rx,
        "first lock-server lock control",
        |control| matches!(control, AttachControl::LockShellCommand(_)),
    )
    .await;
    assert!(
        control_rx.try_recv().is_err(),
        "unexpected extra control message after first lock"
    );

    let second_lock = handler
        .handle(Request::LockServer(rmux_proto::LockServerRequest))
        .await;
    assert!(matches!(second_lock, Response::LockServer(_)));
    match control_rx.try_recv() {
        Err(TryRecvError::Empty) => {}
        Ok(msg) => panic!("suspended client must not receive a second lock, got {msg:?}"),
        Err(e) => panic!("unexpected channel state: {e:?}"),
    }
}

#[tokio::test]
async fn lock_session_for_nonexistent_session_returns_error() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::LockSession(rmux_proto::LockSessionRequest {
            target: session_name("nonexistent"),
        }))
        .await;
    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn kill_server_sets_shutdown_flag() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::KillServer(rmux_proto::KillServerRequest))
        .await;
    assert!(matches!(response, Response::KillServer(_)));

    assert!(
        handler.request_shutdown_if_pending(),
        "shutdown flag must be set after kill-server"
    );
    assert!(
        !handler.request_shutdown_if_pending(),
        "shutdown flag must be consumed after first check"
    );
}

#[tokio::test]
async fn daemon_status_reports_version_and_activity_counts() {
    let handler = RequestHandler::new();
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
        .register_attach(std::process::id(), alpha, control_tx)
        .await;

    let response = handler
        .handle(Request::DaemonStatus(rmux_proto::DaemonStatusRequest))
        .await;
    let Response::DaemonStatus(status) = response else {
        panic!("expected daemon status response");
    };
    assert_eq!(status.rmux_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(status.wire_version, rmux_proto::RMUX_WIRE_VERSION);
    assert_eq!(status.session_count, 1);
    assert_eq!(status.client_count, 1);
}

#[tokio::test]
async fn shutdown_if_idle_queues_shutdown_only_when_empty() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::ShutdownIfIdle(rmux_proto::ShutdownIfIdleRequest))
        .await;
    assert_eq!(
        response,
        Response::ShutdownIfIdle(rmux_proto::ShutdownIfIdleResponse {
            shutdown: true,
            session_count: 0,
            client_count: 0,
        })
    );
    assert!(handler.request_shutdown_if_pending());
}

#[tokio::test]
async fn shutdown_if_idle_refuses_live_sessions() {
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

    let response = handler
        .handle(Request::ShutdownIfIdle(rmux_proto::ShutdownIfIdleRequest))
        .await;
    assert_eq!(
        response,
        Response::ShutdownIfIdle(rmux_proto::ShutdownIfIdleResponse {
            shutdown: false,
            session_count: 1,
            client_count: 0,
        })
    );
    assert!(!handler.request_shutdown_if_pending());
}

#[tokio::test]
async fn shutdown_if_idle_refuses_in_flight_detached_requests() {
    let handler = RequestHandler::new();
    let _guard = handler.begin_detached_request();

    let response = handler
        .handle(Request::ShutdownIfIdle(rmux_proto::ShutdownIfIdleRequest))
        .await;
    assert_eq!(
        response,
        Response::ShutdownIfIdle(rmux_proto::ShutdownIfIdleResponse {
            shutdown: false,
            session_count: 0,
            client_count: 1,
        })
    );
    assert!(!handler.request_shutdown_if_pending());
}

#[tokio::test]
async fn server_access_protects_owner_uid() {
    let handler = RequestHandler::with_owner_uid(current_owner_uid());

    let response = handler
        .handle(Request::ServerAccess(rmux_proto::ServerAccessRequest {
            add: true,
            deny: false,
            list: false,
            read_only: false,
            write: false,
            target: None,
            user: Some(current_owner_uid().to_string()),
        }))
        .await;
    assert!(
        matches!(response, Response::Error(_)),
        "modifying the owner UID must fail"
    );
}

#[tokio::test]
async fn server_access_deny_nonexistent_user_returns_error() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::ServerAccess(rmux_proto::ServerAccessRequest {
            add: false,
            deny: true,
            list: false,
            read_only: false,
            write: false,
            target: None,
            user: Some("99999".to_owned()),
        }))
        .await;
    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn server_access_list_skips_uid_zero() {
    let handler = RequestHandler::with_owner_uid(1000);

    let response = handler
        .handle(Request::ServerAccess(rmux_proto::ServerAccessRequest {
            add: false,
            deny: false,
            list: true,
            read_only: false,
            write: false,
            target: None,
            user: None,
        }))
        .await;
    match response {
        Response::ServerAccess(ref access) => {
            let stdout = String::from_utf8_lossy(&access.output.stdout);
            assert!(
                !stdout.contains("root"),
                "UID 0 must be skipped in server-access -l output"
            );
        }
        _ => panic!("expected ServerAccess response"),
    }
}

#[cfg(not(windows))]
#[tokio::test]
async fn server_access_combined_flags_resolve_user_before_mutation() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::ServerAccess(rmux_proto::ServerAccessRequest {
            add: true,
            deny: true,
            list: false,
            read_only: false,
            write: false,
            target: None,
            user: Some("rmux-no-such-user".to_owned()),
        }))
        .await;
    match response {
        Response::Error(error) => assert_eq!(
            error.error.to_string(),
            "server error: unknown user: rmux-no-such-user"
        ),
        _ => panic!("expected unknown user error"),
    }

    let response = handler
        .handle(Request::ServerAccess(rmux_proto::ServerAccessRequest {
            add: false,
            deny: false,
            list: false,
            read_only: true,
            write: true,
            target: None,
            user: Some("rmux-no-such-user".to_owned()),
        }))
        .await;
    match response {
        Response::Error(error) => assert_eq!(
            error.error.to_string(),
            "server error: unknown user: rmux-no-such-user"
        ),
        _ => panic!("expected unknown user error"),
    }
}

#[cfg(windows)]
#[tokio::test]
async fn server_access_user_mutations_are_rejected_before_user_resolution_windows() {
    let handler = RequestHandler::new();
    let expected = "server error: server-access user mutations are unsupported on Windows; named-pipe access is scoped to the current Windows SID";

    for request in [
        rmux_proto::ServerAccessRequest {
            add: true,
            deny: true,
            list: false,
            read_only: false,
            write: false,
            target: None,
            user: Some("rmux-no-such-user".to_owned()),
        },
        rmux_proto::ServerAccessRequest {
            add: false,
            deny: false,
            list: false,
            read_only: true,
            write: true,
            target: None,
            user: Some("rmux-no-such-user".to_owned()),
        },
    ] {
        match handler.handle(Request::ServerAccess(request)).await {
            Response::Error(error) => assert_eq!(error.error.to_string(), expected),
            _ => panic!("expected Windows server-access unsupported mutation error"),
        }
    }
}

#[tokio::test]
async fn server_access_list_ignores_user_and_mutation_flags() {
    let handler = RequestHandler::with_owner_uid(1000);

    let response = handler
        .handle(Request::ServerAccess(rmux_proto::ServerAccessRequest {
            add: true,
            deny: true,
            list: true,
            read_only: false,
            write: false,
            target: None,
            user: Some("rmux-no-such-user".to_owned()),
        }))
        .await;
    assert!(matches!(response, Response::ServerAccess(_)));
}
