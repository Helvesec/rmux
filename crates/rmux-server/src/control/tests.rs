use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};

use super::subscriptions::{
    handle_pane_event, refresh_subscriptions, PaneEvent, PaneSubscriptionStart,
};
use super::{
    append_control_input, arm_control_eof_transition, control_control_waits_for_attached_session,
    drain_control_command_after_eof, drain_control_queue_after_eof, ensure_control_newline,
    extract_complete_control_lines, forward_control as forward_control_identity,
    install_control_eof_queue_lease_pause, wait_for_control_eof_transition, ActiveControlCommand,
    ControlCommandResult, ControlLifecycle, ControlModeUpgrade, ControlOutputQueue,
    ControlQueueEofCancellation, ControlServerEvent, ControlUpgradeInput, EofDrainContext,
    CONTROL_EOF_GRACE, CONTROL_SERVER_EVENT_CAPACITY, MAX_CONTROL_LINE_BYTES,
    MAX_QUEUED_CONTROL_LINES,
};
use crate::daemon::ShutdownHandle;
use crate::handler::{
    ControlClientIdentity, ControlQueueDrainLease, ControlRegistration, ControlRegistrationError,
    RequestHandler,
};
use crate::outer_terminal::OuterTerminalContext;
use crate::server_access::current_owner_uid;
use rmux_os::identity::UserIdentity;
use rmux_proto::{
    ControlMode, KillSessionRequest, NewSessionRequest, Request, Response, RmuxError, SessionId,
    SessionName, ShowBufferRequest, WaitForMode, WaitForRequest, WaitForResponse,
};

const CONTROL_TEST_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn only_control_control_eof_waits_for_an_attached_session() {
    let session_name = SessionName::new("control-eof-session").expect("valid session name");

    assert!(!control_control_waits_for_attached_session(
        ControlMode::Plain,
        Some(&session_name),
    ));
    assert!(!control_control_waits_for_attached_session(
        ControlMode::ControlControl,
        None,
    ));
    assert!(control_control_waits_for_attached_session(
        ControlMode::ControlControl,
        Some(&session_name),
    ));
}

#[tokio::test]
async fn persistent_eof_deadline_is_global_and_not_rearmed() {
    let mut transition = None;
    arm_control_eof_transition(&mut transition);
    let initial_deadline = transition
        .as_ref()
        .expect("EOF deadline is armed")
        .deadline();

    arm_control_eof_transition(&mut transition);
    assert_eq!(
        transition
            .as_ref()
            .expect("EOF deadline stays armed")
            .deadline(),
        initial_deadline,
        "starting another post-EOF frame must not extend the global budget"
    );

    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            wait_for_control_eof_transition(&mut transition),
        )
        .await
        .is_err(),
        "the deadline must leave a bounded grace for fast command output"
    );
    assert!(
        tokio::time::timeout(
            CONTROL_EOF_GRACE + Duration::from_millis(100),
            wait_for_control_eof_transition(&mut transition),
        )
        .await
        .is_ok(),
        "the persistent EOF deadline must still expire within its global budget"
    );
}

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
async fn pane_subscriptions_reject_a_recreated_same_name_session() {
    let handler = RequestHandler::new();
    let session_name =
        SessionName::new("control-subscription-identity").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    let replacement_output = handler
        .control_session_panes(&session_name)
        .await
        .expect("replacement session pane output exists")
        .into_iter()
        .next()
        .expect("replacement session has a pane")
        .1;

    let requester_pid = 42_421;
    let (event_tx, _event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    let control_identity = ControlClientIdentity::new(requester_pid, control_id);
    handler
        .set_control_subscription_identity_for_test(
            control_identity,
            session_name.clone(),
            SessionId::new(u32::MAX),
        )
        .await;

    let (pane_event_tx, mut pane_event_rx) = mpsc::channel(4);
    let mut subscriptions = std::collections::HashMap::new();
    refresh_subscriptions(
        &handler,
        control_identity,
        Some(&session_name),
        &mut subscriptions,
        pane_event_tx,
        PaneSubscriptionStart::Now,
    )
    .await;

    assert!(
        subscriptions.is_empty(),
        "a stale SessionId must not subscribe to a replacement sharing its name"
    );
    replacement_output.send(b"WRONG_SESSION_OUTPUT".to_vec());
    let received = tokio::time::timeout(Duration::from_millis(50), pane_event_rx.recv()).await;
    assert!(
        !matches!(received, Ok(Some(_))),
        "replacement output must not reach the stale control client"
    );
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
// Product divergence measured against tmux 3.7b: tmux drops queued work once
// control input reaches EOF. RMUX deliberately finishes non-blocking automation
// after closing the transport, while cancelling frames that would wait forever.
async fn eof_closes_transport_while_finite_control_queue_continues_product_divergence() {
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
    let command = format!(
        "run-shell 'sleep 1; printf done > {}'\nset-buffer -b eof-follow-on done\n",
        marker.display()
    );

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
    assert!(
        !control_task.is_finished(),
        "the server-side finite queue must remain alive after the transport closes"
    );

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
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, control_task)
        .await
        .expect("finite control queue completes before timeout")
        .expect("forward control task joins")
        .expect("forward control succeeds");
    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("eof-follow-on".to_owned()),
        }))
        .await;
    assert_eq!(
        response
            .command_output()
            .expect("follow-on set-buffer succeeds")
            .stdout(),
        b"done"
    );
    let _ = std::fs::remove_file(marker);
}

