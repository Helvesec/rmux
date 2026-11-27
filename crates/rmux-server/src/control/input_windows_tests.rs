use std::io::ErrorKind;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
use tokio::sync::{mpsc, watch};

use super::super::{
    forward_control, ControlClientIdentity, ControlLifecycle, ControlMode, ControlUpgradeInput,
    RequestHandler, ShutdownHandle, CONTROL_SERVER_EVENT_CAPACITY,
};
use super::MAX_CONTROL_LINE_BYTES;

const CONTROL_TEST_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn slow_reader_isolation_disconnected_pipe_preserves_rejection_write_error() {
    let name = format!(
        r"\\.\pipe\rmux-control-rejection-gone-{}",
        std::process::id()
    );
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&name)
        .expect("private named pipe");
    let client = ClientOptions::new().open(&name).expect("pipe client");
    server.connect().await.expect("pipe connection");
    drop(client);
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_events_tx, events_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let (shutdown_handle, _shutdown_requests) = ShutdownHandle::new();
    let result = tokio::time::timeout(
        CONTROL_TEST_TIMEOUT,
        forward_control(
            server,
            Arc::new(RequestHandler::new()),
            ControlClientIdentity::new(48_022, 0),
            ControlUpgradeInput::with_mode(
                vec![b'x'; MAX_CONTROL_LINE_BYTES + 1],
                1,
                ControlMode::Plain,
            ),
            shutdown_rx,
            events_rx,
            ControlLifecycle {
                closing: Arc::new(AtomicBool::new(false)),
                shutdown_handle,
            },
        ),
    )
    .await
    .expect("disconnected rejection is bounded");
    let error = result.expect_err("a lost rejection must never become silent success");
    assert!(
        rmux_ipc::is_peer_disconnect(&error),
        "preserve the transport error instead of replacing it with the input error: {error}"
    );
}

#[tokio::test]
async fn slow_reader_isolation_oversized_message_is_explicitly_rejected_on_named_pipe() {
    assert_named_pipe_rejection(Duration::ZERO).await;
}

#[tokio::test]
async fn slow_reader_isolation_delayed_named_pipe_reader_keeps_explicit_rejection() {
    assert_named_pipe_rejection(Duration::from_millis(50)).await;
}

async fn assert_named_pipe_rejection(read_delay: Duration) {
    // The same three input readers and two modes as the Unix acceptance.
    for (route, mode) in (0..3).flat_map(|route| {
        [ControlMode::Plain, ControlMode::ControlControl].map(|mode| (route, mode))
    }) {
        let name = format!(
            r"\\.\pipe\rmux-control-input-{}-{route}-{mode:?}-{}",
            std::process::id(),
            read_delay.as_millis()
        );
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .expect("private named pipe");
        let mut client = ClientOptions::new().open(&name).expect("pipe client");
        server.connect().await.expect("pipe connection");
        let handler = Arc::new(RequestHandler::new());
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
            handler,
            ControlClientIdentity::new(48_019 + route, u64::from(route)),
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
        // A live peer may start reading after the server submitted its rejection.
        // Named-pipe close must not discard those already accepted output bytes.
        if !read_delay.is_zero() {
            tokio::time::sleep(read_delay).await;
        }
        let mut rendered = Vec::new();
        tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
            let mut buffer = [0_u8; 1024];
            loop {
                match client.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(count) => rendered.extend_from_slice(&buffer[..count]),
                    Err(error) if error.kind() == ErrorKind::BrokenPipe => break,
                    Err(error) => panic!("control output drains: {error}"),
                }
            }
        })
        .await
        .expect("control output reaches pipe closure");
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
            "route {route}, mode {mode:?}: oversized message must be rejected on the wire: {rendered:?}"
        );
        assert!(
            !rendered
                .lines()
                .any(|line| { line.starts_with("%end ") && line.ends_with(" 1") }),
            "route {route}: rejected input must not get a successful command reply"
        );
    }
}
