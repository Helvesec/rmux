use super::*;

#[tokio::test]
async fn lock_client_with_empty_lock_command_is_noop() {
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

    handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::LockCommand,
            value: String::new(),
            mode: SetOptionMode::Replace,
        }))
        .await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(std::process::id(), alpha, control_tx)
        .await;

    let response = handler
        .handle(Request::LockClient(rmux_proto::LockClientRequest {
            target_client: "=".to_owned(),
        }))
        .await;
    assert!(matches!(response, Response::LockClient(_)));

    assert!(
        matches!(control_rx.try_recv(), Err(TryRecvError::Empty)),
        "empty lock-command must not send lock control"
    );
}

#[tokio::test]
async fn lock_client_with_invalid_target_returns_error() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let response = handler
        .handle(Request::LockClient(rmux_proto::LockClientRequest {
            target_client: "not-a-number".to_owned(),
        }))
        .await;
    assert!(
        matches!(response, Response::Error(_)),
        "non-numeric lock-client target must fail"
    );

    let response = handler
        .handle(Request::LockClient(rmux_proto::LockClientRequest {
            target_client: "99999".to_owned(),
        }))
        .await;
    assert!(
        matches!(response, Response::Error(_)),
        "lock-client for unattached PID must fail"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn lock_client_accepts_tty_path_targets() {
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

    handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::LockCommand,
            value: String::new(),
            mode: SetOptionMode::Replace,
        }))
        .await;

    let mut child = spawn_tty_child().expect("spawn tty child");
    let tty_path = rmux_os::process::fd_path(child.id(), 0).expect("tty path");
    let tty_target = tty_path.display().to_string();
    let tty_basename = tty_path
        .strip_prefix("/dev")
        .expect("strip /dev prefix")
        .display()
        .to_string();

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler.register_attach(child.id(), alpha, control_tx).await;

    let response = handler
        .handle(Request::LockClient(rmux_proto::LockClientRequest {
            target_client: tty_target,
        }))
        .await;
    assert!(
        matches!(response, Response::LockClient(_)),
        "full tty path target should lock the client, got {response:?}"
    );

    let response = handler
        .handle(Request::LockClient(rmux_proto::LockClientRequest {
            target_client: tty_basename,
        }))
        .await;
    assert!(
        matches!(response, Response::LockClient(_)),
        "basename tty target should lock the client, got {response:?}"
    );

    terminate_child(&mut child);
}

#[cfg(unix)]
#[tokio::test]
async fn overlay_commands_resolve_names_published_by_list_clients() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let mut child = spawn_tty_child().expect("spawn tty child");
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler.register_attach(child.id(), alpha, control_tx).await;
    let other_pid = child.id().saturating_add(10_000);
    let (other_tx, _other_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(other_pid, session_name("alpha"), other_tx)
        .await;
    let published_name = handler
        .list_clients_snapshot()
        .await
        .into_iter()
        .find(|client| client.pid == child.id())
        .expect("attached client is listed")
        .name;

    for command in ["display-menu", "display-popup"] {
        assert_eq!(
            handler
                .resolve_overlay_client(999_999, Some(&published_name), command)
                .await
                .expect("published client name resolves"),
            child.id(),
            "{command} must accept the name emitted by list-clients"
        );
    }

    let menu = handler
        .parse_control_commands(&format!("display-menu -c {published_name} Item i true"))
        .await
        .expect("inventory client name parses for display-menu");
    handler
        .execute_parsed_commands_for_test(other_pid, menu)
        .await
        .expect("display-menu targets the published client name");
    let _ = recv_matching_attach_control(&mut control_rx, "targeted menu overlay", |control| {
        matches!(control, AttachControl::Overlay(_))
    })
    .await;
    {
        let active = handler.active_attach.lock().await;
        assert!(active.by_pid[&child.id()].overlay.is_some());
        assert!(active.by_pid[&other_pid].overlay.is_none());
    }

    let close = handler
        .parse_control_commands(&format!("display-popup -C -c {published_name}"))
        .await
        .expect("inventory client name parses for display-popup");
    handler
        .execute_parsed_commands_for_test(other_pid, close)
        .await
        .expect("display-popup targets the published client name");
    let active = handler.active_attach.lock().await;
    assert!(active.by_pid[&child.id()].overlay.is_none());
    assert!(active.by_pid[&other_pid].overlay.is_none());
    drop(active);

    terminate_child(&mut child);
}

#[tokio::test]
async fn server_access_list_returns_server_access_response() {
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
    assert!(
        matches!(response, Response::ServerAccess(_)),
        "server-access -l must return ServerAccess response"
    );
}

#[cfg(unix)]
struct TtyChild {
    spawned: rmux_pty::SpawnedPty,
}

#[cfg(unix)]
impl TtyChild {
    fn id(&self) -> u32 {
        self.spawned.child().pid().as_u32()
    }
}

#[cfg(unix)]
fn spawn_tty_child() -> Result<TtyChild, Box<dyn std::error::Error>> {
    let spawned = ChildCommand::new("sh")
        .arg("-c")
        .arg("sleep 60")
        .size(PtyTerminalSize { cols: 80, rows: 24 })
        .spawn()?;

    Ok(TtyChild { spawned })
}

#[cfg(unix)]
fn terminate_child(child: &mut TtyChild) {
    let _ = child.spawned.child().terminate_forcefully();
    let _ = child.spawned.child_mut().wait();
}