#[tokio::test]
async fn eof_preserves_active_if_shell_when_wait_is_only_in_unselected_branch_product_divergence() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard = handler.begin_detached_requester_access(4250, true);
    let input = b"if-shell -F 1 { run-shell 'sleep 1' ; set-buffer -b eof-active-finite-branch done } { wait-for eof-active-unselected-wait }\n";

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4250,
        ControlUpgradeInput::new(input.to_vec(), 1),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing,
            shutdown_handle,
        },
    ));

    let mut begin_prefix = vec![0_u8; 256];
    let bytes_read = client_stream
        .read(&mut begin_prefix)
        .await
        .expect("control output begins");
    assert!(
        String::from_utf8_lossy(&begin_prefix[..bytes_read]).contains("%begin "),
        "active frame emits its begin guard before EOF"
    );
    client_stream
        .shutdown()
        .await
        .expect("client write half closes");

    let mut rendered = Vec::new();
    tokio::time::timeout(
        Duration::from_millis(500),
        read_control_to_end(&mut client_stream, &mut rendered),
    )
    .await
    .expect("unselected wait does not retain the transport");
    assert!(
        !control_task.is_finished(),
        "the selected finite branch keeps draining after transport EOF"
    );
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, control_task)
        .await
        .expect("selected finite branch finishes before timeout")
        .expect("control task joins")
        .expect("control queue drains successfully");

    assert_eq!(
        handler.wait_for_counts("eof-active-unselected-wait"),
        (0, 0, false),
        "the unselected wait branch must never register"
    );
    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("eof-active-finite-branch".to_owned()),
        }))
        .await;
    assert_eq!(
        response
            .command_output()
            .expect("selected finite branch executes after EOF")
            .stdout(),
        b"done"
    );
}

#[tokio::test]
async fn eof_queued_if_shell_cancels_only_a_selected_wait_frame_product_divergence() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard = handler.begin_detached_requester_access(4251, true);
    let input = b"run-shell 'sleep 1'\nif-shell -F 1 { set-buffer -b eof-queued-finite-branch done } { wait-for eof-queued-unselected-wait }\nif-shell -F 1 { wait-for eof-queued-selected-wait ; set-buffer -b eof-queued-after-wait must-not-run } { set-buffer -b eof-queued-fallback must-not-run }\nset-buffer -b eof-queued-later-frame done\n";

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4251,
        ControlUpgradeInput::new(input.to_vec(), 1),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing,
            shutdown_handle,
        },
    ));

    client_stream
        .shutdown()
        .await
        .expect("client write half closes");
    let mut rendered = Vec::new();
    tokio::time::timeout(
        Duration::from_millis(500),
        read_control_to_end(&mut client_stream, &mut rendered),
    )
    .await
    .expect("queued wait branches do not retain the transport");
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, control_task)
        .await
        .expect("EOF queue drains before timeout")
        .expect("control task joins")
        .expect("queued frames drain independently");

    for (name, expected) in [
        ("eof-queued-finite-branch", b"done".as_slice()),
        ("eof-queued-later-frame", b"done".as_slice()),
    ] {
        let response = handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some(name.to_owned()),
            }))
            .await;
        assert_eq!(
            response
                .command_output()
                .unwrap_or_else(|| panic!("buffer {name} must exist"))
                .stdout(),
            expected
        );
    }
    for name in ["eof-queued-after-wait", "eof-queued-fallback"] {
        let response = handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some(name.to_owned()),
            }))
            .await;
        assert!(
            matches!(response, Response::Error(_)),
            "selected wait must stop its frame before buffer {name}: {response:?}"
        );
    }
    assert_eq!(
        handler.wait_for_counts("eof-queued-unselected-wait"),
        (0, 0, false)
    );
    assert_eq!(
        handler.wait_for_counts("eof-queued-selected-wait"),
        (0, 0, false)
    );
}

#[tokio::test]
async fn eof_queued_ready_wait_consumes_signal_and_finishes_its_frame() {
    let handler = Arc::new(RequestHandler::new());
    let channel = "eof-queued-ready-wait";
    let response = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: channel.to_owned(),
            mode: WaitForMode::Signal,
        }))
        .await;
    assert!(matches!(response, Response::WaitFor(WaitForResponse)));
    assert_eq!(handler.wait_for_counts(channel), (0, 0, true));

    drain_queued_frame_after_eof(
        &handler,
        4253,
        format!("wait-for {channel} ; set-buffer -b eof-after-ready-wait done"),
    )
    .await;

    assert_eq!(
        handler.wait_for_counts(channel),
        (0, 0, false),
        "the Ready wait must consume its pre-existing signal before EOF cancellation"
    );
    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("eof-after-ready-wait".to_owned()),
        }))
        .await;
    assert_eq!(
        response
            .command_output()
            .expect("Ready wait continues its queued frame")
            .stdout(),
        b"done"
    );
}

