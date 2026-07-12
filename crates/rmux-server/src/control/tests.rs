use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};

use super::subscriptions::{handle_pane_event, PaneEvent};
use super::{
    append_control_input, ensure_control_newline, extract_complete_control_lines,
    forward_control as forward_control_identity, ActiveControlCommand, ControlCommandResult,
    ControlLifecycle, ControlModeUpgrade, ControlOutputQueue, ControlServerEvent,
    ControlUpgradeInput, CONTROL_SERVER_EVENT_CAPACITY, MAX_CONTROL_LINE_BYTES,
    MAX_QUEUED_CONTROL_LINES,
};
use crate::daemon::ShutdownHandle;
use crate::handler::{ControlClientIdentity, RequestHandler};
use crate::outer_terminal::OuterTerminalContext;
use rmux_proto::{ControlMode, Request, Response, WaitForMode, WaitForRequest, WaitForResponse};

const CONTROL_TEST_TIMEOUT: Duration = Duration::from_secs(5);

async fn forward_control(
    stream: UnixStream,
    handler: Arc<RequestHandler>,
    requester_pid: u32,
    upgrade_input: ControlUpgradeInput,
    shutdown: watch::Receiver<()>,
    server_events: mpsc::Receiver<ControlServerEvent>,
    lifecycle: ControlLifecycle,
) -> std::io::Result<()> {
    let (registration_tx, _registration_rx) =
        mpsc::channel::<ControlServerEvent>(CONTROL_SERVER_EVENT_CAPACITY);
    let control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            registration_tx,
            Arc::clone(&lifecycle.closing),
        )
        .await;
    let result = forward_control_identity(
        stream,
        Arc::clone(&handler),
        ControlClientIdentity::new(requester_pid, control_id),
        upgrade_input,
        shutdown,
        server_events,
        lifecycle,
    )
    .await;
    handler.finish_control(requester_pid, control_id).await;
    result
}

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
fn control_input_rejects_unterminated_oversize_lines() {
    let mut input_buffer = Vec::new();
    let mut queued_lines = std::collections::VecDeque::new();
    let mut queued_bytes = 0;
    let oversized = vec![b'x'; MAX_CONTROL_LINE_BYTES + 1];

    let error = append_control_input(
        &mut input_buffer,
        &mut queued_lines,
        &mut queued_bytes,
        &oversized,
    )
    .expect_err("unterminated oversized input must be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn control_input_rejects_excessive_queued_lines() {
    let mut input_buffer = Vec::new();
    let mut queued_lines = std::collections::VecDeque::new();
    let mut queued_bytes = 0;
    let input = "x\n".repeat(MAX_QUEUED_CONTROL_LINES + 1);

    let error = append_control_input(
        &mut input_buffer,
        &mut queued_lines,
        &mut queued_bytes,
        input.as_bytes(),
    )
    .expect_err("an excessive command backlog must be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
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
async fn pane_output_lag_terminates_control_mode_explicitly() {
    let mut queue = ControlOutputQueue::default();
    let mut paused_panes = std::collections::HashSet::new();
    let lagged = handle_pane_event(
        PaneEvent::Lagged {
            pane_id: 7,
            expected_sequence: 2,
            resume_sequence: 9,
            missed_events: 7,
        },
        &mut queue,
        &mut paused_panes,
        Default::default(),
    )
    .expect("lag handling succeeds");
    assert!(
        lagged,
        "a pane-output gap must be terminal for control mode"
    );

    let (mut writer, mut reader) = tokio::io::duplex(256);
    super::flush_output_queue(
        &mut queue,
        &mut writer,
        Default::default(),
        &mut paused_panes,
    )
    .await
    .expect("terminal lag frame flushes");
    writer.shutdown().await.expect("writer closes");
    let mut rendered = Vec::new();
    reader
        .read_to_end(&mut rendered)
        .await
        .expect("lag transcript reads");
    assert_eq!(rendered, b"%exit too far behind\n");
}

#[tokio::test]
async fn notifications_wait_until_after_the_active_command_block() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard = handler.begin_detached_requester_access(4242, true);

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        ControlUpgradeInput::new(b"wait-for control-test-block\n\n".to_vec(), 1),
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
        .await
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
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        ControlUpgradeInput::new(Vec::new(), 0),
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

    assert_initial_control_frame_then_exit(&rendered);
}

#[tokio::test]
async fn eof_after_command_block_appends_exit() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        ControlUpgradeInput::new(b"display-message -p ok\n".to_vec(), 1),
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
    let begin = parse_guard_lines(&rendered, "%begin ")
        .pop()
        .expect("expected %begin guard for the command block");
    let end = parse_guard_lines(&rendered, "%end ")
        .pop()
        .expect("expected %end guard for the command block");
    assert_eq!(begin.command_number, end.command_number);
    assert_eq!(begin.flags, end.flags);
    assert_eq!(begin.command_number, 1);
    assert_eq!(begin.flags, 0);
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

#[tokio::test]
async fn eof_detaches_finite_control_command_and_closes_immediately() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard = handler.begin_detached_requester_access(4242, true);
    let marker = std::env::temp_dir().join(format!(
        "rmux-control-eof-detached-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos()
    ));
    let command = format!("run-shell 'sleep 1; printf done > {}'\n", marker.display());

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        ControlUpgradeInput::new(command.into_bytes(), 1),
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

    client_stream
        .shutdown()
        .await
        .expect("client write half closes");

    let mut remaining = Vec::new();
    tokio::time::timeout(
        Duration::from_millis(500),
        read_control_to_end(&mut client_stream, &mut remaining),
    )
    .await
    .expect("control EOF must not wait for the foreground shell job");
    tokio::time::timeout(Duration::from_millis(500), control_task)
        .await
        .expect("forward control exits before the shell job")
        .expect("forward control task joins")
        .expect("forward control succeeds");

    let rendered = format!(
        "{begin_prefix}{}",
        String::from_utf8(remaining).expect("utf-8 control stream")
    );
    assert!(
        rendered.contains("%end "),
        "EOF must close the pending command guard: {rendered:?}"
    );
    assert!(
        !rendered.contains("%error "),
        "finite pending command must not be converted to %error after EOF: {rendered:?}"
    );
    assert!(
        rendered.ends_with("%exit\n"),
        "EOF must terminate control mode immediately: {rendered:?}"
    );

    tokio::time::timeout(Duration::from_secs(3), async {
        while !marker.is_file() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("detached foreground shell job still completes server-side");
    assert_eq!(
        std::fs::read_to_string(&marker).expect("read detached shell marker"),
        "done"
    );
    let _ = std::fs::remove_file(marker);
}

#[tokio::test]
async fn stdin_command_after_upgrade_uses_flags_one_after_initial_ack() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        ControlUpgradeInput::new(Vec::new(), 0),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing: Arc::clone(&closing),
            shutdown_handle,
        },
    ));

    client_stream
        .write_all(b"display-message -p ok\n")
        .await
        .expect("stdin command writes");
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
    let begins = parse_guard_lines(&rendered, "%begin ");
    let ends = parse_guard_lines(&rendered, "%end ");
    assert_eq!(
        begins.len(),
        2,
        "expected ack plus stdin block: {rendered:?}"
    );
    assert_eq!(ends.len(), 2, "expected ack plus stdin block: {rendered:?}");
    assert_eq!(begins[0].command_number, 1);
    assert_eq!(begins[0].flags, 0);
    assert_eq!(begins[1].command_number, 2);
    assert_eq!(begins[1].flags, 1);
    assert_eq!(ends[1].command_number, begins[1].command_number);
    assert_eq!(ends[1].flags, begins[1].flags);
    assert!(
        rendered.contains("ok\n"),
        "stdin command output should be present: {rendered:?}"
    );
    assert!(
        rendered.ends_with("%exit\n"),
        "EOF after stdin command must terminate with %exit: {rendered:?}"
    );
}

