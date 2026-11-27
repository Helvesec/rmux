use super::*;

#[tokio::test]
async fn plain_control_ready_attach_writes_session_change_before_new_pane_output() {
    // tmux 3.7b drains its notification queue right after the command that
    // attached a control client, before it dispatches any pane read, so
    // `%session-changed` precedes every `%output` of the attached session.
    // Hold the RMUX transport until the attach command has completed, its
    // session change and notification are queued, and the pane has produced
    // output after its attach cursor: the completion then wins the biased
    // select, which is the order a loaded host produces.
    let handler = Arc::new(RequestHandler::new());
    let requester_pid = 42_432;
    let session_name =
        SessionName::new("plain-control-attach-notice-order").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    let pane_output = handler
        .control_session_panes(&session_name)
        .await
        .expect("session pane output is available")
        .into_iter()
        .next()
        .expect("initial pane has an output sender")
        .1;

    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 1,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            event_tx,
            Arc::clone(&closing),
        )
        .await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    let eof_pause = install_control_eof_queue_lease_pause(&handler, identity);
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard =
        handler.begin_test_detached_requester_access(requester_pid, AccessMode::ReadWrite);
    let command =
        format!("attach-session -t {session_name} ; set-buffer -b attach-notice-ready done\n");
    let handler_for_control = Arc::clone(&handler);
    let control_task = tokio::spawn(async move {
        let result = forward_control_identity(
            server_stream,
            Arc::clone(&handler_for_control),
            identity,
            ControlUpgradeInput::with_mode(command.into_bytes(), 1, ControlMode::Plain),
            shutdown_rx,
            event_rx,
            ControlLifecycle {
                closing,
                shutdown_handle,
            },
        )
        .await;
        handler_for_control
            .finish_control(requester_pid, control_id)
            .await;
        result
    });

    client_stream
        .shutdown()
        .await
        .expect("client write half closes immediately");
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, eof_pause.reached.notified())
        .await
        .expect("forward loop observes EOF while attach is active");
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        loop {
            let ready = handler
                .handle(Request::ShowBuffer(ShowBufferRequest {
                    name: Some("attach-notice-ready".to_owned()),
                }))
                .await
                .command_output()
                .is_some_and(|output| output.stdout() == b"done");
            if ready {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("attach command completes while the forward loop remains paused");
    pane_output.send(b"ATTACH_NOTICE_LIVE".to_vec());
    eof_pause.release.notify_one();

    let mut rendered = Vec::new();
    read_control_to_end(&mut client_stream, &mut rendered).await;
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");

    let rendered = String::from_utf8(rendered).expect("utf-8 control stream");
    let records = rendered.lines().collect::<Vec<_>>();
    assert!(records.len() >= 4, "{rendered:?}");
    assert!(records[0].starts_with("%begin "), "{rendered:?}");
    assert!(records[1].starts_with("%end "), "{rendered:?}");
    assert!(
        records[2].starts_with("%session-changed $")
            && records[2].ends_with(&format!(" {session_name}")),
        "attach notice must precede the attached pane's output: {rendered:?}"
    );
    assert_eq!(records.last(), Some(&"%exit"), "{rendered:?}");
}