#[tokio::test]
async fn eof_queued_free_lock_acquires_and_finishes_its_frame() {
    let handler = Arc::new(RequestHandler::new());
    let channel = "eof-queued-ready-lock";
    assert_eq!(handler.wait_for_counts(channel), (0, 0, false));

    drain_queued_frame_after_eof(
        &handler,
        4254,
        format!("wait-for -L {channel} ; set-buffer -b eof-after-ready-lock done"),
    )
    .await;

    assert_eq!(
        handler.wait_for_counts(channel),
        (0, 0, true),
        "a free lock is Ready and must be acquired before EOF cancellation"
    );
    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("eof-after-ready-lock".to_owned()),
        }))
        .await;
    assert_eq!(
        response
            .command_output()
            .expect("Ready lock continues its queued frame")
            .stdout(),
        b"done"
    );

    let response = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: channel.to_owned(),
            mode: WaitForMode::Unlock,
        }))
        .await;
    assert!(matches!(response, Response::WaitFor(WaitForResponse)));
    assert_eq!(handler.wait_for_counts(channel), (0, 0, false));
}

#[tokio::test]
async fn eof_queue_skips_parse_errors_and_blocking_frames_before_later_finite_frame_product_divergence(
) {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let input = b"run-shell 'sleep 1'\ndisplay-message -p 'unterminated\nwait-for never-signalled\nset-buffer -b eof-after-skipped-frames done\n";

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4245,
        ControlUpgradeInput::new(input.to_vec(), 1),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing,
            shutdown_handle,
        },
    ));

    client_stream
        .shutdown()
        .await
        .expect("client write half closes");
    let mut rendered = Vec::new();
    tokio::time::timeout(
        Duration::from_millis(500),
        read_control_to_end(&mut client_stream, &mut rendered),
    )
    .await
    .expect("parse and wait-for frames must not retain the transport");
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, control_task)
        .await
        .expect("blocking wait-for frame is skipped after EOF")
        .expect("control task joins")
        .expect("queued parse errors stay local to their frame");

    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("eof-after-skipped-frames".to_owned()),
        }))
        .await;
    assert_eq!(
        response
            .command_output()
            .expect("later finite frame still executes")
            .stdout(),
        b"done"
    );
}

#[tokio::test]
async fn eof_queue_exit_event_stops_before_later_mutation_frame() {
    let handler = Arc::new(RequestHandler::new());
    let requester_pid = 4246;
    let (event_tx, mut event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            event_tx.clone(),
            Arc::clone(&closing),
        )
        .await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    closing.store(true, Ordering::SeqCst);
    assert_eq!(
        handler.begin_control_queue_drain(identity).await,
        ControlQueueDrainLease::Acquired,
        "exact control registration begins draining"
    );

    // Model a completed first frame that synchronously emitted Exit. Waiting
    // until both the event and JoinHandle are ready pins the select race: the
    // post-join event drain must still suppress frame two.
    let active_task = tokio::spawn(async move {
        event_tx
            .send(ControlServerEvent::Exit(None))
            .await
            .expect("control event receiver remains open");
        ControlCommandResult {
            stdout: Vec::new(),
            error: None,
            source_file_error: None,
            execution_error: None,
            exit_status: Some(0),
            server_shutdown_started: false,
        }
    });
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        while !active_task.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first frame finishes");

    let mut queued_lines =
        std::collections::VecDeque::from(["set-buffer -b eof-after-exit must-not-run".to_owned()]);
    let mut queued_bytes = queued_lines.iter().map(String::len).sum();
    let (_shutdown_tx, mut shutdown_rx) = watch::channel(());
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let mut drain_context = EofDrainContext {
        server_events: &mut event_rx,
        events_open: true,
        handler: &handler,
        control_identity: identity,
        shutdown: &mut shutdown_rx,
        shutdown_handle: &shutdown_handle,
    };
    drain_control_queue_after_eof(
        Some(active_task),
        &mut queued_lines,
        &mut queued_bytes,
        false,
        &mut drain_context,
    )
    .await
    .expect("EOF queue drains without transport");

    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("eof-after-exit".to_owned()),
        }))
        .await;
    assert!(
        matches!(response, Response::Error(_)),
        "an Exit from frame one must suppress frame two: {response:?}"
    );
    handler.finish_control(requester_pid, control_id).await;
}

