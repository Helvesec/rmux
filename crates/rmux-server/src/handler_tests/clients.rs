use super::*;
use crate::control::ControlServerEvent;

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

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let _control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
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
            Request::RefreshClient(rmux_proto::request::RefreshClientRequest {
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
            }),
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
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let _control_id = handler
            .register_control_with_closing(
                pid,
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
            format: Some("#{client_name}|#{client_pid}|#{client_tty}|#{client_session}".to_owned()),
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
    assert_eq!(parts.len(), 4);
    assert_eq!(parts[1], requester_pid.to_string());
    assert_eq!(parts[3], "alpha");
    #[cfg(unix)]
    assert!(!parts[2].is_empty(), "client_tty should be populated");
    #[cfg(windows)]
    assert_eq!(parts[2], "");
}

#[tokio::test]
async fn list_clients_exposes_attached_key_table_and_prefix_state() {
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
}

async fn list_client_prefix_state(handler: &RequestHandler) -> String {
    let response = handler
        .handle(Request::ListClients(rmux_proto::ListClientsRequest {
            format: Some("#{client_prefix}|#{client_key_table}".to_owned()),
            target_session: None,
            filter: None,
            sort_order: None,
            reversed: false,
        }))
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