#[tokio::test]
async fn fragmented_argv_command_stays_initial_without_synthetic_ack() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        ControlUpgradeInput::new(Vec::new(), 1),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing: Arc::clone(&closing),
            shutdown_handle,
        },
    ));

    for fragment in [b"display-message -p ".as_slice(), b"initial", b"\n"] {
        client_stream
            .write_all(fragment)
            .await
            .expect("fragment writes");
        tokio::task::yield_now().await;
    }
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
    let begins = parse_guard_lines(&rendered, "%begin ");
    let ends = parse_guard_lines(&rendered, "%end ");
    assert_eq!(begins.len(), 1, "no empty ACK is allowed: {rendered:?}");
    assert_eq!(ends.len(), 1, "no empty ACK is allowed: {rendered:?}");
    assert_eq!(begins[0].command_number, 1);
    assert_eq!(begins[0].flags, 0);
    assert_eq!(ends[0].command_number, 1);
    assert_eq!(ends[0].flags, 0);
    assert!(rendered.contains("initial\n"), "{rendered:?}");
}

#[tokio::test]
async fn command_with_more_than_one_thousand_arguments_errors() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let mut input = String::from("display-message");
    for index in 0..1001 {
        input.push_str(" arg");
        input.push_str(&index.to_string());
    }
    input.push_str("\n\n");

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        ControlUpgradeInput::new(input.into_bytes(), 1),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing: Arc::clone(&closing),
            shutdown_handle,
        },
    ));

    let mut rendered = Vec::new();
    read_control_to_end(&mut client_stream, &mut rendered).await;
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");

    let rendered = String::from_utf8(rendered).expect("utf-8 control stream");
    assert!(
        rendered.contains("too many arguments: 1001 (maximum 1000)"),
        "oversized MSG_COMMAND should report the argument cap: {rendered:?}"
    );
    assert!(
        rendered.contains("%error "),
        "oversized MSG_COMMAND should close the block with %error: {rendered:?}"
    );
    assert!(
        !rendered
            .lines()
            .any(|line| line.starts_with("%end ") && line.ends_with(" 1")),
        "oversized MSG_COMMAND must not close the user block with %end: {rendered:?}"
    );
    assert!(
        rendered.ends_with("%exit\n"),
        "empty trailing line should still close control mode: {rendered:?}"
    );
}