#[tokio::test]
async fn eof_queue_rechecks_registration_after_active_exit_delivery_fails() {
    let handler = Arc::new(RequestHandler::new());
    let requester_pid = 4249;
    let (event_tx, mut event_rx) = mpsc::channel(1);
    let closing = Arc::new(AtomicBool::new(false));
    let control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            event_tx.clone(),
            Arc::clone(&closing),
        )
        .await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    assert_eq!(
        handler.begin_control_queue_drain(identity).await,
        ControlQueueDrainLease::Acquired
    );
    event_tx
        .try_send(ControlServerEvent::Notification(
            "%message saturated-before-exit".to_owned(),
        ))
        .expect("fill the control event channel");

    let handler_for_task = Arc::clone(&handler);
    let active_task = tokio::spawn(async move {
        let response = handler_for_task
            .handle(Request::DetachClientExt(
                rmux_proto::DetachClientExtRequest {
                    target_client: Some(requester_pid.to_string()),
                    all_other_clients: false,
                    target_session: None,
                    kill_on_detach: false,
                    exec_command: None,
                },
            ))
            .await;
        assert!(
            matches!(response, Response::DetachClient(_)),
            "{response:?}"
        );
        ControlCommandResult {
            stdout: Vec::new(),
            error: None,
            source_file_error: None,
            execution_error: None,
            exit_status: Some(0),
            server_shutdown_started: false,
        }
    });
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        while !active_task.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("active detach finishes while the event channel stays saturated");
    assert!(closing.load(Ordering::SeqCst));
    assert_eq!(
        handler.begin_control_queue_drain(identity).await,
        ControlQueueDrainLease::Unavailable,
        "failed Exit delivery removes the exact registration"
    );

    let (_shutdown_tx, mut shutdown_rx) = watch::channel(());
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let mut drain_context = EofDrainContext {
        server_events: &mut event_rx,
        events_open: true,
        handler: &handler,
        control_identity: identity,
        shutdown: &mut shutdown_rx,
        shutdown_handle: &shutdown_handle,
    };
    assert!(
        drain_control_command_after_eof(active_task, &mut drain_context)
            .await
            .expect("active EOF frame drains"),
        "a removed or closing registration is terminal even when Exit was never delivered"
    );
}

#[tokio::test]
async fn eof_after_deferred_exit_with_removed_registration_finishes_only_active_frame_product_divergence(
) {
    const EVENT_CAPACITY: usize = 8;

    let handler = Arc::new(RequestHandler::new());
    let requester_pid = 4248;
    let session_name =
        rmux_proto::SessionName::new("eof-deferred-exit-session").expect("valid session name");
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (event_tx, event_rx) = mpsc::channel(EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            event_tx.clone(),
            Arc::clone(&closing),
        )
        .await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard = handler.begin_detached_requester_access(requester_pid, true);
    let marker = std::env::temp_dir().join(format!(
        "rmux-control-eof-deferred-exit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos()
    ));
    let command = format!(
        "new-session -s {session_name}\nrun-shell 'printf started > {}; sleep 2; printf done >> {}'\nset-buffer -b eof-after-deferred-exit must-not-run\n",
        marker.display(),
        marker.display()
    );
    let handler_for_control = Arc::clone(&handler);
    let control_task = tokio::spawn(async move {
        forward_control_identity(
            server_stream,
            handler_for_control,
            identity,
            ControlUpgradeInput::new(command.into_bytes(), 1),
            shutdown_rx,
            event_rx,
            ControlLifecycle {
                closing,
                shutdown_handle,
            },
        )
        .await
    });

    tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        while !matches!(std::fs::read_to_string(&marker).as_deref(), Ok("started")) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("finite active command starts before the detach");
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        while event_tx.capacity() != EVENT_CAPACITY {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("startup control events drain before detach");

    let detached = handler
        .handle(Request::DetachClientExt(
            rmux_proto::DetachClientExtRequest {
                target_client: None,
                all_other_clients: false,
                target_session: Some(session_name),
                kill_on_detach: false,
                exec_command: None,
            },
        ))
        .await;
    assert!(
        matches!(detached, Response::DetachClient(_)),
        "{detached:?}"
    );
    assert_eq!(
        handler.begin_control_queue_drain(identity).await,
        ControlQueueDrainLease::Unavailable,
        "target-session detach removes the exact control registration"
    );

    // Fill through one event beyond channel capacity. Whether Exit was still
    // queued or had just been consumed, the last barrier cannot be accepted
    // until the forward loop has completed at least one later event turn.
    // Therefore Exit is in DeferredServerEvents, rather than merely waiting
    // in the receiver, before EOF is delivered.
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        for index in 0..=EVENT_CAPACITY {
            event_tx
                .send(ControlServerEvent::Notification(format!(
                    "%message deferred-exit-barrier-{index}"
                )))
                .await
                .expect("forward control still owns the event receiver");
        }
    })
    .await
    .expect("forward loop consumes Exit and a later barrier while the command is active");

    client_stream
        .shutdown()
        .await
        .expect("client write half closes");
    let mut rendered = Vec::new();
    tokio::time::timeout(
        Duration::from_millis(500),
        read_control_to_end(&mut client_stream, &mut rendered),
    )
    .await
    .expect("deferred Exit closes the transport before the active command finishes");
    assert!(
        !control_task.is_finished(),
        "the already-started finite command must finish after transport close"
    );
    let rendered = String::from_utf8(rendered).expect("utf-8 control transcript");
    assert!(
        rendered.contains("%end "),
        "EOF closes the active frame guard: {rendered:?}"
    );
    assert!(
        rendered.ends_with("%exit\n"),
        "the deferred Exit remains terminal: {rendered:?}"
    );

    tokio::time::timeout(CONTROL_TEST_TIMEOUT, control_task)
        .await
        .expect("active command finishes before timeout")
        .expect("forward control task joins")
        .expect("missing queue lease is not a product error");
    assert_eq!(
        std::fs::read_to_string(&marker).expect("read completed command marker"),
        "starteddone",
        "the finite command that was active at EOF must finish"
    );
    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("eof-after-deferred-exit".to_owned()),
        }))
        .await;
    assert!(
        matches!(response, Response::Error(_)),
        "a queued frame after deferred Exit must never run: {response:?}"
    );
    let _ = std::fs::remove_file(marker);
}

