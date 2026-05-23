use super::*;

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

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let _control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
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
async fn control_client_flags_without_session_emit_only_control_mode() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let _control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
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
async fn list_clients_exposes_pid_and_tty_format_variables_for_attached_clients() {
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

    let response = handler
        .handle(Request::ListClients(rmux_proto::ListClientsRequest {
            format: Some("#{client_name}|#{client_pid}|#{client_tty}".to_owned()),
            target_session: None,
            filter: None,
            sort_order: None,
            reversed: false,
        }))
        .await;
    let Response::ListClients(response) = response else {
        panic!("expected list-clients response");
    };
    let output = String::from_utf8(response.output.stdout().to_vec()).expect("utf-8");
    let line = output.lines().next().expect("client line");
    let parts = line.split('|').collect::<Vec<_>>();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[1], requester_pid.to_string());
    #[cfg(unix)]
    assert!(!parts[2].is_empty(), "client_tty should be populated");
    #[cfg(windows)]
    assert_eq!(parts[2], "");
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