#[tokio::test]
async fn nested_command_with_more_than_one_thousand_arguments_errors() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let mut input = String::from("bind-key x { display-message");
    for index in 0..1001 {
        input.push_str(" arg");
        input.push_str(&index.to_string());
    }
    input.push_str(" }\n\n");

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        ControlUpgradeInput::new(input.into_bytes(), 1),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing: Arc::clone(&closing),
            shutdown_handle,
        },
    ));

    let mut rendered = Vec::new();
    read_control_to_end(&mut client_stream, &mut rendered).await;
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");

    let rendered = String::from_utf8(rendered).expect("utf-8 control stream");
    assert!(
        rendered.contains("too many arguments: 1001 (maximum 1000)"),
        "oversized nested command should report the argument cap: {rendered:?}"
    );
    assert!(
        rendered.contains("%error "),
        "oversized nested command should close the block with %error: {rendered:?}"
    );
    assert!(
        !rendered
            .lines()
            .any(|line| line.starts_with("%end ") && line.ends_with(" 1")),
        "oversized nested command must not close the user block with %end: {rendered:?}"
    );
    assert!(
        rendered.ends_with("%exit\n"),
        "empty trailing line should still close control mode: {rendered:?}"
    );
}

#[tokio::test]
async fn pending_control_command_waits_for_completion_without_execution_timeout() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard = handler.begin_detached_requester_access(4242, true);

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        ControlUpgradeInput::new(b"wait-for control-timeout-block\n\n".to_vec(), 1),
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

    wait_for_waiter(&handler, "control-timeout-block").await;
    tokio::time::sleep(Duration::from_millis(650)).await;
    let response = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: "control-timeout-block".to_owned(),
            mode: WaitForMode::Signal,
        }))
        .await;
    assert!(matches!(response, Response::WaitFor(WaitForResponse)));

    let mut rendered = Vec::new();
    read_control_to_end(&mut client_stream, &mut rendered).await;
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");

    let rendered = format!(
        "{begin_prefix}{}",
        String::from_utf8(rendered).expect("utf-8 control stream")
    );
    assert!(
        !rendered.contains("command timed out after"),
        "control-mode must not cap command execution at 500ms: {rendered:?}"
    );
    assert!(
        rendered.contains("%end "),
        "signalled pending control command should close successfully: {rendered:?}"
    );
    assert!(
        !rendered.contains("%error "),
        "signalled pending control command must not emit %error: {rendered:?}"
    );
    assert!(
        rendered.ends_with("%exit\n"),
        "empty trailing line should close control mode after command completion: {rendered:?}"
    );
}