#[tokio::test]
async fn external_shutdown_cancels_finite_eof_queue_drain_product_divergence() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4247,
        ControlUpgradeInput::new(b"run-shell 'sleep 2'\n".to_vec(), 1),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing,
            shutdown_handle,
        },
    ));
    client_stream
        .shutdown()
        .await
        .expect("client write half closes");
    let mut rendered = Vec::new();
    tokio::time::timeout(
        Duration::from_millis(500),
        read_control_to_end(&mut client_stream, &mut rendered),
    )
    .await
    .expect("control transport closes before the finite frame completes");
    assert!(
        !control_task.is_finished(),
        "finite frame is still draining before external shutdown"
    );

    shutdown_tx.send_replace(());
    tokio::time::timeout(Duration::from_millis(500), control_task)
        .await
        .expect("external shutdown cancels the detached queue promptly")
        .expect("control task joins")
        .expect("shutdown cancellation is clean");
}

#[tokio::test]
async fn eof_drains_finite_queue_through_kill_server_product_divergence() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, shutdown_request_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle.clone());

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4243,
        ControlUpgradeInput::new(
            b"run-shell 'sleep 1' ; kill-server ; set-buffer -b eof-same-frame must-not-run\nset-buffer -b eof-next-frame must-not-run\n"
                .to_vec(),
            2,
        ),
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
    tokio::time::timeout(
        Duration::from_millis(500),
        read_control_to_end(&mut client_stream, &mut rendered),
    )
    .await
    .expect("control transport closes before the shell job finishes");
    assert!(
        !control_task.is_finished(),
        "kill-server must remain queued after transport EOF"
    );

    tokio::time::timeout(CONTROL_TEST_TIMEOUT, shutdown_request_rx)
        .await
        .expect("queued kill-server requests shutdown before timeout")
        .expect("shutdown request channel stays open");
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, control_task)
        .await
        .expect("finite control queue completes before timeout")
        .expect("forward control task joins")
        .expect("forward control succeeds");

    for buffer_name in ["eof-same-frame", "eof-next-frame"] {
        let response = handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some(buffer_name.to_owned()),
            }))
            .await;
        assert!(
            matches!(response, Response::Error(_)),
            "kill-server must suppress {buffer_name}: {response:?}"
        );
    }
}

