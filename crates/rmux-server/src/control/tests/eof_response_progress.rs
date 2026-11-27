use super::*;
use std::future::{poll_fn, Future};
use std::task::Poll;

#[tokio::test(start_paused = true)]
async fn eof_expired_during_transport_pause_preserves_finite_command_responses() {
    let handler = Arc::new(RequestHandler::new());
    let pid = 48_052;
    let (server, mut client) = UnixStream::pair().expect("private control transport");
    let (events_tx, events_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let control_id = handler
        .register_control_with_closing(
            pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            events_tx,
            Arc::clone(&closing),
        )
        .await;
    let identity = ControlClientIdentity::new(pid, control_id);
    let pause = install_control_eof_queue_lease_pause(&handler, identity);
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (shutdown_handle, _shutdown_requests) = ShutdownHandle::new();
    let input = b"display-message -p FIRST\ndisplay-message -p SECOND\ndisplay-message -p THIRD\n";
    client
        .shutdown()
        .await
        .expect("accepted queue followed by EOF");
    let mut transport = Box::pin(forward_control_identity(
        server,
        Arc::clone(&handler),
        identity,
        ControlUpgradeInput::new(input.to_vec(), 0),
        shutdown_rx,
        events_rx,
        ControlLifecycle {
            closing,
            shutdown_handle,
        },
    ));
    // Drive only the transport to the existing EOF lease barrier. Command tasks
    // cannot run between explicit polls on this current-thread runtime.
    tokio::select! {
        biased;
        result = &mut transport => panic!("transport closed before its EOF lease: {result:?}"),
        _ = pause.reached.notified() => {}
    }
    pause.release.notify_one();
    poll_fn(|cx| {
        assert!(transport.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    // Hold transport progression while its first finite task publishes replies
    // and the unchanged global EOF deadline expires. Subsequent tasks have not
    // been admitted yet; their responses must survive that scheduling delay.
    tokio::task::yield_now().await;
    tokio::time::advance(CONTROL_EOF_GRACE + Duration::from_millis(1)).await;
    let mut output = Vec::new();
    let (result, ()) = tokio::join!(
        transport,
        output_drain::read_to_eof(&mut client, &mut output)
    );
    result.expect("finite EOF transport succeeds");
    handler.finish_control(pid, control_id).await;
    let rendered = String::from_utf8(output).expect("UTF-8 transcript");
    let transcript = parse_strict_control_transcript(&rendered);
    assert_eq!(
        transcript.frames.len(),
        4,
        "every accepted finite frame replies: {rendered:?}"
    );
    for (frame, token) in transcript.frames[1..]
        .iter()
        .zip(["FIRST", "SECOND", "THIRD"])
    {
        assert_eq!(frame.terminal, TestGuardTerminal::End);
        assert_eq!(
            frame.payload,
            [token],
            "finite response lost after EOF: {rendered:?}"
        );
    }
    assert_eq!(transcript.asynchronous_notifications, ["%exit"]);
    assert!(rendered.ends_with("%exit\n"));
}