#[tokio::test]
async fn eof_while_control_command_is_pending_closes_guard_and_exits() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard = handler.begin_detached_requester_access(4242, true);

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        ControlUpgradeInput::new(b"wait-for control-eof-block\n".to_vec(), 1),
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
    wait_for_waiter(&handler, "control-eof-block").await;

    client_stream
        .shutdown()
        .await
        .expect("client write half closes");

    let mut remaining = Vec::new();
    read_control_to_end(&mut client_stream, &mut remaining).await;
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");

    let rendered = format!(
        "{begin_prefix}{}",
        String::from_utf8(remaining).expect("utf-8 control stream")
    );
    assert!(
        rendered.contains("%end "),
        "EOF while a command is pending must close the guard: {rendered:?}"
    );
    assert!(
        !rendered.contains("%error "),
        "EOF cancellation should be a clean end guard: {rendered:?}"
    );
    assert!(
        rendered.ends_with("%exit\n"),
        "EOF while a command is pending must terminate control mode: {rendered:?}"
    );
}

#[tokio::test]
async fn dropping_active_control_command_aborts_inflight_task() {
    struct DropProbe(Arc<AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let started = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let task_started = Arc::clone(&started);
    let task_dropped = Arc::clone(&dropped);
    let task = tokio::spawn(async move {
        let _probe = DropProbe(task_dropped);
        task_started.store(true, Ordering::SeqCst);
        std::future::pending::<ControlCommandResult>().await
    });

    while !started.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }

    drop(ActiveControlCommand {
        timestamp: 0,
        command_number: 1,
        guard_flag: 0,
        abort_on_eof: true,
        task: Some(task),
    });

    for _ in 0..50 {
        if dropped.load(Ordering::SeqCst) {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("dropping an in-flight control command must abort its task");
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
async fn empty_line_input_emits_initial_frame_and_bare_exit() {
    // Minimal control-mode scenario: a bare `\n` as the first input byte must
    // route through the in-loop empty-line branch after the initial tmux-style
    // control guard pair, then terminate with a bare `%exit\n`.
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        ControlUpgradeInput::new(b"\n".to_vec(), 0),
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

    assert_initial_control_frame_then_exit(&rendered);
}

#[tokio::test]
async fn crlf_empty_line_also_emits_bare_exit() {
    // `extract_complete_control_lines` strips CR+LF as if it were LF,
    // so a bare CRLF must trip the empty-line exit path identically.
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        ControlUpgradeInput::new(b"\r\n".to_vec(), 0),
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

    assert_initial_control_frame_then_exit(&rendered);
}

#[tokio::test]
async fn incomplete_trailing_line_is_discarded_on_eof() {
    // control-mode contract: `extract_complete_control_lines` discards any
    // incomplete trailing line on EOF (tmux `evbuffer_readln` semantics).
    // The command-without-newline must not trigger a user-command %begin, and
    // the transcript must still terminate in a bare `%exit\n`.
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4242,
        ControlUpgradeInput::new(b"display-message -p hello".to_vec(), 0),
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

    assert_initial_control_frame_then_exit(&rendered);
}

fn assert_initial_control_frame_then_exit(rendered: &[u8]) {
    let rendered = String::from_utf8(rendered.to_vec()).expect("utf-8 control stream");
    let begins = parse_guard_lines(&rendered, "%begin ");
    let ends = parse_guard_lines(&rendered, "%end ");
    assert_eq!(
        begins.len(),
        1,
        "empty/discarded input must emit only the initial %begin: {rendered:?}"
    );
    assert_eq!(
        ends.len(),
        1,
        "empty/discarded input must emit only the initial %end: {rendered:?}"
    );
    assert_eq!(begins[0].command_number, 1);
    assert_eq!(begins[0].flags, 0);
    assert_eq!(ends[0].command_number, 1);
    assert_eq!(ends[0].flags, 0);
    assert!(
        !rendered.contains("%error "),
        "empty/discarded input must not emit %error: {rendered:?}"
    );
    assert!(
        rendered.ends_with("%exit\n"),
        "control stream must end with bare %exit: {rendered:?}"
    );
}

#[derive(Debug, Clone)]
struct TestGuardTuple {
    time_secs: i64,
    command_number: u64,
    flags: u8,
}

fn parse_guard_lines(output: &str, prefix: &str) -> Vec<TestGuardTuple> {
    output
        .lines()
        .filter_map(|line| parse_guard_tuple(line, prefix))
        .collect()
}

fn parse_guard_tuple(line: &str, prefix: &str) -> Option<TestGuardTuple> {
    if !line.starts_with(prefix) {
        return None;
    }
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