#[tokio::test]
async fn eof_queue_lease_blocks_same_pid_registration_and_preserves_permissions_product_divergence()
{
    let handler = Arc::new(RequestHandler::new());
    let requester_pid = 4244;
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (old_event_tx, old_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let old_closing = Arc::new(AtomicBool::new(false));
    let old_control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            old_event_tx,
            Arc::clone(&old_closing),
        )
        .await;
    let old_identity = ControlClientIdentity::new(requester_pid, old_control_id);
    let eof_lease_pause = install_control_eof_queue_lease_pause(old_identity);
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let handler_for_control = Arc::clone(&handler);
    let control_task = tokio::spawn(async move {
        let result = forward_control_identity(
            server_stream,
            Arc::clone(&handler_for_control),
            old_identity,
            ControlUpgradeInput::new(
                b"run-shell 'sleep 1' ; set-buffer -b eof-old-identity old\n".to_vec(),
                1,
            ),
            shutdown_rx,
            old_event_rx,
            ControlLifecycle {
                closing: old_closing,
                shutdown_handle,
            },
        )
        .await;
        handler_for_control
            .finish_control(requester_pid, old_control_id)
            .await;
        result
    });

    client_stream
        .shutdown()
        .await
        .expect("client write half closes");
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, eof_lease_pause.reached.notified())
        .await
        .expect("EOF acquires the old queue lease before its next select turn");

    let (new_event_tx, _new_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let new_closing = Arc::new(AtomicBool::new(false));
    let handler_for_registration = Arc::clone(&handler);
    let registration_task = tokio::spawn(async move {
        handler_for_registration
            .register_control_with_access(
                requester_pid,
                ControlModeUpgrade {
                    initial_command_count: 0,
                    mode: ControlMode::Plain,
                    terminal_context: OuterTerminalContext::default(),
                },
                ControlRegistration {
                    event_tx: new_event_tx,
                    closing: new_closing,
                    uid: current_owner_uid(),
                    user: UserIdentity::Uid(current_owner_uid()),
                    can_write: false,
                },
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !registration_task.is_finished(),
        "same-PID registration must wait as soon as EOF is observed"
    );
    eof_lease_pause.release.notify_one();

    let mut rendered = Vec::new();
    tokio::time::timeout(
        Duration::from_millis(500),
        read_control_to_end(&mut client_stream, &mut rendered),
    )
    .await
    .expect("old control transport closes before its queue finishes");
    assert!(
        !control_task.is_finished(),
        "old control queue must still own its registration lease"
    );

    tokio::time::timeout(CONTROL_TEST_TIMEOUT, control_task)
        .await
        .expect("old finite queue completes before timeout")
        .expect("old control task joins")
        .expect("old control queue succeeds");
    let new_control_id = tokio::time::timeout(CONTROL_TEST_TIMEOUT, registration_task)
        .await
        .expect("new same-PID registration resumes after the old lease")
        .expect("new registration task joins")
        .expect("finite drain finishes within the registration deadline");
    assert_ne!(old_control_id, new_control_id);

    let old_buffer = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("eof-old-identity".to_owned()),
        }))
        .await;
    assert_eq!(
        old_buffer
            .command_output()
            .expect("old queue keeps its write permission")
            .stdout(),
        b"old"
    );

    let commands = handler
        .parse_control_commands("set-buffer -b eof-new-identity new")
        .await
        .expect("new control command parses");
    let denied = handler
        .execute_control_commands_identity(requester_pid, new_control_id, commands)
        .await;
    assert!(
        denied
            .error
            .as_ref()
            .is_some_and(|error| error.to_string().contains("read-only")),
        "new registration must use its own read-only permission: {denied:?}"
    );
    let new_buffer = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("eof-new-identity".to_owned()),
        }))
        .await;
    assert!(matches!(new_buffer, Response::Error(_)));
    handler.finish_control(requester_pid, new_control_id).await;
}

#[tokio::test]
async fn same_pid_registration_times_out_behind_a_stuck_eof_drain() {
    let handler = RequestHandler::new();
    let requester_pid = 42_441;
    let (old_event_tx, _old_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let old_control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            old_event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    let old_identity = ControlClientIdentity::new(requester_pid, old_control_id);
    assert_eq!(
        handler.begin_control_queue_drain(old_identity).await,
        ControlQueueDrainLease::Acquired
    );

    let (replacement_event_tx, _replacement_event_rx) =
        mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let error = handler
        .register_control_with_access_timeout_for_test(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            ControlRegistration {
                event_tx: replacement_event_tx,
                closing: Arc::new(AtomicBool::new(false)),
                uid: current_owner_uid(),
                user: UserIdentity::Uid(current_owner_uid()),
                can_write: true,
            },
            Duration::from_millis(25),
        )
        .await
        .expect_err("a stuck old drain must not retain a replacement forever");
    assert_eq!(
        error,
        ControlRegistrationError::QueueDrainTimedOut { requester_pid }
    );
    assert!(matches!(
        error.into_rmux_error(),
        RmuxError::Server(message)
            if message.contains("previous control queue")
                && message.contains(&requester_pid.to_string())
    ));
    assert!(
        handler.control_queue_identity_is_open(old_identity).await,
        "timing out the replacement must not cancel the old finite automation"
    );

    handler.finish_control(requester_pid, old_control_id).await;
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
async fn immediate_socket_eof_preserves_fast_attach_query_payloads_and_guards() {
    let handler = Arc::new(RequestHandler::new());
    let session_name =
        rmux_proto::SessionName::new("eof-fast-multi-frame").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(rmux_proto::NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");

    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4243,
        ControlUpgradeInput::new(Vec::new(), 0),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing,
            shutdown_handle,
        },
    ));

    let frames = format!(
        "attach-session -t {session_name}\nlist-clients -F '#{{client_flags}}'\ndisplay-message -p second\n"
    );
    client_stream
        .write_all(frames.as_bytes())
        .await
        .expect("all control frames write in one socket batch");
    client_stream
        .shutdown()
        .await
        .expect("client write half closes immediately after the frames");

    let mut rendered = Vec::new();
    read_control_to_end(&mut client_stream, &mut rendered).await;
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");

    let rendered = String::from_utf8(rendered).expect("utf-8 control stream");
    let payloads = rendered
        .lines()
        .filter(|line| *line == "attached,focused,control-mode" || *line == "second")
        .collect::<Vec<_>>();
    assert_eq!(
        payloads,
        vec!["attached,focused,control-mode", "second"],
        "every fast frame accepted before EOF keeps its payload: {rendered:?}"
    );

    let begins = parse_guard_lines(&rendered, "%begin ");
    let ends = parse_guard_lines(&rendered, "%end ");
    assert_eq!(begins.len(), 4, "ACK plus three frame guards: {rendered:?}");
    assert_eq!(ends.len(), 4, "ACK plus three frame guards: {rendered:?}");
    for (begin, end) in begins.iter().zip(&ends) {
        assert_eq!(begin.command_number, end.command_number, "{rendered:?}");
        assert_eq!(begin.flags, end.flags, "{rendered:?}");
    }
    assert_eq!(
        rendered
            .lines()
            .filter(|line| line.starts_with("%exit"))
            .count(),
        1,
        "EOF emits exactly one terminal exit line: {rendered:?}"
    );
    assert!(
        rendered.ends_with("%exit\n"),
        "EOF remains the final control record: {rendered:?}"
    );
}

