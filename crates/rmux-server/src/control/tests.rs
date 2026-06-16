use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};

use super::{
    ensure_control_newline, extract_complete_control_lines, forward_control, ControlLifecycle,
    ControlOutputQueue, ControlServerEvent,
};
use crate::daemon::ShutdownHandle;
use crate::handler::RequestHandler;
use rmux_proto::{Request, Response, WaitForMode, WaitForRequest, WaitForResponse};

const CONTROL_TEST_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn extracts_complete_control_lines_from_buffer() {
    let mut buffer = b"one\ntwo\r\nthree".to_vec();
    let lines = extract_complete_control_lines(&mut buffer);

    assert_eq!(lines, vec!["one".to_owned(), "two".to_owned()]);
    assert_eq!(buffer, b"three");
}

#[test]
fn extracts_empty_line_for_exit_trigger() {
    let mut buffer = b"\n".to_vec();
    let lines = extract_complete_control_lines(&mut buffer);

    assert_eq!(lines, vec!["".to_owned()]);
    assert!(buffer.is_empty());
}

#[test]
fn empty_buffer_produces_no_lines() {
    let mut buffer = Vec::new();
    let lines = extract_complete_control_lines(&mut buffer);

    assert!(lines.is_empty());
    assert!(buffer.is_empty());
}

#[test]
fn multiple_empty_lines_are_preserved() {
    let mut buffer = b"\n\ncommand\n".to_vec();
    let lines = extract_complete_control_lines(&mut buffer);

    assert_eq!(
        lines,
        vec!["".to_owned(), "".to_owned(), "command".to_owned()]
    );
    assert!(buffer.is_empty());
}

#[test]
fn stdout_lines_are_newline_terminated() {
    assert_eq!(ensure_control_newline(b"hello".to_vec()), b"hello\n");
    assert_eq!(ensure_control_newline(b"hello\n".to_vec()), b"hello\n");
}

#[test]
fn output_queue_tracks_buffered_bytes() {
    let mut queue = ControlOutputQueue::default();
    assert_eq!(queue.buffered_bytes, 0);

    queue.enqueue_line(b"hello\n".to_vec(), true);
    assert_eq!(queue.buffered_bytes, 6);

    queue.enqueue_stdout(b"world".to_vec());
    assert_eq!(queue.buffered_bytes, 12); // 6 + "world\n" = 6
}

#[test]
fn enqueue_stdout_skips_empty_bytes() {
    let mut queue = ControlOutputQueue::default();
    queue.enqueue_stdout(Vec::new());
    assert_eq!(queue.blocks.len(), 0);
    assert_eq!(queue.buffered_bytes, 0);
}

#[tokio::test]
async fn notifications_wait_until_after_the_active_command_block() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (server_event_tx, server_event_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        b"wait-for control-test-block\n\n".to_vec(),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing: Arc::clone(&closing),
            shutdown_handle,
        },
    ));

    let mut begin_prefix = vec![0_u8; 256];
    let bytes_read = client_stream
        .read(&mut begin_prefix)
        .await
        .expect("control output begins");
    let begin_prefix =
        String::from_utf8(begin_prefix[..bytes_read].to_vec()).expect("control output is utf-8");
    assert!(
        begin_prefix.contains("%begin "),
        "expected begin guard in initial output: {begin_prefix:?}"
    );

    wait_for_waiter(&handler, "control-test-block").await;
    server_event_tx
        .send(ControlServerEvent::Notification(
            "%message command-notification-finished".to_owned(),
        ))
        .expect("notification send succeeds");
    drop(server_event_tx);
    let response = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: "control-test-block".to_owned(),
            mode: WaitForMode::Signal,
        }))
        .await;
    assert!(matches!(response, Response::WaitFor(WaitForResponse)));

    let mut remaining = Vec::new();
    read_control_to_end(&mut client_stream, &mut remaining).await;
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");

    let rendered = format!(
        "{begin_prefix}{}",
        String::from_utf8(remaining).expect("control output is utf-8")
    );
    let end_index = rendered.find("%end ").expect("end guard present");
    let notification_index = rendered
        .find("%message command-notification-finished")
        .expect("notification present");

    assert!(
        end_index < notification_index,
        "notifications must flush after the command block closes: {rendered:?}"
    );
}

#[tokio::test]
async fn eof_on_empty_input_emits_bare_exit() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        Vec::new(),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing: Arc::clone(&closing),
            shutdown_handle,
        },
    ));

    client_stream
        .shutdown()
        .await
        .expect("client write half closes");

    let mut rendered = Vec::new();
    client_stream
        .read_to_end(&mut rendered)
        .await
        .expect("control output drains");
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");

    assert_eq!(
        rendered, b"%exit\n",
        "EOF must be promoted to a bare %exit with no guard tuple"
    );
}

