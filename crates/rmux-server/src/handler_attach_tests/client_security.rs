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
    assert!(
        matches!(second_rx.try_recv(), Ok(AttachControl::Detach)),
        "the other client must receive a detach control"
    );
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
    assert!(
        matches!(control_rx.try_recv(), Ok(AttachControl::Suspend)),
        "suspend-client must emit Suspend control"
    );

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
    use crate::handler::attach_support::ClientFlags;

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
            Request::RefreshClient(RefreshClientRequest {
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
            }),
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
                .contains(crate::handler::attach_support::ClientFlags::ACTIVEPANE),
            "active-pane flag must be set after refresh-client -f"
        );
    }

    let response = handler
        .dispatch(
            std::process::id(),
            Request::RefreshClient(RefreshClientRequest {
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
            }),
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
                .contains(crate::handler::attach_support::ClientFlags::ACTIVEPANE),
            "active-pane flag must still be set after second refresh-client -f"
        );
        assert!(
            active
                .flags
                .contains(crate::handler::attach_support::ClientFlags::NO_DETACH_ON_DESTROY),
            "no-detach-on-destroy flag must be set after second refresh-client -f"
        );
    }
}