#[tokio::test]
async fn detach_client_all_other_detaches_only_non_requester_clients() {
    use rmux_proto::request::DetachClientExtRequest;

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

    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();
    let _first_attach = handler.register_attach(101, alpha.clone(), first_tx).await;
    let _second_attach = handler.register_attach(202, alpha, second_tx).await;

    let response = handler
        .dispatch(
            101,
            Request::DetachClientExt(DetachClientExtRequest {
                target_client: None,
                all_other_clients: true,
                target_session: None,
                kill_on_detach: false,
                exec_command: None,
            }),
        )
        .await
        .response;
    assert!(matches!(response, Response::DetachClient(_)));

    assert!(
        matches!(first_rx.try_recv(), Err(TryRecvError::Empty)),
        "the requester itself must not be detached"
    );
    let _ = recv_matching_attach_control(&mut second_rx, "other client detach", |control| {
        matches!(control, AttachControl::Detach)
    })
    .await;
}

#[tokio::test]
async fn suspend_client_marks_client_as_suspended() {
    use rmux_proto::request::SuspendClientRequest;

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

    let response = handler
        .dispatch(
            std::process::id(),
            Request::SuspendClient(SuspendClientRequest {
                target_client: None,
            }),
        )
        .await
        .response;
    assert!(matches!(response, Response::SuspendClient(_)));
    let _ = recv_matching_attach_control(&mut control_rx, "suspend-client control", |control| {
        matches!(control, AttachControl::Suspend)
    })
    .await;

    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&std::process::id())
            .expect("attached client must exist");
        assert!(active.suspended, "client must be marked suspended");
    }
}

#[tokio::test]
async fn client_flags_apply_named_supports_negate_prefix() {
    use super::super::attach_support::ClientFlags;

    let mut flags = ClientFlags::default();
    flags.apply_named("read-only").expect("apply read-only");
    assert!(flags.contains(ClientFlags::READONLY));

    flags.apply_named("!read-only").expect("negate read-only");
    assert!(!flags.contains(ClientFlags::READONLY));

    flags.apply_named("active-pane").expect("apply active-pane");
    flags
        .apply_named("no-detach-on-destroy")
        .expect("apply no-detach-on-destroy");
    assert!(flags.contains(ClientFlags::ACTIVEPANE));
    assert!(flags.contains(ClientFlags::NO_DETACH_ON_DESTROY));

    flags
        .apply_named("!active-pane")
        .expect("negate active-pane");
    assert!(!flags.contains(ClientFlags::ACTIVEPANE));
    assert!(flags.contains(ClientFlags::NO_DETACH_ON_DESTROY));
}

#[tokio::test]
async fn refresh_client_flags_merge_incrementally() {
    use rmux_proto::request::RefreshClientRequest;

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

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(std::process::id(), alpha, control_tx)
        .await;

    let response = handler
        .dispatch(
            std::process::id(),
            Request::RefreshClient(Box::new(RefreshClientRequest {
                target_client: None,
                adjustment: None,
                clear_pan: false,
                pan_left: false,
                pan_right: false,
                pan_up: false,
                pan_down: false,
                status_only: false,
                clipboard_query: false,
                flags: Some("active-pane".to_owned()),
                flags_alias: None,
                subscriptions: vec![],
                subscriptions_format: vec![],
                control_size: None,
                colour_report: None,
            })),
        )
        .await
        .response;
    assert!(matches!(response, Response::RefreshClient(_)));

    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&std::process::id())
            .expect("attached client must exist");
        assert!(
            active
                .flags
                .contains(super::super::attach_support::ClientFlags::ACTIVEPANE),
            "active-pane flag must be set after refresh-client -f"
        );
    }

    let response = handler
        .dispatch(
            std::process::id(),
            Request::RefreshClient(Box::new(RefreshClientRequest {
                target_client: None,
                adjustment: None,
                clear_pan: false,
                pan_left: false,
                pan_right: false,
                pan_up: false,
                pan_down: false,
                status_only: false,
                clipboard_query: false,
                flags: Some("no-detach-on-destroy".to_owned()),
                flags_alias: None,
                subscriptions: vec![],
                subscriptions_format: vec![],
                control_size: None,
                colour_report: None,
            })),
        )
        .await
        .response;
    assert!(matches!(response, Response::RefreshClient(_)));

    {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&std::process::id())
            .expect("attached client must exist");
        assert!(
            active
                .flags
                .contains(super::super::attach_support::ClientFlags::ACTIVEPANE),
            "active-pane flag must still be set after second refresh-client -f"
        );
        assert!(
            active
                .flags
                .contains(super::super::attach_support::ClientFlags::NO_DETACH_ON_DESTROY),
            "no-detach-on-destroy flag must be set after second refresh-client -f"
        );
    }
}

#[tokio::test]
async fn refresh_client_reserved_wire_fields_from_old_clients_are_rejected() {
    use rmux_proto::request::RefreshClientRequest;

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

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(std::process::id(), alpha, control_tx)
        .await;

    for (subscriptions, subscriptions_format, colour_report, expected) in [
        (
            vec!["%0:on".to_owned()],
            Vec::new(),
            None,
            "refresh-client -A is not supported",
        ),
        (
            Vec::new(),
            vec!["name:%0:#{pane_id}".to_owned()],
            None,
            "refresh-client -B is not supported",
        ),
        (
            Vec::new(),
            Vec::new(),
            Some("%0".to_owned()),
            "refresh-client -r is not supported",
        ),
    ] {
        let response = handler
            .dispatch(
                std::process::id(),
                Request::RefreshClient(Box::new(RefreshClientRequest {
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
                    subscriptions,
                    subscriptions_format,
                    control_size: Some("80x24".to_owned()),
                    colour_report,
                })),
            )
            .await
            .response;

        assert_eq!(
            response,
            Response::Error(rmux_proto::ErrorResponse {
                error: RmuxError::Server(expected.to_owned()),
            })
        );
    }
}