#[tokio::test]
async fn control_control_eof_reconciles_ready_session_change_before_exit() {
    // tmux 3.7b keeps `-CC new-session` attached after terminal EOF and
    // delivers pane output before `%exit`. Hold the RMUX transport in its EOF
    // path until both the attach command result and SessionChangedAt are ready
    // so the biased-select ordering is deterministic.
    let handler = Arc::new(RequestHandler::new());
    let requester_pid = 42_430;
    let session_name =
        SessionName::new("control-control-eof-session-race").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
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
                mode: ControlMode::ControlControl,
                terminal_context: OuterTerminalContext::default(),
            },
            event_tx,
            Arc::clone(&closing),
        )
        .await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    let attach_pause = handler.install_created_session_control_attach_pause(session_name.clone());
    let eof_pause = install_control_eof_queue_lease_pause(identity);
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard = handler.begin_detached_requester_access(requester_pid, true);
    let handler_for_control = Arc::clone(&handler);
    let command =
        format!("new-session -A -s {session_name} ; set-buffer -b control-cc-race-ready done\n");
    let control_task = tokio::spawn(async move {
        let result = forward_control_identity(
            server_stream,
            Arc::clone(&handler_for_control),
            identity,
            ControlUpgradeInput::with_mode(command.into_bytes(), 1, ControlMode::ControlControl),
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

    tokio::time::timeout(CONTROL_TEST_TIMEOUT, attach_pause.reached.notified())
        .await
        .expect("attach command reaches the pre-commit pause");
    client_stream
        .shutdown()
        .await
        .expect("client write half closes");
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, eof_pause.reached.notified())
        .await
        .expect("forward loop observes EOF while attach is active");

    attach_pause.release.notify_one();
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        loop {
            let ready = handler
                .handle(Request::ShowBuffer(ShowBufferRequest {
                    name: Some("control-cc-race-ready".to_owned()),
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
    pane_output.send(b"CONTROL_CC_RACE_LIVE".to_vec());
    eof_pause.release.notify_one();

    let mut rendered = Vec::new();
    let saw_live_output = tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        let mut read_buffer = [0_u8; 1024];
        loop {
            let bytes_read = client_stream
                .read(&mut read_buffer)
                .await
                .expect("control output read succeeds");
            if bytes_read == 0 {
                return false;
            }
            rendered.extend_from_slice(&read_buffer[..bytes_read]);
            if rendered
                .windows(b"CONTROL_CC_RACE_LIVE".len())
                .any(|window| window == b"CONTROL_CC_RACE_LIVE")
            {
                return true;
            }
        }
    })
    .await
    .expect("control client produces live output or closes before timeout");
    assert!(
        saw_live_output,
        "ready SessionChangedAt must be reconciled before EOF exit: {:?}",
        String::from_utf8_lossy(&rendered)
    );

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    read_control_to_end(&mut client_stream, &mut rendered).await;
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");
    assert!(
        String::from_utf8_lossy(&rendered).contains("%exit"),
        "session teardown terminates the control client: {:?}",
        String::from_utf8_lossy(&rendered)
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
        ControlUpgradeInput::new(
            b"if-shell -F 1 { wait-for control-eof-block ; set-buffer -b eof-active-after-wait must-not-run } { set-buffer -b eof-active-fallback must-not-run }\n"
                .to_vec(),
            1,
        ),
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
    assert_eq!(
        handler.wait_for_counts("control-eof-block"),
        (0, 0, false),
        "EOF cancellation must remove the selected wait registration"
    );
    for name in ["eof-active-after-wait", "eof-active-fallback"] {
        let response = handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some(name.to_owned()),
            }))
            .await;
        assert!(
            matches!(response, Response::Error(_)),
            "selected wait cancellation must stop its frame before {name}: {response:?}"
        );
    }
}

#[tokio::test]
async fn eof_transition_is_not_starved_by_continuous_server_events() {
    let handler = Arc::new(RequestHandler::new());
    let requester_pid = 42_527;
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            event_tx.clone(),
            Arc::clone(&closing),
        )
        .await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard = handler.begin_detached_requester_access(requester_pid, true);
    let handler_for_control = Arc::clone(&handler);
    let control_task = tokio::spawn(async move {
        let result = forward_control_identity(
            server_stream,
            Arc::clone(&handler_for_control),
            identity,
            ControlUpgradeInput::new(b"wait-for eof-event-starvation\n".to_vec(), 1),
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
    wait_for_waiter(&handler, "eof-event-starvation").await;

    let producer =
        tokio::spawn(
            async move { while event_tx.send(ControlServerEvent::Refresh).await.is_ok() {} },
        );
    client_stream
        .shutdown()
        .await
        .expect("client write half closes");

    let mut rendered = Vec::new();
    tokio::time::timeout(
        Duration::from_millis(500),
        read_control_to_end(&mut client_stream, &mut rendered),
    )
    .await
    .expect("continuous server events cannot retain the EOF transport");
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, control_task)
        .await
        .expect("control task exits before timeout")
        .expect("control task joins")
        .expect("control EOF succeeds");
    producer.await.expect("event producer joins");

    assert_eq!(
        handler.wait_for_counts("eof-event-starvation"),
        (0, 0, false),
        "EOF cancellation removes the selected waiter"
    );
    let rendered = String::from_utf8(rendered).expect("control output is utf-8");
    assert!(
        rendered.contains("%end "),
        "active guard closes: {rendered:?}"
    );
    assert!(
        rendered.ends_with("%exit\n"),
        "transport exits: {rendered:?}"
    );

    let (replacement_tx, _replacement_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let replacement_id = tokio::time::timeout(
        Duration::from_millis(500),
        handler.register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            replacement_tx,
            Arc::new(AtomicBool::new(false)),
        ),
    )
    .await
    .expect("EOF releases the same-PID queue lease");
    assert_ne!(replacement_id, control_id);
    handler.finish_control(requester_pid, replacement_id).await;
}

#[tokio::test]
async fn eof_cancels_selected_lock_waiter_without_releasing_the_lock_owner() {
    let handler = Arc::new(RequestHandler::new());
    let lock_channel = "control-eof-lock-block";
    let response = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: lock_channel.to_owned(),
            mode: WaitForMode::Lock,
        }))
        .await;
    assert!(matches!(response, Response::WaitFor(WaitForResponse)));

    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let input =
        format!("wait-for -L {lock_channel} ; set-buffer -b eof-active-after-lock must-not-run\n");
    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4252,
        ControlUpgradeInput::new(input.into_bytes(), 1),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing,
            shutdown_handle,
        },
    ));

    wait_for_lock_waiter(&handler, lock_channel).await;
    client_stream
        .shutdown()
        .await
        .expect("client write half closes");
    let mut rendered = Vec::new();
    read_control_to_end(&mut client_stream, &mut rendered).await;
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, control_task)
        .await
        .expect("selected lock waiter cancels before timeout")
        .expect("control task joins")
        .expect("control queue drains successfully");

    assert_eq!(
        handler.wait_for_counts(lock_channel),
        (0, 0, true),
        "EOF removes only the queued lock waiter and preserves the current owner"
    );
    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("eof-active-after-lock".to_owned()),
        }))
        .await;
    assert!(
        matches!(response, Response::Error(_)),
        "selected lock cancellation must stop the rest of its frame: {response:?}"
    );

    let response = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: lock_channel.to_owned(),
            mode: WaitForMode::Unlock,
        }))
        .await;
    assert!(matches!(response, Response::WaitFor(WaitForResponse)));
    assert_eq!(handler.wait_for_counts(lock_channel), (0, 0, false));
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
        eof_cancellation: ControlQueueEofCancellation::new(ControlClientIdentity::new(4242, 1)),
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