#[tokio::test]
async fn eof_after_command_block_appends_exit() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        b"display-message -p ok\n".to_vec(),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing: Arc::clone(&closing),
            shutdown_handle,
        },
    ));

    client_stream
        .shutdown()
        .await
        .expect("client write half closes");

    let mut rendered = Vec::new();
    read_control_to_end(&mut client_stream, &mut rendered).await;
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");

    let rendered = String::from_utf8(rendered).expect("utf-8 control stream");
    let begin = parse_guard_line(&rendered, "%begin ")
        .expect("expected %begin guard for the command block");
    let end =
        parse_guard_line(&rendered, "%end ").expect("expected %end guard for the command block");
    assert_eq!(begin.command_number, end.command_number);
    assert_eq!(begin.flags, end.flags);
    assert!(
        begin.time_secs > 0,
        "begin timestamp must be populated: {begin:?}"
    );
    assert!(
        end.time_secs >= begin.time_secs,
        "end timestamp must be monotonic: {begin:?} -> {end:?}"
    );
    let last_line = rendered
        .lines()
        .last()
        .expect("control output is non-empty");
    assert_eq!(
        last_line, "%exit",
        "EOF after a command block must terminate with %exit: {rendered:?}"
    );
}

async fn read_control_to_end(client_stream: &mut UnixStream, output: &mut Vec<u8>) {
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, client_stream.read_to_end(output))
        .await
        .expect("control output drains before timeout")
        .expect("control output drains");
}

async fn wait_for_waiter(handler: &RequestHandler, channel: &str) {
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        loop {
            if handler.wait_for_counts(channel).0 == 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("wait-for waiter registers before timeout");
}

#[tokio::test]
async fn empty_line_input_emits_bare_exit_without_begin() {
    // Minimal control-mode scenario: a bare `\n` as the first input
    // byte must route through the in-loop empty-line branch and emit a
    // bare `%exit\n` with no preceding %begin/%end/%error guard tuple.
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        b"\n".to_vec(),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing: Arc::clone(&closing),
            shutdown_handle,
        },
    ));

    let mut rendered = Vec::new();
    client_stream
        .read_to_end(&mut rendered)
        .await
        .expect("control output drains");
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");

    assert_eq!(
        rendered, b"%exit\n",
        "empty-line input must emit bare %exit with no guard tuple"
    );
}

#[tokio::test]
async fn crlf_empty_line_also_emits_bare_exit() {
    // `extract_complete_control_lines` strips CR+LF as if it were LF,
    // so a bare CRLF must trip the empty-line exit path identically.
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        b"\r\n".to_vec(),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing: Arc::clone(&closing),
            shutdown_handle,
        },
    ));

    let mut rendered = Vec::new();
    client_stream
        .read_to_end(&mut rendered)
        .await
        .expect("control output drains");
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");

    assert_eq!(rendered, b"%exit\n");
}

#[tokio::test]
async fn incomplete_trailing_line_is_discarded_on_eof() {
    // control-mode contract: `extract_complete_control_lines` discards any
    // incomplete trailing line on EOF (tmux `evbuffer_readln` semantics).
    // The command-without-newline must not trigger a %begin, and the
    // transcript must still terminate in a bare `%exit\n`.
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        b"display-message -p hello".to_vec(),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing: Arc::clone(&closing),
            shutdown_handle,
        },
    ));

    client_stream
        .shutdown()
        .await
        .expect("client write half closes");

    let mut rendered = Vec::new();
    client_stream
        .read_to_end(&mut rendered)
        .await
        .expect("control output drains");
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");

    assert_eq!(
        rendered, b"%exit\n",
        "incomplete trailing line must be dropped and EOF must emit bare %exit"
    );
}

#[derive(Debug, Clone)]
struct TestGuardTuple {
    time_secs: i64,
    command_number: u64,
    flags: u8,
}

fn parse_guard_line(output: &str, prefix: &str) -> Option<TestGuardTuple> {
    let line = output.lines().find(|line| line.starts_with(prefix))?;
    let rest = line.strip_prefix(prefix)?;
    let mut parts = rest.split_whitespace();
    let time_secs = parts.next()?.parse::<i64>().ok()?;
    let command_number = parts.next()?.parse::<u64>().ok()?;
    let flags = parts.next()?.parse::<u8>().ok()?;
    Some(TestGuardTuple {
        time_secs,
        command_number,
        flags,
    })
}
