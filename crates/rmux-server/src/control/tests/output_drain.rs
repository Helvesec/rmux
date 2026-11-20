use std::io::ErrorKind;
use std::time::Instant;

use tokio::net::UnixStream;

use super::CONTROL_TEST_TIMEOUT;

#[tokio::test]
async fn control_mode_frame_order_survives_backpressure() {
    use super::*;

    const COMMAND_PAIRS: usize = 32;
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("control-frame-backpressure").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    let (server, mut client) = UnixStream::pair().expect("private control transport");
    // Force partial native writes without depending on the host socket default.
    rustix::net::sockopt::set_socket_send_buffer_size(&server, 4096)
        .expect("bound native send buffer");
    let payloads: Vec<_> = (0..COMMAND_PAIRS)
        .map(|index| format!("FRAME-{index:02}-{}", "x".repeat(16 * 1024)))
        .collect();
    let mut commands = Vec::new();
    for (index, payload) in payloads.iter().enumerate() {
        commands.push(format!("display-message -p -- {payload}"));
        commands.push(format!("display-message -- NOTICE-{index:02}"));
    }
    let input = format!("{}\n", commands.join("\n")).into_bytes();
    let (events_tx, events_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let pid = 48_027;
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
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (shutdown_handle, _shutdown_requests) = ShutdownHandle::new();
    let task = tokio::spawn(forward_control_identity(
        server,
        Arc::clone(&handler),
        ControlClientIdentity::new(pid, control_id),
        ControlUpgradeInput::new(input, 0),
        shutdown_rx,
        events_rx,
        ControlLifecycle {
            closing,
            shutdown_handle,
        },
    ));
    client
        .shutdown()
        .await
        .expect("input EOF follows accepted commands");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!task.is_finished(), "paused reader must exert backpressure");
    let mut output = Vec::new();
    read_to_eof(&mut client, &mut output).await;
    task.await
        .expect("control joins")
        .expect("control transport succeeds");
    handler.finish_control(pid, control_id).await;

    let rendered = String::from_utf8(output).expect("complete UTF-8 framing");
    let transcript = parse_strict_control_transcript(&rendered);
    assert_eq!(transcript.frames.len(), 1 + COMMAND_PAIRS * 2);
    assert!(transcript.frames[0].payload.is_empty());
    for (index, payload) in payloads.iter().enumerate() {
        let print = &transcript.frames[1 + index * 2];
        assert_eq!(
            print.terminal,
            TestGuardTerminal::End,
            "{:?}",
            print.payload
        );
        assert_eq!(print.payload, [payload.as_str()]);
        assert!(print.notifications.is_empty());
        let notice = &transcript.frames[2 + index * 2];
        assert_eq!(notice.terminal, TestGuardTerminal::End);
        assert!(notice.payload.is_empty());
        assert_message_owned_once(&transcript, 2 + index * 2, &format!("NOTICE-{index:02}"));
    }
    assert_eq!(transcript.asynchronous_notifications, ["%exit"]);
    assert!(
        rendered.ends_with("%exit\n"),
        "EOF follows the final complete frame"
    );
}

pub(super) async fn read_to_eof(client_stream: &mut UnixStream, output: &mut Vec<u8>) {
    // Format subscription tests pause Tokio's clock. A concurrent PTY child can
    // retain an inherited socket until exec, so EOF still depends on native I/O.
    // Keep the runtime runnable instead of auto-advancing its timeout or cadence.
    let started = Instant::now();
    let mut buffer = [0_u8; 1024];
    loop {
        match client_stream.try_read(&mut buffer) {
            Ok(0) => return,
            Ok(count) => output.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => panic!("control output drains: {error}"),
        }
        assert!(
            started.elapsed() < CONTROL_TEST_TIMEOUT,
            "control output reaches EOF before the real-time safety bound"
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn slow_reader_isolation_oversized_message_is_explicitly_rejected() {
    use super::*;

    // Exercise the upgrade buffer, its initial-command reader, and the live reader.
    for (route, mode) in (0..3).flat_map(|route| {
        [ControlMode::Plain, ControlMode::ControlControl].map(|mode| (route, mode))
    }) {
        let handler = Arc::new(RequestHandler::new());
        let (server, mut client) = UnixStream::pair().expect("private local transport");
        let (_shutdown_tx, shutdown_rx) = watch::channel(());
        let (_events_tx, events_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        let (shutdown_handle, _shutdown_requests) = ShutdownHandle::new();
        let oversized = vec![b'x'; MAX_CONTROL_LINE_BYTES + 1];
        let initial = if route == 0 {
            oversized.clone()
        } else {
            Vec::new()
        };
        let task = tokio::spawn(forward_control(
            server,
            Arc::clone(&handler),
            48_018 + route,
            ControlUpgradeInput::with_mode(initial, usize::from(route != 2), mode),
            shutdown_rx,
            events_rx,
            ControlLifecycle {
                closing: Arc::new(AtomicBool::new(false)),
                shutdown_handle,
            },
        ));
        if route != 0 {
            tokio::time::timeout(CONTROL_TEST_TIMEOUT, client.write_all(&oversized))
                .await
                .expect("local input progresses")
                .expect("oversized input reaches its reader");
        }
        let mut rendered = Vec::new();
        read_control_to_end(&mut client, &mut rendered).await;
        let error = task
            .await
            .expect("control reader joins")
            .expect_err("oversized message is rejected locally");
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        let rendered = String::from_utf8(rendered).expect("control response is UTF-8");
        assert!(
            rendered.lines().any(|line| {
                line.starts_with("%exit ") && line.contains("control input line exceeds")
            }),
            "route {route}: oversized message must be rejected on the wire: {rendered:?}"
        );
        assert!(
            !rendered
                .lines()
                .any(|line| { line.starts_with("%end ") && line.ends_with(" 1") }),
            "route {route}: rejected input must not get a successful command reply"
        );
    }
}