async fn drain_queued_frame_after_eof(
    handler: &Arc<RequestHandler>,
    requester_pid: u32,
    line: String,
) {
    let (event_tx, mut event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            event_tx,
            closing,
        )
        .await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    assert_eq!(
        handler.begin_control_queue_drain(identity).await,
        ControlQueueDrainLease::Acquired
    );
    let mut queued_lines = std::collections::VecDeque::from([line]);
    let mut queued_bytes = queued_lines.iter().map(String::len).sum();
    let (_shutdown_tx, mut shutdown_rx) = watch::channel(());
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let mut context = EofDrainContext {
        server_events: &mut event_rx,
        events_open: true,
        handler,
        control_identity: identity,
        shutdown: &mut shutdown_rx,
        shutdown_handle: &shutdown_handle,
    };

    drain_control_queue_after_eof(
        None,
        &mut queued_lines,
        &mut queued_bytes,
        false,
        &mut context,
    )
    .await
    .expect("queued EOF frame drains");
    assert!(queued_lines.is_empty());
    assert_eq!(queued_bytes, 0);
    handler.finish_control(requester_pid, control_id).await;
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

async fn wait_for_lock_waiter(handler: &RequestHandler, channel: &str) {
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        loop {
            if handler.wait_for_counts(channel).1 == 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("wait-for lock waiter registers before timeout");
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
