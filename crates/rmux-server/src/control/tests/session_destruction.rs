use super::*;

#[tokio::test]
async fn session_destruction_flushes_notification_before_transport_exit() {
    for ending in [
        "kill-session -t destruction",
        "kill-window -t destruction:0",
        "kill-pane -t destruction:0.0",
        "unlink-window -k -t destruction:0",
    ] {
        for eof in [false, true] {
            let handler = Arc::new(RequestHandler::new());
            let created = handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: SessionName::new("destruction").unwrap(),
                    detached: true,
                    size: None,
                    environment: None,
                }))
                .await;
            crate::session_fixture::assert_new_session(&created);
            let (server, mut peer) = UnixStream::pair().unwrap();
            let (_shutdown_tx, shutdown_rx) = watch::channel(());
            let (events_tx, events_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
            let closing = Arc::new(AtomicBool::new(false));
            let pid = 42_574;
            let id = handler
                .register_control_with_closing(
                    pid,
                    ControlModeUpgrade {
                        initial_command_count: 1,
                        mode: ControlMode::Plain,
                        terminal_context: OuterTerminalContext::default(),
                    },
                    events_tx,
                    Arc::clone(&closing),
                )
                .await;
            let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
            let input = format!("attach-session -t destruction\n{ending}\n");
            if eof {
                peer.shutdown().await.unwrap();
            }
            let task = tokio::spawn(forward_control_identity(
                server,
                Arc::clone(&handler),
                ControlClientIdentity::new(pid, id),
                ControlUpgradeInput::new(input.into_bytes(), 1),
                shutdown_rx,
                events_rx,
                ControlLifecycle {
                    closing,
                    shutdown_handle,
                },
            ));
            let mut output = Vec::new();
            read_control_to_end(&mut peer, &mut output).await;
            task.await.unwrap().unwrap();
            handler.finish_control(pid, id).await;
            let output = String::from_utf8(output).unwrap();
            assert!(
                output.ends_with("%sessions-changed\n%exit\n"),
                "{ending}, eof={eof}: {output}"
            );
            assert_eq!(output.matches("%sessions-changed\n").count(), 1, "{output}");
        }
    }
}
