use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};

use super::subscriptions::{
    handle_pane_event, refresh_subscriptions, PaneEvent, PaneSubscriptionStart,
    PaneSubscriptionState,
};
use super::{
    append_control_input, arm_control_eof_transition, control_commands_require_drain,
    control_control_waits_for_attached_session, drain_control_command_after_eof,
    drain_control_queue_after_eof, ensure_control_newline, extract_complete_control_lines,
    forward_control as forward_control_identity, install_control_eof_queue_lease_pause,
    install_control_transport_drain_pause, pause_after_control_eof_queue_lease,
    wait_for_control_eof_transition, ActiveControlCommand, ControlClientFlags,
    ControlCommandOrigin, ControlCommandResult, ControlLifecycle, ControlModeUpgrade,
    ControlOutputQueue, ControlPaneOutputStates, ControlQueueEofCancellation, ControlServerEvent,
    ControlSessionAttachment, ControlUpgradeInput, EofDrainContext, CONTROL_EOF_GRACE,
    CONTROL_SERVER_EVENT_CAPACITY, MAX_CONTROL_LINE_BYTES, MAX_QUEUED_CONTROL_LINES,
};
use crate::daemon::ShutdownHandle;
use crate::handler::{
    ControlClientIdentity, ControlQueueDrainLease, ControlRegistration, ControlRegistrationError,
    RequestHandler,
};
use crate::outer_terminal::OuterTerminalContext;
use crate::server_access::{current_owner_uid, AccessMode};
use rmux_os::identity::UserIdentity;
use rmux_proto::{
    ControlMode, KillSessionRequest, NewSessionRequest, OptionName, Request, Response, RmuxError,
    ScopeSelector, SessionId, SessionName, SetOptionMode, SetOptionRequest, ShowBufferRequest,
    WaitForMode, WaitForRequest, WaitForResponse,
};

const CONTROL_TEST_TIMEOUT: Duration = Duration::from_secs(5);

#[path = "tests/pane_read_demand.rs"]
mod pane_read_demand;

#[path = "tests/resume_race.rs"]
mod resume_race;

#[path = "tests/output_drain.rs"]
mod output_drain;

mod stdout_bound;

mod stdout_bound_neighbors;

#[path = "tests/eof_response_progress.rs"]
mod eof_response_progress;

#[path = "tests/attach_notice_order.rs"]
mod attach_notice_order;

#[tokio::test]
async fn only_blocking_control_waits_are_cancel_safe_during_shutdown() {
    let handler = RequestHandler::new();
    for line in [
        "wait-for channel",
        "wait-for -- -channel",
        "wait-for -L lock",
    ] {
        let commands = handler
            .parse_control_commands(line)
            .await
            .expect("cancel-safe wait parses");
        assert!(
            !control_commands_require_drain(&commands),
            "{line:?} must not hold the mutation drain barrier"
        );
    }
    for line in [
        "wait-for -S channel",
        "wait-for -U lock",
        "set-buffer changed",
        "RMUX_TEST=value",
    ] {
        let commands = handler
            .parse_control_commands(line)
            .await
            .expect("mutating command parses");
        assert!(
            control_commands_require_drain(&commands),
            "{line:?} must hold the mutation drain barrier"
        );
    }
}

#[tokio::test]
async fn eof_queue_lease_pauses_are_scoped_by_handler_and_cleaned_on_drop() {
    let identity = ControlClientIdentity::new(81_001, 1);
    let first_handler = Arc::new(RequestHandler::new());
    let second_handler = Arc::new(RequestHandler::new());
    let abandoned_pause = install_control_eof_queue_lease_pause(&first_handler, identity);
    drop(abandoned_pause);
    let second_pause = install_control_eof_queue_lease_pause(&second_handler, identity);
    let first_pause = install_control_eof_queue_lease_pause(&first_handler, identity);

    let first_handler_for_task = Arc::clone(&first_handler);
    let first_task = tokio::spawn(async move {
        pause_after_control_eof_queue_lease(&first_handler_for_task, identity).await;
    });
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, first_pause.reached.notified())
        .await
        .expect("first handler reaches its EOF lease pause");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), second_pause.reached.notified())
            .await
            .is_err(),
        "the first handler must not consume the second handler's pause"
    );
    first_pause.release.notify_one();
    first_task.await.expect("first EOF lease pause joins");

    let second_handler_for_task = Arc::clone(&second_handler);
    let second_task = tokio::spawn(async move {
        pause_after_control_eof_queue_lease(&second_handler_for_task, identity).await;
    });
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, second_pause.reached.notified())
        .await
        .expect("second handler reaches its EOF lease pause");

    second_pause.release.notify_one();
    second_task.await.expect("second EOF lease pause joins");
}

#[test]
fn only_control_control_eof_waits_for_an_attached_session() {
    let session_name = SessionName::new("control-eof-session").expect("valid session name");
    let unattached = ControlSessionAttachment::new(None);
    let attached = ControlSessionAttachment::new(Some(session_name));

    assert!(!control_control_waits_for_attached_session(
        ControlMode::Plain,
        &attached,
    ));
    assert!(!control_control_waits_for_attached_session(
        ControlMode::ControlControl,
        &unattached,
    ));
    assert!(control_control_waits_for_attached_session(
        ControlMode::ControlControl,
        &attached,
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

#[tokio::test]
async fn shutdown_quiesce_finishes_the_active_control_mutation_and_rejects_later_frames() {
    const REQUESTER_PID: u32 = 42_422;

    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let control_id = handler
        .register_control_with_closing(
            REQUESTER_PID,
            ControlModeUpgrade {
                initial_command_count: 1,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            server_event_tx,
            Arc::clone(&closing),
        )
        .await;
    let identity = ControlClientIdentity::new(REQUESTER_PID, control_id);
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard =
        handler.begin_test_detached_requester_access(REQUESTER_PID, AccessMode::ReadWrite);
    let marker = std::env::temp_dir().join(format!(
        "rmux-control-quiesce-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos()
    ));
    let input = format!(
        "run-shell 'printf started > {}; sleep 0.4' ; set-buffer -b shutdown-control-active committed\n\
         set-buffer -b shutdown-control-later must-not-run\n",
        marker.display()
    );
    let handler_for_control = Arc::clone(&handler);
    let control_task = tokio::spawn(async move {
        forward_control_identity(
            server_stream,
            handler_for_control,
            identity,
            ControlUpgradeInput::new(input.into_bytes(), 1),
            shutdown_rx,
            server_event_rx,
            ControlLifecycle {
                closing,
                shutdown_handle,
            },
        )
        .await
    });

    tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("active control frame reaches its foreground shell");
    assert!(!handler.normal_drain_requests_quiesced());

    handler.close_normal_request_admission();
    shutdown_tx.send_replace(());
    let mut rendered = Vec::new();
    read_control_to_end(&mut client_stream, &mut rendered).await;
    control_task
        .await
        .expect("control task joins")
        .expect("control forwarding succeeds");
    assert!(handler.normal_drain_requests_quiesced());

    let active = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("shutdown-control-active".to_owned()),
        }))
        .await;
    assert_eq!(
        active
            .command_output()
            .expect("the admitted active frame commits")
            .stdout(),
        b"committed"
    );
    let later = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("shutdown-control-later".to_owned()),
        }))
        .await;
    assert!(
        matches!(later, Response::Error(_)),
        "a later frame must not be admitted during quiesce: {later:?}"
    );
    let rendered = String::from_utf8(rendered).expect("control transcript is utf-8");
    assert!(rendered.contains("%end "), "{rendered:?}");
    assert!(rendered.ends_with("\n%exit\n"), "{rendered:?}");

    handler.finish_control(REQUESTER_PID, control_id).await;
    let _ = std::fs::remove_file(marker);
}

#[tokio::test]
async fn eof_queue_rechecks_normal_request_admission_before_spawning_each_frame() {
    let handler = Arc::new(RequestHandler::new());
    let requester_pid = 4252;
    let (event_tx, mut event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
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
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    assert_eq!(
        handler.begin_control_queue_drain(identity).await,
        ControlQueueDrainLease::Acquired
    );

    handler.close_normal_request_admission();
    let mut queued_lines = std::collections::VecDeque::from([
        "set-buffer -b eof-admission-after-close must-not-run".to_owned(),
    ]);
    let mut queued_bytes = queued_lines.iter().map(String::len).sum();
    let (_shutdown_tx, mut shutdown_rx) = watch::channel(());
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let mut context = EofDrainContext {
        server_events: &mut event_rx,
        events_open: true,
        handler: &handler,
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
    .expect("closed EOF queue stops without spawning its next frame");

    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("eof-admission-after-close".to_owned()),
        }))
        .await;
    assert!(
        matches!(response, Response::Error(_)),
        "a frame rejected by normal admission must not mutate: {response:?}"
    );
    handler.finish_control(requester_pid, control_id).await;
}

#[tokio::test]
async fn live_kill_server_stops_buffered_frames_before_shutdown_watch_propagates() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, shutdown_request_rx) = ShutdownHandle::new();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4248,
        ControlUpgradeInput::new(
            b"kill-server\nset-buffer -b live-after-kill must-not-run\n".to_vec(),
            2,
        ),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing,
            shutdown_handle,
        },
    ));

    tokio::time::timeout(CONTROL_TEST_TIMEOUT, shutdown_request_rx)
        .await
        .expect("kill-server requests shutdown before timeout")
        .expect("shutdown request channel stays open");
    let mut rendered = Vec::new();
    read_control_to_end(&mut client_stream, &mut rendered).await;
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, control_task)
        .await
        .expect("forward control exits before timeout")
        .expect("forward control task joins")
        .expect("forward control succeeds");

    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("live-after-kill".to_owned()),
        }))
        .await;
    assert!(
        matches!(response, Response::Error(_)),
        "a live frame buffered behind kill-server must never be admitted: {response:?}"
    );
    let rendered = String::from_utf8(rendered).expect("control transcript is utf-8");
    assert!(rendered.ends_with("\n%exit\n"), "{rendered:?}");
}

#[tokio::test]
async fn shutdown_cancels_only_the_explicit_control_wait() {
    const REQUESTER_PID: u32 = 42_421;
    const WAIT_CHANNEL: &str = "control-shutdown-active";

    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let control_id = handler
        .register_control_with_closing(
            REQUESTER_PID,
            ControlModeUpgrade {
                initial_command_count: 1,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            server_event_tx,
            Arc::clone(&closing),
        )
        .await;
    let identity = ControlClientIdentity::new(REQUESTER_PID, control_id);
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard =
        handler.begin_test_detached_requester_access(REQUESTER_PID, AccessMode::ReadWrite);
    let input = format!(
        "wait-for {WAIT_CHANNEL}\n\
         set-buffer -b shutdown-active-later must-not-run\n"
    );
    let handler_for_control = Arc::clone(&handler);
    let control_task = tokio::spawn(async move {
        forward_control_identity(
            server_stream,
            handler_for_control,
            identity,
            ControlUpgradeInput::new(input.into_bytes(), 1),
            shutdown_rx,
            server_event_rx,
            ControlLifecycle {
                closing,
                shutdown_handle,
            },
        )
        .await
    });

    wait_for_waiter(&handler, WAIT_CHANNEL).await;
    assert!(
        handler.normal_drain_requests_quiesced(),
        "a pure wait-for frame must not hold the mutation barrier"
    );
    assert!(!handler.normal_requests_quiesced());
    handler.close_normal_request_admission();
    shutdown_tx.send_replace(());
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        while !handler.normal_requests_quiesced() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the selected wait cancels during shutdown");

    let mut rendered = Vec::new();
    read_control_to_end(&mut client_stream, &mut rendered).await;
    control_task
        .await
        .expect("control task joins")
        .expect("control forwarding succeeds");
    let rendered = String::from_utf8(rendered).expect("control transcript is utf-8");
    assert!(rendered.contains("%end "), "{rendered:?}");
    assert!(rendered.ends_with("\n%exit\n"), "{rendered:?}");

    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("shutdown-active-later".to_owned()),
        }))
        .await;
    assert!(
        matches!(response, Response::Error(_)),
        "shutdown must suppress the later frame: {response:?}"
    );
    handler.finish_control(REQUESTER_PID, control_id).await;
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

#[test]
fn control_client_flags_apply_and_remove_supported_values() {
    let mut flags = ControlClientFlags::default();
    flags.apply_flag_string("no-output,pause-after=1,wait-exit,unknown");
    assert!(flags.no_output);
    assert_eq!(flags.pause_after_millis, Some(1_000));
    assert!(flags.wait_exit);

    flags.apply_flag_string("!no-output,!pause-after,!wait-exit");
    assert_eq!(flags, ControlClientFlags::default());
}

#[test]
fn control_client_flags_read_only_is_set_but_never_cleared_by_a_flag_string() {
    let mut flags = ControlClientFlags::default();
    flags.apply_flag_string("!read-only");
    assert!(!flags.read_only);
    flags.apply_flag_string("read-only");
    assert!(flags.read_only);
    assert!(
        !flags.ignore_size,
        "-f read-only does not imply ignore-size"
    );
    flags.apply_flag_string("!read-only");
    assert!(
        flags.read_only,
        "tmux masks !read-only off a read-only client"
    );
}

#[tokio::test]
async fn pane_output_lag_terminates_control_mode_explicitly() {
    let mut queue = ControlOutputQueue::default();
    let mut paused_panes = std::collections::HashSet::new();
    let lagged = handle_pane_event(
        PaneEvent::Lagged {
            pane_id: 7,
            epoch: 0,
            expected_sequence: 2,
            resume_sequence: 9,
            missed_events: 7,
            retention: None,
        },
        &mut queue,
        &mut paused_panes,
        &mut ControlPaneOutputStates::default(),
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
        None,
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
    crate::session_fixture::assert_new_session(&created);
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
    let mut reader_demands = std::collections::HashMap::new();
    let mut pane_output_states = ControlPaneOutputStates::default();
    refresh_subscriptions(
        &handler,
        control_identity,
        Some(&session_name),
        PaneSubscriptionState {
            subscriptions: &mut subscriptions,
            reader_demands: &mut reader_demands,
            pane_output_states: &mut pane_output_states,
        },
        pane_event_tx.into(),
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
    let _requester_access_guard =
        handler.begin_test_detached_requester_access(4242, AccessMode::ReadWrite);

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

async fn run_registered_initial_control_batch(
    handler: Arc<RequestHandler>,
    requester_pid: u32,
    input: Vec<u8>,
    initial_command_count: usize,
) -> String {
    run_registered_control_batch(handler, requester_pid, input, initial_command_count, false).await
}

async fn attach_message_test_client(
    handler: &RequestHandler,
    requester_pid: u32,
    control_id: u64,
    session: &SessionName,
) {
    let commands = handler
        .parse_control_commands(&format!("attach-session -t {session}"))
        .await
        .unwrap();
    let result = handler
        .execute_control_commands_identity(requester_pid, control_id, commands)
        .await;
    assert!(result.error.is_none(), "{result:?}");
}

async fn run_registered_message_control_batch(
    handler: Arc<RequestHandler>,
    requester_pid: u32,
    input: Vec<u8>,
    initial_command_count: usize,
) -> String {
    run_registered_control_batch(handler, requester_pid, input, initial_command_count, true).await
}

async fn run_registered_control_batch(
    handler: Arc<RequestHandler>,
    requester_pid: u32,
    input: Vec<u8>,
    initial_command_count: usize,
    message_recipient: bool,
) -> String {
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (server_event_tx, mut server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: u32::try_from(initial_command_count)
                    .expect("test command count fits u32"),
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            server_event_tx,
            Arc::clone(&closing),
        )
        .await;
    if message_recipient {
        // tmux status messages require a session-bearing recipient. These
        // framing tests exercise message ownership, not detached delivery.
        let session = SessionName::new("message-recipient").unwrap();
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: None,
                    environment: None,
                }))
                .await,
        );
        attach_message_test_client(&handler, requester_pid, control_id, &session).await;
        while server_event_rx.try_recv().is_ok() {}
    }
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let handler_for_control = Arc::clone(&handler);
    let control_task = tokio::spawn(async move {
        forward_control_identity(
            server_stream,
            handler_for_control,
            identity,
            ControlUpgradeInput::new(input, initial_command_count),
            shutdown_rx,
            server_event_rx,
            ControlLifecycle {
                closing,
                shutdown_handle,
            },
        )
        .await
    });

    if message_recipient {
        client_stream
            .shutdown()
            .await
            .expect("message fixture input EOF");
    }
    let mut rendered = Vec::new();
    read_control_to_end(&mut client_stream, &mut rendered).await;
    control_task
        .await
        .expect("control task joins")
        .expect("control forwarding succeeds");
    handler.finish_control(requester_pid, control_id).await;
    String::from_utf8(rendered).expect("control transcript is utf-8")
}

struct OpenControlTestClient {
    client_stream: UnixStream,
    output: Vec<u8>,
    control_task: tokio::task::JoinHandle<std::io::Result<()>>,
    initial_ack_pending: bool,
    _shutdown_tx: watch::Sender<()>,
}

impl OpenControlTestClient {
    async fn spawn(handler: Arc<RequestHandler>, requester_pid: u32) -> Self {
        let (server_stream, client_stream) = UnixStream::pair().expect("unix stream pair");
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let (server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        let closing = Arc::new(AtomicBool::new(false));
        let control_id = handler
            .register_control_with_closing(
                requester_pid,
                ControlModeUpgrade {
                    initial_command_count: 0,
                    mode: ControlMode::Plain,
                    terminal_context: OuterTerminalContext::default(),
                },
                server_event_tx,
                Arc::clone(&closing),
            )
            .await;
        let identity = ControlClientIdentity::new(requester_pid, control_id);
        let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
        let handler_for_control = Arc::clone(&handler);
        let control_task = tokio::spawn(async move {
            let result = forward_control_identity(
                server_stream,
                Arc::clone(&handler_for_control),
                identity,
                ControlUpgradeInput::new(Vec::new(), 0),
                shutdown_rx,
                server_event_rx,
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
        Self {
            client_stream,
            output: Vec::new(),
            control_task,
            initial_ack_pending: true,
            _shutdown_tx: shutdown_tx,
        }
    }

    async fn command(&mut self, line: impl AsRef<str>) {
        let line = line.as_ref();
        let expected_frames =
            closed_control_frame_count(&self.output) + 1 + usize::from(self.initial_ack_pending);
        self.initial_ack_pending = false;
        self.client_stream
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("control command writes");
        self.read_until_closed_frame_count(expected_frames, line)
            .await;
    }

    async fn read_until_closed_frame_count(&mut self, expected_frames: usize, label: &str) {
        for _ in 0..2048 {
            self.drain_available();
            if closed_control_frame_count(&self.output) >= expected_frames {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("timed out waiting for control frame after {label}");
    }

    async fn advance_and_drain(&mut self, duration: Duration) {
        tokio::time::advance(duration).await;
        self.yield_and_drain_available().await;
    }

    async fn yield_and_drain_available(&mut self) {
        for _ in 0..8 {
            tokio::task::yield_now().await;
            self.drain_available();
        }
    }

    fn drain_available(&mut self) {
        let mut buffer = [0_u8; 1024];
        loop {
            match self.client_stream.try_read(&mut buffer) {
                Ok(0) => panic!("control stream closed unexpectedly"),
                Ok(bytes_read) => self.output.extend_from_slice(&buffer[..bytes_read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(error) => panic!("control output try_read failed: {error}"),
            }
        }
    }

    fn rendered(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }

    async fn close(mut self) -> String {
        self.client_stream
            .write_all(b"\n")
            .await
            .expect("empty command exits control mode");
        output_drain::read_to_eof(&mut self.client_stream, &mut self.output).await;
        self.control_task
            .await
            .expect("control task joins")
            .expect("control forwarding succeeds");
        String::from_utf8(self.output).expect("control transcript is utf-8")
    }
}

fn closed_control_frame_count(output: &[u8]) -> usize {
    let rendered = String::from_utf8_lossy(output);
    parse_guard_lines(&rendered, "%end ").len() + parse_guard_lines(&rendered, "%error ").len()
}

fn subscription_lines_named(output: &[u8], name: &str) -> Vec<String> {
    let prefix = format!("%subscription-changed {name} ");
    String::from_utf8_lossy(output)
        .lines()
        .filter(|line| line.starts_with(&prefix))
        .map(ToOwned::to_owned)
        .collect()
}

fn control_message_test_config(label: &str, contents: &str) -> crate::test_shell::ScratchPath {
    let path =
        crate::test_shell::ScratchPath::reserve(&format!("control-message-{label}"), ".conf");
    std::fs::write(&path, contents).expect("control message test config");
    path
}

#[tokio::test]
async fn admitted_display_messages_are_owned_by_their_exact_control_guards() {
    let handler = Arc::new(RequestHandler::new());
    let session_name =
        SessionName::new("control-message-guard-pipeline").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);

    let commands = [
        "display-message -- SYNC-FIRST-A",
        "display-message -p -- PRINT-FIRST",
        "list-sessions -F 'LIST-FIRST:#{session_name}'",
        "display-message -- SYNC-FIRST-B",
        "definitely-not-a-command",
        "display-message -- SYNC-REPEAT-A",
        "display-message -p -- PRINT-REPEAT",
        "list-sessions -F 'LIST-REPEAT:#{session_name}'",
        "display-message -- SYNC-REPEAT-B",
    ];
    // The commands arrive on stdin, after the synthetic initial frame: an
    // error stops a command-line list, but not the stdin lines after it.
    let input = format!("{}\n\n", commands.join("\n")).into_bytes();
    let rendered =
        run_registered_message_control_batch(Arc::clone(&handler), 42_430, input, 0).await;
    let transcript = parse_strict_control_transcript(&rendered);
    let frame = |command_index: usize| command_index + 1;

    assert_eq!(
        transcript.frames.len(),
        frame(commands.len()),
        "{rendered:?}"
    );
    assert_message_owned_once(&transcript, frame(0), "SYNC-FIRST-A");
    assert_message_owned_once(&transcript, frame(3), "SYNC-FIRST-B");
    assert_message_owned_once(&transcript, frame(5), "SYNC-REPEAT-A");
    assert_message_owned_once(&transcript, frame(8), "SYNC-REPEAT-B");
    assert_eq!(
        transcript.frames[frame(1)].payload,
        ["PRINT-FIRST"],
        "{rendered:?}"
    );
    assert!(
        transcript.frames[frame(2)]
            .payload
            .iter()
            .any(|line| line == &format!("LIST-FIRST:{session_name}")),
        "{rendered:?}"
    );
    assert_eq!(
        transcript.frames[frame(4)].terminal,
        TestGuardTerminal::Error,
        "{rendered:?}"
    );
    assert_eq!(
        transcript.frames[frame(6)].payload,
        ["PRINT-REPEAT"],
        "{rendered:?}"
    );
    assert!(
        transcript.frames[frame(7)]
            .payload
            .iter()
            .any(|line| line == &format!("LIST-REPEAT:{session_name}")),
        "{rendered:?}"
    );
}

#[tokio::test]
async fn initial_batch_stops_at_its_first_failed_command_like_tmux() {
    // tmux 3.7b oracle, measured 2026-11-19 (R3511 M1): the command-line
    // commands form one command list, so the failed guard of
    // `has-session -t missing \; kill-session -t keep` stops the list.
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("control-initial-guard").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);

    let commands = [
        "display-message -p FIRST".to_owned(),
        "has-session -t control-initial-missing".to_owned(),
        format!("kill-session -t {session_name}"),
        "display-message -p NEVER".to_owned(),
    ];
    let input = format!("{}\n", commands.join("\n")).into_bytes();
    let rendered =
        run_registered_initial_control_batch(Arc::clone(&handler), 42_480, input, commands.len())
            .await;
    let transcript = parse_strict_control_transcript(&rendered);

    assert_eq!(transcript.frames.len(), 2, "{rendered:?}");
    assert_eq!(transcript.frames[0].payload, ["FIRST"], "{rendered:?}");
    assert_eq!(
        transcript.frames[1].terminal,
        TestGuardTerminal::Error,
        "{rendered:?}"
    );
    assert!(!rendered.contains("NEVER"), "{rendered:?}");
    let exists = handler
        .handle(Request::HasSession(rmux_proto::HasSessionRequest {
            target: session_name,
        }))
        .await;
    assert_eq!(
        exists,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );
}

#[tokio::test]
async fn initial_batch_continues_after_an_error_inside_an_inserted_child() {
    // tmux 3.7b oracle, measured 2026-11-19: only the command's own error
    // stops the list; if-shell itself succeeds when its child fails.
    let handler = Arc::new(RequestHandler::new());
    let commands = [
        "if-shell -F 1 'has-session -t control-initial-missing'",
        "display-message -p AFTER-CHILD-ERROR",
    ];
    let input = format!("{}\n", commands.join("\n")).into_bytes();
    let rendered =
        run_registered_initial_control_batch(Arc::clone(&handler), 42_481, input, commands.len())
            .await;
    let transcript = parse_strict_control_transcript(&rendered);

    assert_eq!(transcript.frames.len(), 3, "{rendered:?}");
    assert_eq!(
        transcript.frames[0].terminal,
        TestGuardTerminal::End,
        "{rendered:?}"
    );
    assert_eq!(
        transcript.frames[1].terminal,
        TestGuardTerminal::Error,
        "{rendered:?}"
    );
    assert_eq!(
        transcript.frames[2].payload,
        ["AFTER-CHILD-ERROR"],
        "{rendered:?}"
    );
}

#[tokio::test]
async fn stdin_lines_keep_running_after_an_execution_error_on_stdin() {
    // tmux 3.7b oracle, measured 2026-11-19 (R3521 c14): only a command-line
    // list stops at an error; each stdin line is its own list, so a failed
    // stdin command leaves the lines queued behind it to run.
    let handler = Arc::new(RequestHandler::new());
    let commands = [
        "has-session -t control-stdin-missing",
        "display-message -p AFTER-STDIN-ERROR",
        "has-session -t control-stdin-missing",
        "display-message -p LAST-STDIN-LINE",
    ];
    let input = format!("{}\n\n", commands.join("\n")).into_bytes();
    let rendered =
        run_registered_initial_control_batch(Arc::clone(&handler), 42_482, input, 0).await;
    let transcript = parse_strict_control_transcript(&rendered);
    let frame = |command_index: usize| command_index + 1;

    assert_eq!(
        transcript.frames.len(),
        frame(commands.len()),
        "{rendered:?}"
    );
    for failed in [0, 2] {
        assert_eq!(
            transcript.frames[frame(failed)].terminal,
            TestGuardTerminal::Error,
            "{rendered:?}"
        );
    }
    assert_eq!(
        transcript.frames[frame(1)].payload,
        ["AFTER-STDIN-ERROR"],
        "{rendered:?}"
    );
    assert_eq!(
        transcript.frames[frame(3)].payload,
        ["LAST-STDIN-LINE"],
        "{rendered:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn refresh_client_format_subscriptions_tick_current_value_and_stop_on_remove() {
    let handler = Arc::new(RequestHandler::new());
    let session_name =
        SessionName::new("control-format-subscriptions").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);

    let mut control = OpenControlTestClient::spawn(Arc::clone(&handler), 42_443).await;
    control
        .command(format!("attach-session -t {session_name}"))
        .await;
    control
        .command("display-message -p '#{session_id} #{window_id} #{window_index} #{pane_id}'")
        .await;

    let rendered = control.rendered();
    let transcript = parse_strict_control_transcript(&rendered);

    let ids = transcript.frames[2]
        .payload
        .first()
        .unwrap_or_else(|| panic!("identity payload missing: {rendered:?}"))
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 4, "unexpected identity payload: {rendered:?}");
    let session_id = &ids[0];
    let window_id = &ids[1];
    let window_index = &ids[2];
    let pane_id = &ids[3];
    let latest_title = format!(
        "%subscription-changed title {session_id} {window_id} {window_index} {pane_id} : SECOND"
    );

    control
        .command("refresh-client -B 'bad:%0:#{pane_id}' -C bogus")
        .await;
    let rendered = control.rendered();
    let transcript = parse_strict_control_transcript(&rendered);
    assert!(
        transcript
            .frames
            .last()
            .is_some_and(|frame| frame.terminal == TestGuardTerminal::Error),
        "invalid -C must reject before mutating -B subscriptions: {rendered:?}"
    );
    assert!(
        transcript.frames.last().is_some_and(|frame| frame
            .payload
            .iter()
            .any(|line| line.contains("invalid refresh-client size 'bogus'"))),
        "invalid -C error payload missing: {rendered:?}"
    );
    assert!(
        subscription_lines_named(&control.output, "bad").is_empty(),
        "failed refresh-client must not install -B definitions: {rendered:?}"
    );

    control
        .command("refresh-client -B 'title:%0:#{pane_title}'")
        .await;
    control.command("select-pane -T FIRST").await;
    control.command("refresh-client").await;
    control.command("select-pane -T SECOND").await;
    control.yield_and_drain_available().await;
    assert_eq!(
        subscription_lines_named(&control.output, "title"),
        Vec::<String>::new(),
        "registration and rapid refreshes must not emit before the cadence tick: {:?}",
        control.rendered()
    );

    control.advance_and_drain(Duration::from_millis(999)).await;
    assert_eq!(
        subscription_lines_named(&control.output, "title"),
        Vec::<String>::new(),
        "the first cadence tick must not fire early: {:?}",
        control.rendered()
    );

    control.advance_and_drain(Duration::from_millis(1)).await;
    assert_eq!(
        subscription_lines_named(&control.output, "title"),
        std::slice::from_ref(&latest_title),
        "the first tick must publish only the current value: {:?}",
        control.rendered()
    );

    control.command("refresh-client").await;
    control.command("refresh-client").await;
    control.advance_and_drain(Duration::from_secs(1)).await;
    assert_eq!(
        subscription_lines_named(&control.output, "title"),
        std::slice::from_ref(&latest_title),
        "unchanged refreshes must stay silent at the next tick: {:?}",
        control.rendered()
    );

    control
        .command("refresh-client -B 'title:%0:#{pane_title}'")
        .await;
    control.yield_and_drain_available().await;
    assert_eq!(
        subscription_lines_named(&control.output, "title"),
        std::slice::from_ref(&latest_title),
        "replacement must wait for the next cadence tick: {:?}",
        control.rendered()
    );

    control.advance_and_drain(Duration::from_secs(1)).await;
    assert_eq!(
        subscription_lines_named(&control.output, "title"),
        [latest_title.clone(), latest_title.clone()],
        "replacement must re-emit on cadence, not immediately: {:?}",
        control.rendered()
    );

    control.command("refresh-client -B title").await;
    control.command("select-pane -T REMOVED").await;
    control.advance_and_drain(Duration::from_secs(3)).await;
    let rendered = control.close().await;
    assert!(
        !rendered.contains("REMOVED"),
        "last removal must stop the owned cadence: {rendered:?}"
    );
    assert_eq!(
        subscription_lines_named(rendered.as_bytes(), "title"),
        [latest_title.clone(), latest_title],
        "removed subscriptions must not publish later changes: {rendered:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn refresh_client_format_subscriptions_filter_targets_to_control_session() {
    let handler = Arc::new(RequestHandler::new());
    let alpha = SessionName::new("control-format-alpha").expect("valid session name");
    let beta = SessionName::new("control-format-beta").expect("valid session name");
    for session_name in [&alpha, &beta] {
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: None,
                environment: None,
            }))
            .await;
        crate::session_fixture::assert_new_session(&created);
    }

    let mut control = OpenControlTestClient::spawn(Arc::clone(&handler), 42_444).await;
    control.command(format!("attach-session -t {beta}")).await;
    control
        .command("refresh-client -B 'cross:%0:#{pane_id}'")
        .await;
    control.command(format!("switch-client -t {alpha}")).await;
    control
        .command("display-message -p '#{session_id} #{window_id} #{window_index} #{pane_id}'")
        .await;
    control.yield_and_drain_available().await;
    assert_eq!(
        subscription_lines_named(&control.output, "cross"),
        Vec::<String>::new(),
        "session switch must not force an immediate format emission: {:?}",
        control.rendered()
    );
    control.advance_and_drain(Duration::from_secs(1)).await;
    let rendered = control.close().await;
    let transcript = parse_strict_control_transcript(&rendered);

    let ids = transcript.frames[4]
        .payload
        .first()
        .unwrap_or_else(|| panic!("identity payload missing: {rendered:?}"))
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 4, "unexpected identity payload: {rendered:?}");
    let expected = format!(
        "%subscription-changed cross {} {} {} {} : {}",
        ids[0], ids[1], ids[2], ids[3], ids[3]
    );
    let cross_lines = transcript
        .asynchronous_notifications
        .iter()
        .filter(|line| line.starts_with("%subscription-changed cross "))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(cross_lines, [expected], "{rendered:?}");
}

#[tokio::test]
async fn queued_display_messages_stay_once_inside_the_admitted_control_guard() {
    let handler = Arc::new(RequestHandler::new());
    let input = b"display-message -- QUEUE-SYNC-A ; \
                  display-message -p -- QUEUE-PRINT ; \
                  display-message -- QUEUE-SYNC-B\n"
        .to_vec();
    let rendered =
        run_registered_message_control_batch(Arc::clone(&handler), 42_431, input, 1).await;
    let transcript = parse_strict_control_transcript(&rendered);

    assert_eq!(transcript.frames.len(), 1, "{rendered:?}");
    assert_message_owned_once(&transcript, 0, "QUEUE-SYNC-A");
    assert_message_owned_once(&transcript, 0, "QUEUE-SYNC-B");
    assert_eq!(
        transcript.frames[0].payload,
        ["QUEUE-PRINT"],
        "{rendered:?}"
    );
}

#[tokio::test]
async fn sourced_and_conditional_display_messages_get_distinct_child_guards() {
    // Fresh tmux 3.7b oracle:
    // source-file: parent end, then one flags-preserving guard per sourced command.
    // if-shell -F: parent end, then one guard for the selected branch.
    let source = control_message_test_config(
        "source-child-ownership",
        "display-message -- SOURCE-CHILD-A\n\
         display-message -- SOURCE-CHILD-B\n",
    );
    let handler = Arc::new(RequestHandler::new());
    let commands = [
        format!("source-file {}", source.display()),
        "if-shell -F 1 'display-message -- IF-TRUE-CHILD' \
         'display-message -- IF-TRUE-UNSELECTED'"
            .to_owned(),
        "if -F 0 'display-message -- IF-FALSE-UNSELECTED' \
         'display-message -- IF-FALSE-CHILD'"
            .to_owned(),
        "display-message -d 0 -- DIRECT-AFTER-CHILDREN".to_owned(),
    ];
    let input = format!("{}\n", commands.join("\n")).into_bytes();
    let rendered =
        run_registered_message_control_batch(Arc::clone(&handler), 42_433, input, commands.len())
            .await;
    let transcript = parse_strict_control_transcript(&rendered);

    assert_eq!(transcript.frames.len(), 8, "{rendered:?}");
    assert!(
        transcript.frames[0].notifications.is_empty(),
        "{rendered:?}"
    );
    assert_message_owned_once(&transcript, 1, "SOURCE-CHILD-A");
    assert_message_owned_once(&transcript, 2, "SOURCE-CHILD-B");
    assert!(
        transcript.frames[3].notifications.is_empty(),
        "{rendered:?}"
    );
    assert_message_owned_once(&transcript, 4, "IF-TRUE-CHILD");
    assert!(
        transcript.frames[5].notifications.is_empty(),
        "{rendered:?}"
    );
    assert_message_owned_once(&transcript, 6, "IF-FALSE-CHILD");
    assert_message_owned_once(&transcript, 7, "DIRECT-AFTER-CHILDREN");
    assert!(
        transcript.frames.iter().all(|frame| frame.guard.flags == 0),
        "initial parent and synchronous children retain flag 0: {rendered:?}"
    );
    assert!(
        !rendered.contains("IF-TRUE-UNSELECTED") && !rendered.contains("IF-FALSE-UNSELECTED"),
        "{rendered:?}"
    );

    std::fs::remove_file(source).expect("remove source child config");
}

#[tokio::test]
async fn sourced_command_alias_keeps_the_sourced_child_owner() {
    let source = control_message_test_config(
        "source-child-command-alias",
        "announce SOURCE-ALIAS-CHILD\n",
    );
    let handler = Arc::new(RequestHandler::new());
    let commands = [
        "set-option -s 'command-alias[100]' 'announce=display-message --'".to_owned(),
        format!("source-file {}", source.display()),
    ];
    let input = format!("{}\n", commands.join("\n")).into_bytes();
    let rendered =
        run_registered_message_control_batch(Arc::clone(&handler), 42_436, input, commands.len())
            .await;
    let transcript = parse_strict_control_transcript(&rendered);

    assert_eq!(transcript.frames.len(), 3, "{rendered:?}");
    assert!(
        transcript.frames[1].notifications.is_empty(),
        "{rendered:?}"
    );
    assert_message_owned_once(&transcript, 2, "SOURCE-ALIAS-CHILD");

    std::fs::remove_file(source).expect("remove source command-alias config");
}

#[tokio::test]
async fn command_alias_to_if_shell_keeps_the_selected_child_owner() {
    let handler = Arc::new(RequestHandler::new());
    let commands = [
        "set-option -s 'command-alias[101]' 'choose=if-shell -F 1'".to_owned(),
        "choose 'display-message -- IF-COMMAND-ALIAS-CHILD'".to_owned(),
    ];
    let input = format!("{}\n", commands.join("\n")).into_bytes();
    let rendered =
        run_registered_message_control_batch(Arc::clone(&handler), 42_437, input, commands.len())
            .await;
    let transcript = parse_strict_control_transcript(&rendered);

    assert_eq!(transcript.frames.len(), 3, "{rendered:?}");
    assert!(
        transcript.frames[1].notifications.is_empty(),
        "{rendered:?}"
    );
    assert_message_owned_once(&transcript, 2, "IF-COMMAND-ALIAS-CHILD");
}

#[tokio::test]
async fn sourced_runtime_error_stays_in_its_child_guard_after_prior_message() {
    // The invalid target is a runtime command failure, not a parse error: tmux
    // first closes source-file, then ends the display child and errors the
    // following child.
    let source = control_message_test_config(
        "source-child-error",
        "display-message -- SOURCE-BEFORE-ERROR\n\
         kill-pane -t missing-source-session:0.0\n",
    );
    let handler = Arc::new(RequestHandler::new());
    let input = format!("source-file {}\n", source.display()).into_bytes();
    let rendered =
        run_registered_message_control_batch(Arc::clone(&handler), 42_434, input, 1).await;
    let transcript = parse_strict_control_transcript(&rendered);

    assert_eq!(transcript.frames.len(), 3, "{rendered:?}");
    assert_eq!(
        transcript.frames[0].terminal,
        TestGuardTerminal::End,
        "{rendered:?}"
    );
    assert!(
        transcript.frames[0].notifications.is_empty(),
        "{rendered:?}"
    );
    assert_message_owned_once(&transcript, 1, "SOURCE-BEFORE-ERROR");
    assert_eq!(
        transcript.frames[2].terminal,
        TestGuardTerminal::Error,
        "{rendered:?}"
    );
    assert_eq!(
        transcript.frames[2].payload,
        ["can't find session: missing-source-session"],
        "{rendered:?}"
    );

    std::fs::remove_file(source).expect("remove source error config");
}

#[tokio::test]
async fn conditional_runtime_error_stays_in_its_child_guard_after_prior_message() {
    let handler = Arc::new(RequestHandler::new());
    let input = b"if-shell -F 1 'display-message -- IF-BEFORE-ERROR ; \
                  kill-pane -t missing-if-session:0.0'\n"
        .to_vec();
    let rendered =
        run_registered_message_control_batch(Arc::clone(&handler), 42_438, input, 1).await;
    let transcript = parse_strict_control_transcript(&rendered);

    assert_eq!(transcript.frames.len(), 3, "{rendered:?}");
    assert_eq!(
        transcript.frames[0].terminal,
        TestGuardTerminal::End,
        "{rendered:?}"
    );
    assert_message_owned_once(&transcript, 1, "IF-BEFORE-ERROR");
    assert_eq!(
        transcript.frames[2].terminal,
        TestGuardTerminal::Error,
        "{rendered:?}"
    );
    assert!(
        transcript.frames[2]
            .payload
            .iter()
            .any(|line| line.contains("missing-if-session")),
        "{rendered:?}"
    );
}

#[tokio::test]
async fn inserted_child_frames_exceed_channel_capacity_without_fifo_loss() {
    // Ready response frames must drain without a scheduler turn per protocol line.
    const CHILD_COUNT: usize = CONTROL_SERVER_EVENT_CAPACITY / 2;

    let contents = (0..CHILD_COUNT)
        .map(|index| format!("display-message -- SOURCE-FIFO-{index:03}\n"))
        .collect::<String>();
    let source = control_message_test_config("source-child-fifo", &contents);
    let handler = Arc::new(RequestHandler::new());
    let input = format!("source-file {}\n", source.display()).into_bytes();
    let rendered =
        run_registered_message_control_batch(Arc::clone(&handler), 42_439, input, 1).await;
    let transcript = parse_strict_control_transcript(&rendered);

    assert_eq!(transcript.frames.len(), CHILD_COUNT + 1, "{rendered:?}");
    assert!(
        transcript.frames[0].notifications.is_empty(),
        "{rendered:?}"
    );
    for (index, frame) in transcript.frames.iter().skip(1).enumerate() {
        assert_eq!(
            frame.notifications,
            [format!("%message SOURCE-FIFO-{index:03}")],
            "child {index} lost, duplicated, or reordered: {rendered:?}"
        );
    }
    assert!(
        transcript
            .asynchronous_notifications
            .iter()
            .all(|line| !line.starts_with("%message ")),
        "{rendered:?}"
    );

    std::fs::remove_file(source).expect("remove source FIFO config");
}

#[tokio::test]
async fn rejected_synchronous_insertion_errors_the_parent_without_an_orphan_guard() {
    let inserted =
        "start-server ;".repeat(crate::handler::TEST_CONTROL_QUEUE_INSERTED_COMMAND_LIMIT + 1);
    let handler = Arc::new(RequestHandler::new());
    let input = format!("if-shell -F 1 '{inserted}'\n").into_bytes();
    let rendered =
        run_registered_initial_control_batch(Arc::clone(&handler), 42_440, input, 1).await;
    let transcript = parse_strict_control_transcript(&rendered);

    assert_eq!(transcript.frames.len(), 1, "{rendered:?}");
    assert_eq!(
        transcript.frames[0].terminal,
        TestGuardTerminal::Error,
        "{rendered:?}"
    );
    assert!(
        transcript.frames[0]
            .payload
            .iter()
            .any(|line| line.contains("inserted too many commands")),
        "{rendered:?}"
    );
    assert!(
        transcript
            .asynchronous_notifications
            .iter()
            .all(|line| !line.starts_with("%message ")),
        "{rendered:?}"
    );
}

#[tokio::test]
async fn direct_display_forms_remain_in_their_admitted_guards() {
    let handler = Arc::new(RequestHandler::new());
    let commands = [
        "display -- DIRECT-ALIAS",
        "display-mes -- DIRECT-PREFIX",
        "display-message -d 0 -- DIRECT-EXT-DURATION",
        "display-message -F 'DIRECT-EXT-FORMAT'",
        "display-message -- DIRECT-REPEAT",
        "display-message -- DIRECT-REPEAT",
    ];
    let input = format!("{}\n", commands.join("\n")).into_bytes();
    let rendered =
        run_registered_message_control_batch(Arc::clone(&handler), 42_435, input, commands.len())
            .await;
    let transcript = parse_strict_control_transcript(&rendered);

    assert_eq!(transcript.frames.len(), commands.len(), "{rendered:?}");
    for (index, token) in [
        "DIRECT-ALIAS",
        "DIRECT-PREFIX",
        "DIRECT-EXT-DURATION",
        "DIRECT-EXT-FORMAT",
    ]
    .into_iter()
    .enumerate()
    {
        assert_message_owned_once(&transcript, index, token);
    }
    assert_eq!(
        transcript.frames[4].notifications,
        ["%message DIRECT-REPEAT"],
        "{rendered:?}"
    );
    assert_eq!(
        transcript.frames[5].notifications,
        ["%message DIRECT-REPEAT"],
        "{rendered:?}"
    );
    assert_eq!(
        transcript
            .frames
            .iter()
            .flat_map(|frame| frame.notifications.iter())
            .filter(|line| line.as_str() == "%message DIRECT-REPEAT")
            .count(),
        2,
        "{rendered:?}"
    );
}

#[tokio::test]
async fn immediate_run_shell_commands_get_one_child_guard_per_nesting_level() {
    // Fresh tmux 3.7b oracle: each run-shell -C level closes its current
    // guard before the inserted callback begins in a new guard.
    let handler = Arc::new(RequestHandler::new());
    let session_name =
        SessionName::new("control-message-run-shell-nesting").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name,
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);

    let commands = [
        "run-shell -C 'display-message -- RUN-C-NEST-1'",
        "run-shell -C \"run-shell -C 'display-message -- RUN-C-NEST-2'\"",
        "run-shell -C \"run-shell -C \\\"run-shell -C \
         'display-message -- RUN-C-NEST-3'\\\"\"",
    ];
    let input = format!("{}\n", commands.join("\n")).into_bytes();
    let rendered =
        run_registered_message_control_batch(Arc::clone(&handler), 42_441, input, commands.len())
            .await;
    let transcript = parse_strict_control_transcript(&rendered);

    assert_eq!(transcript.frames.len(), 9, "{rendered:?}");
    for parent in [0, 2, 3, 5, 6, 7] {
        assert!(
            transcript.frames[parent].notifications.is_empty(),
            "run-shell parent/level {parent} captured its child: {rendered:?}"
        );
    }
    assert_message_owned_once(&transcript, 1, "RUN-C-NEST-1");
    assert_message_owned_once(&transcript, 4, "RUN-C-NEST-2");
    assert_message_owned_once(&transcript, 8, "RUN-C-NEST-3");
    assert!(
        transcript.frames.iter().all(|frame| frame.guard.flags == 0),
        "initial parents and synchronous callbacks retain flag 0: {rendered:?}"
    );
    assert!(
        transcript
            .asynchronous_notifications
            .iter()
            .all(|line| !line.starts_with("%message ")),
        "{rendered:?}"
    );
}

#[tokio::test]
async fn immediate_run_shell_callback_error_gets_its_own_child_guard() {
    let handler = Arc::new(RequestHandler::new());
    let session_name =
        SessionName::new("control-message-run-shell-error").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name,
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);

    let input = b"run-shell -C 'display-message -- RUN-C-BEFORE-ERROR ; \
                  kill-pane -t missing-run-session:0.0'\n"
        .to_vec();
    let rendered =
        run_registered_message_control_batch(Arc::clone(&handler), 42_442, input, 1).await;
    let transcript = parse_strict_control_transcript(&rendered);

    assert_eq!(transcript.frames.len(), 3, "{rendered:?}");
    assert_eq!(
        transcript.frames[0].terminal,
        TestGuardTerminal::End,
        "{rendered:?}"
    );
    assert!(
        transcript.frames[0].notifications.is_empty(),
        "{rendered:?}"
    );
    assert_message_owned_once(&transcript, 1, "RUN-C-BEFORE-ERROR");
    assert_eq!(
        transcript.frames[2].terminal,
        TestGuardTerminal::Error,
        "{rendered:?}"
    );
    assert!(
        transcript.frames[2]
            .payload
            .iter()
            .any(|line| line.contains("missing-run-session")),
        "{rendered:?}"
    );
}

#[tokio::test]
async fn delayed_run_shell_control_message_remains_asynchronous_product_divergence() {
    // Frozen tmux 3.7b gives the delayed `run-shell -C` callback its own
    // control guard. RMUX did not do so before W13-M30, and this fix must not
    // annex that delayed notification to the already-closed parent guard.
    let handler = Arc::new(RequestHandler::new());
    let session_name =
        SessionName::new("control-message-guard-delayed").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);

    let requester_pid = 42_432;
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 2,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            server_event_tx,
            Arc::clone(&closing),
        )
        .await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    let input = format!(
        "attach-session -t {session_name}\n\
         run-shell -d 0.05 -C \"display-message -- DELAYED-RUN-SHELL-MESSAGE\"\n"
    )
    .into_bytes();
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let handler_for_control = Arc::clone(&handler);
    let control_task = tokio::spawn(async move {
        forward_control_identity(
            server_stream,
            handler_for_control,
            identity,
            ControlUpgradeInput::new(input, 2),
            shutdown_rx,
            server_event_rx,
            ControlLifecycle {
                closing,
                shutdown_handle,
            },
        )
        .await
    });

    let mut rendered = Vec::new();
    let mut buffer = [0_u8; 1024];
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        loop {
            let bytes_read = client_stream
                .read(&mut buffer)
                .await
                .expect("control output reads");
            assert_ne!(
                bytes_read, 0,
                "control stream closed before delayed notification"
            );
            rendered.extend_from_slice(&buffer[..bytes_read]);
            if String::from_utf8_lossy(&rendered).contains("%message DELAYED-RUN-SHELL-MESSAGE") {
                break;
            }
        }
    })
    .await
    .expect("delayed run-shell notification arrives before timeout");

    client_stream
        .write_all(b"\n")
        .await
        .expect("empty command exits control mode");
    read_control_to_end(&mut client_stream, &mut rendered).await;
    control_task
        .await
        .expect("control task joins")
        .expect("control forwarding succeeds");
    handler.finish_control(requester_pid, control_id).await;

    let rendered = String::from_utf8(rendered).expect("control transcript is utf-8");
    let transcript = parse_strict_control_transcript(&rendered);
    assert_eq!(transcript.frames.len(), 2, "{rendered:?}");
    assert_message_asynchronous_once(&transcript, "DELAYED-RUN-SHELL-MESSAGE");
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

// Product divergence measured against tmux 3.7b: tmux drops queued work once
// control input reaches EOF. RMUX deliberately finishes non-blocking automation
// after closing the transport, while cancelling frames that would wait forever.
//
// Paused clock (see `ACTIVE_FRAME_HOLD`): the active job is a virtual delay,
// so "the transport closes first" is proved by ordering, not by host latency.
#[tokio::test(start_paused = true)]
async fn eof_closes_transport_while_finite_control_queue_continues_product_divergence() {
    const ACTIVE_JOB_BUFFER: &str = "eof-detached-active-job";
    const FOLLOW_ON_BUFFER: &str = "eof-follow-on";
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard =
        handler.begin_test_detached_requester_access(4242, AccessMode::ReadWrite);
    let command = format!(
        "run-shell -d {} -C 'set-buffer -b {ACTIVE_JOB_BUFFER} done'\nset-buffer -b {FOLLOW_ON_BUFFER} done\n",
        ACTIVE_FRAME_HOLD.as_secs()
    );
    let started = tokio::time::Instant::now();

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
    read_control_to_end(&mut client_stream, &mut remaining).await;
    let rendered = format!(
        "{begin_prefix}{}",
        String::from_utf8(remaining).expect("utf-8 control stream")
    );
    assert!(
        started.elapsed() < ACTIVE_FRAME_HOLD,
        "control EOF must not wait for the foreground job: {rendered:?}"
    );
    assert!(
        !control_task.is_finished(),
        "the server-side finite queue must remain alive after the transport closes"
    );
    for name in [ACTIVE_JOB_BUFFER, FOLLOW_ON_BUFFER] {
        assert_eq!(
            show_buffer(&handler, name).await,
            None,
            "{name} must not exist before the transport closes: {rendered:?}"
        );
    }
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

    tokio::time::timeout(ACTIVE_FRAME_HOLD * 2, control_task)
        .await
        .expect("finite control queue completes before timeout")
        .expect("forward control task joins")
        .expect("forward control succeeds");
    assert!(started.elapsed() >= ACTIVE_FRAME_HOLD);
    assert_eq!(
        show_buffer(&handler, ACTIVE_JOB_BUFFER).await.as_deref(),
        Some(b"done".as_slice()),
        "detached foreground job still completes server-side"
    );
    assert_eq!(
        show_buffer(&handler, FOLLOW_ON_BUFFER).await.as_deref(),
        Some(b"done".as_slice()),
        "follow-on set-buffer succeeds"
    );
}

#[tokio::test(start_paused = true)]
async fn eof_preserves_active_if_shell_when_wait_is_only_in_unselected_branch_product_divergence() {
    const BRANCH_BUFFER: &str = "eof-active-finite-branch";
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard =
        handler.begin_test_detached_requester_access(4250, AccessMode::ReadWrite);
    let input = format!(
        "if-shell -F 1 {{ run-shell -d {} '' ; set-buffer -b {BRANCH_BUFFER} done }} {{ wait-for eof-active-unselected-wait }}\n",
        ACTIVE_FRAME_HOLD.as_secs()
    );
    let started = tokio::time::Instant::now();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4250,
        ControlUpgradeInput::new(input.into_bytes(), 1),
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
    read_control_to_end(&mut client_stream, &mut rendered).await;
    assert!(
        started.elapsed() < ACTIVE_FRAME_HOLD,
        "unselected wait does not retain the transport"
    );
    assert!(
        !control_task.is_finished(),
        "the selected finite branch keeps draining after transport EOF"
    );
    assert_eq!(
        show_buffer(&handler, BRANCH_BUFFER).await,
        None,
        "the transport closes while the selected branch is still active"
    );
    tokio::time::timeout(ACTIVE_FRAME_HOLD * 2, control_task)
        .await
        .expect("selected finite branch finishes before timeout")
        .expect("control task joins")
        .expect("control queue drains successfully");

    assert_eq!(
        handler.wait_for_counts("eof-active-unselected-wait"),
        (0, 0, false),
        "the unselected wait branch must never register"
    );
    assert_eq!(
        show_buffer(&handler, BRANCH_BUFFER).await.as_deref(),
        Some(b"done".as_slice()),
        "selected finite branch executes after EOF"
    );
}

#[tokio::test(start_paused = true)]
async fn eof_queued_if_shell_cancels_only_a_selected_wait_frame_product_divergence() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard =
        handler.begin_test_detached_requester_access(4251, AccessMode::ReadWrite);
    let input = format!(
        "run-shell -d {} ''\nif-shell -F 1 {{ set-buffer -b eof-queued-finite-branch done }} {{ wait-for eof-queued-unselected-wait }}\nif-shell -F 1 {{ wait-for eof-queued-selected-wait ; set-buffer -b eof-queued-after-wait must-not-run }} {{ set-buffer -b eof-queued-fallback must-not-run }}\nset-buffer -b eof-queued-later-frame done\n",
        ACTIVE_FRAME_HOLD.as_secs()
    );
    let started = tokio::time::Instant::now();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4251,
        ControlUpgradeInput::new(input.into_bytes(), 1),
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
    read_control_to_end(&mut client_stream, &mut rendered).await;
    assert!(
        started.elapsed() < ACTIVE_FRAME_HOLD,
        "queued wait branches do not retain the transport"
    );
    for name in ["eof-queued-finite-branch", "eof-queued-later-frame"] {
        assert_eq!(
            show_buffer(&handler, name).await,
            None,
            "the transport closes while the active frame still owns the queue: {name}"
        );
    }
    tokio::time::timeout(ACTIVE_FRAME_HOLD * 2, control_task)
        .await
        .expect("EOF queue drains before timeout")
        .expect("control task joins")
        .expect("queued frames drain independently");

    for name in ["eof-queued-finite-branch", "eof-queued-later-frame"] {
        assert_eq!(
            show_buffer(&handler, name).await.as_deref(),
            Some(b"done".as_slice()),
            "buffer {name} must exist"
        );
    }
    for name in ["eof-queued-after-wait", "eof-queued-fallback"] {
        assert_eq!(
            show_buffer(&handler, name).await,
            None,
            "selected wait must stop its frame before buffer {name}"
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

// The paused clock removes the wall-clock race: the active frame's one-hour
// delay and the EOF grace are virtual, and the runtime only advances time while
// every task is idle. A transport retained by a queued frame therefore shows up
// as an ordering failure (the later frame ran, or a virtual timeout expired)
// rather than as host load. No child process runs, so idling never hides real
// work from the auto-advancing clock.
#[tokio::test(start_paused = true)]
async fn eof_queue_skips_parse_errors_and_blocking_frames_before_later_finite_frame_product_divergence(
) {
    const ACTIVE_FRAME_DELAY: Duration = Duration::from_secs(3_600);
    const BLOCKED_CHANNEL: &str = "never-signalled";
    const LATER_BUFFER: &str = "eof-after-skipped-frames";
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let input = format!(
        "run-shell -d {} ''\ndisplay-message -p 'unterminated\nwait-for {BLOCKED_CHANNEL}\nset-buffer -b {LATER_BUFFER} done\n",
        ACTIVE_FRAME_DELAY.as_secs()
    );
    let started = tokio::time::Instant::now();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4245,
        ControlUpgradeInput::new(input.into_bytes(), 1),
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
    read_control_to_end(&mut client_stream, &mut rendered).await;
    let rendered = String::from_utf8(rendered).expect("UTF-8 control transcript");

    // Ordering, not latency: the transport reached EOF while the active frame
    // still owned the queue, so no later frame can have retained it.
    assert!(
        started.elapsed() < ACTIVE_FRAME_DELAY,
        "the transport must close before the active frame completes: {rendered:?}"
    );
    assert!(
        matches!(
            handler
                .handle(Request::ShowBuffer(ShowBufferRequest {
                    name: Some(LATER_BUFFER.to_owned()),
                }))
                .await,
            Response::Error(_)
        ),
        "the later finite frame must not run before the transport closes: {rendered:?}"
    );
    assert_eq!(
        rendered.matches("%begin ").count(),
        1,
        "only the active frame may open a guard before %exit: {rendered:?}"
    );
    assert!(
        !rendered.contains("parse error"),
        "a parse error queued behind EOF has no transport to report to: {rendered:?}"
    );
    assert!(rendered.ends_with("%exit\n"), "{rendered:?}");

    // One more virtual hour than the active frame: only a blocked wait-for
    // frame can leave the drain idle long enough for this timeout to expire.
    tokio::time::timeout(ACTIVE_FRAME_DELAY * 2, control_task)
        .await
        .expect("blocking wait-for frame is skipped after EOF")
        .expect("control task joins")
        .expect("queued parse errors stay local to their frame");
    assert_eq!(
        handler.wait_for_counts(BLOCKED_CHANNEL),
        (0, 0, false),
        "the skipped wait-for frame leaves no waiter behind"
    );

    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some(LATER_BUFFER.to_owned()),
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
            execution_errors: Vec::new(),
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
            execution_errors: Vec::new(),
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
        ControlQueueDrainLease::Acquired,
        "failed Exit delivery keeps the exact closing registration"
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
        "a closing registration is terminal even when Exit was never delivered"
    );
    handler.finish_control(requester_pid, control_id).await;
    assert_eq!(
        handler.begin_control_queue_drain(identity).await,
        ControlQueueDrainLease::Unavailable,
        "transport finish removes the exact registration"
    );
}

#[tokio::test(start_paused = true)]
async fn eof_after_deferred_exit_with_removed_registration_finishes_only_active_frame_product_divergence(
) {
    const EVENT_CAPACITY: usize = 8;
    const STARTED_BUFFER: &str = "eof-deferred-exit-started";

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
    let _requester_access_guard =
        handler.begin_test_detached_requester_access(requester_pid, AccessMode::ReadWrite);
    let command = format!(
        "new-session -s {session_name}\nset-buffer -b {STARTED_BUFFER} started ; run-shell -d {} ''\nset-buffer -b eof-after-deferred-exit must-not-run\n",
        ACTIVE_FRAME_HOLD.as_secs()
    );
    let started = tokio::time::Instant::now();
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
        while show_buffer(&handler, STARTED_BUFFER).await.is_none() {
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
    read_control_to_end(&mut client_stream, &mut rendered).await;
    let rendered = String::from_utf8(rendered).expect("utf-8 control transcript");
    assert!(
        started.elapsed() < ACTIVE_FRAME_HOLD,
        "deferred Exit closes the transport before the active command finishes: {rendered:?}"
    );
    assert!(
        !control_task.is_finished(),
        "the already-started finite command must finish after transport close"
    );
    assert!(
        rendered.contains("%end "),
        "EOF closes the active frame guard: {rendered:?}"
    );
    assert!(
        rendered.ends_with("%exit\n"),
        "the deferred Exit remains terminal: {rendered:?}"
    );

    tokio::time::timeout(ACTIVE_FRAME_HOLD * 2, control_task)
        .await
        .expect("active command finishes before timeout")
        .expect("forward control task joins")
        .expect("missing queue lease is not a product error");
    // The registration is gone, so the frame has no client left to report
    // to; only its full virtual hold shows it was finished, not abandoned.
    assert!(
        started.elapsed() >= ACTIVE_FRAME_HOLD,
        "the finite command that was active at EOF must finish"
    );
    assert_eq!(
        show_buffer(&handler, "eof-after-deferred-exit").await,
        None,
        "a queued frame after deferred Exit must never run"
    );
}

#[tokio::test(start_paused = true)]
async fn external_shutdown_drains_admitted_finite_eof_mutation_product_divergence() {
    const MUTATION_BUFFER: &str = "eof-shutdown-mutation";
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let command = format!(
        "run-shell -d {} -C 'set-buffer -b {MUTATION_BUFFER} done'\n",
        ACTIVE_FRAME_HOLD.as_secs()
    );
    let started = tokio::time::Instant::now();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4247,
        ControlUpgradeInput::new(command.into_bytes(), 1),
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
    read_control_to_end(&mut client_stream, &mut rendered).await;
    assert!(
        started.elapsed() < ACTIVE_FRAME_HOLD,
        "control transport closes before the finite frame completes"
    );
    assert!(
        !control_task.is_finished(),
        "finite frame is still draining before external shutdown"
    );
    assert!(!handler.normal_drain_requests_quiesced());
    assert_eq!(show_buffer(&handler, MUTATION_BUFFER).await, None);

    handler.close_normal_request_admission();
    shutdown_tx.send_replace(());
    tokio::time::timeout(ACTIVE_FRAME_HOLD * 2, control_task)
        .await
        .expect("external shutdown drains the admitted detached mutation")
        .expect("control task joins")
        .expect("shutdown drain is clean");
    assert!(handler.normal_drain_requests_quiesced());
    assert_eq!(
        show_buffer(&handler, MUTATION_BUFFER).await.as_deref(),
        Some(b"done".as_slice()),
        "admitted EOF mutation commits"
    );
}

#[tokio::test(start_paused = true)]
async fn eof_drains_finite_queue_through_kill_server_product_divergence() {
    let handler = Arc::new(RequestHandler::new());
    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let (shutdown_handle, mut shutdown_request_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle.clone());
    let input = format!(
        "run-shell -d {} '' ; kill-server ; set-buffer -b eof-same-frame must-not-run\nset-buffer -b eof-next-frame must-not-run\n",
        ACTIVE_FRAME_HOLD.as_secs()
    );
    let started = tokio::time::Instant::now();

    let control_task = tokio::spawn(forward_control(
        server_stream,
        Arc::clone(&handler),
        4243,
        ControlUpgradeInput::new(input.into_bytes(), 2),
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
    assert!(
        started.elapsed() < ACTIVE_FRAME_HOLD,
        "control transport closes before the shell job finishes"
    );
    assert!(
        !control_task.is_finished(),
        "kill-server must remain queued after transport EOF"
    );
    assert!(
        matches!(
            shutdown_request_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ),
        "kill-server has not run when the transport closes"
    );

    tokio::time::timeout(ACTIVE_FRAME_HOLD * 2, &mut shutdown_request_rx)
        .await
        .expect("queued kill-server requests shutdown before timeout")
        .expect("shutdown request channel stays open");
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, control_task)
        .await
        .expect("finite control queue completes before timeout")
        .expect("forward control task joins")
        .expect("forward control succeeds");
    assert!(started.elapsed() >= ACTIVE_FRAME_HOLD);

    for buffer_name in ["eof-same-frame", "eof-next-frame"] {
        assert_eq!(
            show_buffer(&handler, buffer_name).await,
            None,
            "kill-server must suppress {buffer_name}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn eof_queue_lease_blocks_same_pid_registration_and_preserves_permissions_product_divergence()
{
    // Shorter than the 5 s registration drain deadline, which is virtual too,
    // so the replacement still waits for the old queue instead of timing out.
    const OLD_QUEUE_HOLD: Duration = Duration::from_secs(2);
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
    let eof_lease_pause = install_control_eof_queue_lease_pause(&handler, old_identity);
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let input = format!(
        "run-shell -d {} '' ; set-buffer -b eof-old-identity old\n",
        OLD_QUEUE_HOLD.as_secs()
    );
    let started = tokio::time::Instant::now();
    let handler_for_control = Arc::clone(&handler);
    let control_task = tokio::spawn(async move {
        let result = forward_control_identity(
            server_stream,
            Arc::clone(&handler_for_control),
            old_identity,
            ControlUpgradeInput::new(input.into_bytes(), 1),
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
    // On the paused clock this sleep ends only once every task is idle, so
    // the registration has had every chance to run and is truly parked.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !registration_task.is_finished(),
        "same-PID registration must wait as soon as EOF is observed"
    );
    eof_lease_pause.release.notify_one();

    let mut rendered = Vec::new();
    read_control_to_end(&mut client_stream, &mut rendered).await;
    assert!(
        started.elapsed() < OLD_QUEUE_HOLD,
        "old control transport closes before its queue finishes"
    );
    assert!(
        !control_task.is_finished(),
        "old control queue must still own its registration lease"
    );
    assert!(
        !registration_task.is_finished(),
        "the replacement still waits for the old queue lease"
    );
    assert_eq!(show_buffer(&handler, "eof-old-identity").await, None);

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

    assert_eq!(
        show_buffer(&handler, "eof-old-identity").await.as_deref(),
        Some(b"old".as_slice()),
        "old queue keeps its write permission"
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
    assert_eq!(show_buffer(&handler, "eof-new-identity").await, None);
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
async fn runtime_alias_terminal_records_are_finalized_before_control_transport_close() {
    const REQUESTER_PID: u32 = 42_441;

    let handler = Arc::new(RequestHandler::new());
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::CommandAlias,
                value: "zz=CONTROL_ALIAS_VALUE=ok display-message -p \"$CONTROL_ALIAS_VALUE\""
                    .to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (server_event_tx, server_event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(AtomicBool::new(false));
    let control_id = handler
        .register_control_with_closing(
            REQUESTER_PID,
            ControlModeUpgrade {
                initial_command_count: 1,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            server_event_tx,
            Arc::clone(&closing),
        )
        .await;
    let identity = ControlClientIdentity::new(REQUESTER_PID, control_id);
    let transport_drain_pause = install_control_transport_drain_pause(&handler, identity);
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let control_task = tokio::spawn(forward_control_identity(
        server_stream,
        Arc::clone(&handler),
        identity,
        ControlUpgradeInput::new(b"zz\n".to_vec(), 1),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing,
            shutdown_handle,
        },
    ));

    tokio::time::timeout(
        CONTROL_TEST_TIMEOUT,
        transport_drain_pause.wait_until_reached(),
    )
    .await
    .expect("alias completion reaches transport finalization");

    let mut rendered = Vec::new();
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        while !rendered.ends_with(b"%exit\n") {
            let mut chunk = [0_u8; 256];
            let bytes_read = client_stream
                .read(&mut chunk)
                .await
                .expect("read control output before finalization");
            assert_ne!(
                bytes_read, 0,
                "the transport must remain open until finalization"
            );
            rendered.extend_from_slice(&chunk[..bytes_read]);
        }
    })
    .await
    .expect("terminal control output arrives before finalization");
    assert!(
        !control_task.is_finished(),
        "the control task must still own the transport at the drain boundary"
    );
    let mut eof_probe = [0_u8; 1];
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            client_stream.read(&mut eof_probe)
        )
        .await
        .is_err(),
        "transport EOF must follow the drain boundary"
    );

    transport_drain_pause.release();
    let mut trailing = Vec::new();
    read_control_to_end(&mut client_stream, &mut trailing).await;
    assert!(
        trailing.is_empty(),
        "unexpected output after %exit: {trailing:?}"
    );
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");
    handler.finish_control(REQUESTER_PID, control_id).await;

    let rendered = String::from_utf8(rendered).expect("utf-8 control stream");
    assert_eq!(parse_guard_lines(&rendered, "%begin ").len(), 1);
    assert_eq!(parse_guard_lines(&rendered, "%end ").len(), 1);
    assert!(rendered.lines().any(|line| line == "ok"), "{rendered:?}");
    assert!(rendered.ends_with("%exit\n"), "{rendered:?}");
}

#[tokio::test]
async fn completed_unattached_initial_command_exits_with_stdin_open_and_discards_follow_on_frames()
{
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
        ControlUpgradeInput::new(
            b"display-message -p INITIAL\ndisplay-message -p SHOULD-NOT-RUN\n".to_vec(),
            1,
        ),
        shutdown_rx,
        server_event_rx,
        ControlLifecycle {
            closing,
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
    assert!(rendered.contains("INITIAL\n"), "{rendered:?}");
    assert!(!rendered.contains("SHOULD-NOT-RUN"), "{rendered:?}");
    assert_eq!(parse_guard_lines(&rendered, "%begin ").len(), 1);
    assert_eq!(parse_guard_lines(&rendered, "%end ").len(), 1);
    assert!(rendered.ends_with("%exit\n"), "{rendered:?}");
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
    crate::session_fixture::assert_new_session(&created);

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
async fn plain_control_eof_keeps_ready_existing_session_attach_before_exit() {
    let handler = Arc::new(RequestHandler::new());
    let requester_pid = 42_431;
    let session_name =
        SessionName::new("plain-control-eof-attach-race").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);

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
    let command = format!("attach-session -t {session_name}\n");
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
            if handler.control_session_name(requester_pid).await.as_ref() == Some(&session_name) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("attach commits while the forward loop remains paused");
    eof_pause.release.notify_one();

    let mut rendered = Vec::new();
    read_control_to_end(&mut client_stream, &mut rendered).await;
    control_task
        .await
        .expect("forward control task joins")
        .expect("forward control succeeds");

    let rendered = String::from_utf8(rendered).expect("utf-8 control stream");
    let records = rendered.lines().collect::<Vec<_>>();
    assert_eq!(records.len(), 4, "{rendered:?}");
    assert!(records[0].starts_with("%begin "), "{rendered:?}");
    assert!(records[1].starts_with("%end "), "{rendered:?}");
    assert_eq!(
        records[2],
        format!("%session-changed $0 {session_name}"),
        "{rendered:?}"
    );
    assert_eq!(records[3], "%exit", "{rendered:?}");
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
                mode: ControlMode::ControlControl,
                terminal_context: OuterTerminalContext::default(),
            },
            event_tx,
            Arc::clone(&closing),
        )
        .await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    let attach_pause = handler.install_created_session_control_attach_pause(session_name.clone());
    let eof_pause = install_control_eof_queue_lease_pause(&handler, identity);
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let _requester_access_guard =
        handler.begin_test_detached_requester_access(requester_pid, AccessMode::ReadWrite);
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
async fn pane_output_waits_for_run_shell_guard() {
    let handler = Arc::new(RequestHandler::new());
    let session = SessionName::new("guarded-output").unwrap();
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    let (_, pane) = handler
        .control_session_panes(&session)
        .await
        .unwrap()
        .remove(0);
    let mut control = OpenControlTestClient::spawn(handler, 42_258).await;
    control
        .command(format!("attach-session -t {session}"))
        .await;
    control.output.clear();
    control
        .client_stream
        .write_all(b"run-shell 'sleep 0.3'\n")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !control.rendered().contains("%begin ") {
            control.client_stream.readable().await.unwrap();
            control.drain_available();
        }
        pane.send(b"LATE".to_vec());
        while !control.rendered().contains("LATE") {
            control.client_stream.readable().await.unwrap();
            control.drain_available();
        }
    })
    .await
    .expect("pane output delivered");
    let rendered = control.close().await;
    assert!(
        rendered.find("%end ").unwrap() < rendered.find("LATE").unwrap(),
        "{rendered}"
    );
}

#[tokio::test]
async fn refresh_client_a_preserves_tmux_pane_output_state_transitions() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("control-pane-output-actions").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    let (pane_id, pane_output) = handler
        .control_session_panes(&session_name)
        .await
        .expect("session pane output is available")
        .into_iter()
        .next()
        .expect("session has an initial pane");

    let mut control = OpenControlTestClient::spawn(Arc::clone(&handler), 42_447).await;
    control
        .command(format!("attach-session -t {session_name}"))
        .await;

    pane_output.send(b"BASE\r\n".to_vec());
    control.yield_and_drain_available().await;
    assert!(control.rendered().contains("BASE"));

    control
        .command(format!("refresh-client -A '%{pane_id}:off'"))
        .await;
    pane_output.send(b"OFF\r\n".to_vec());
    control.yield_and_drain_available().await;
    assert!(
        !control.rendered().contains("OFF"),
        "off output leaked into control transcript: {}",
        control.rendered()
    );

    control
        .command(format!("refresh-client -A '%{pane_id}:on'"))
        .await;
    control.yield_and_drain_available().await;
    assert!(
        !control.rendered().contains("OFF"),
        "on must resume at the current delivered position without replay"
    );

    control
        .command(format!("refresh-client -A '%{pane_id}:pause'"))
        .await;
    pane_output.send(b"PAUSED\r\n".to_vec());
    control.yield_and_drain_available().await;
    assert!(!control.rendered().contains("PAUSED"));
    assert!(control.rendered().contains(&format!("%pause %{pane_id}")));

    control
        .command(format!("refresh-client -A '%{pane_id}:continue'"))
        .await;
    control.yield_and_drain_available().await;
    assert!(
        !control.rendered().contains("PAUSED"),
        "continue does not replay paused output"
    );
    assert!(control
        .rendered()
        .contains(&format!("%continue %{pane_id}")));

    pane_output.send(b"LIVE\r\n".to_vec());
    control.yield_and_drain_available().await;
    assert!(control.rendered().contains("LIVE"));

    control.command("refresh-client -f no-output").await;
    pane_output.send(b"NO_OUTPUT\r\n".to_vec());
    control.yield_and_drain_available().await;
    assert!(!control.rendered().contains("NO_OUTPUT"));

    control.command("refresh-client -f !no-output").await;
    pane_output.send(b"FLAGS_LIVE\r\n".to_vec());
    control.yield_and_drain_available().await;
    assert!(control.rendered().contains("FLAGS_LIVE"));
    let _ = control.close().await;
}

#[tokio::test]
async fn external_refresh_client_a_applies_off_before_target_refresh() {
    let handler = Arc::new(RequestHandler::new());
    let session_name =
        SessionName::new("control-pane-output-external-off").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    let (pane_id, pane_output) = handler
        .control_session_panes(&session_name)
        .await
        .expect("session pane output is available")
        .into_iter()
        .next()
        .expect("session has an initial pane");

    let target_pid = 42_510;
    let actor_pid = 42_511;
    let mut target = OpenControlTestClient::spawn(Arc::clone(&handler), target_pid).await;
    target
        .command(format!("attach-session -t {session_name}"))
        .await;
    let mut actor = OpenControlTestClient::spawn(Arc::clone(&handler), actor_pid).await;
    actor
        .command(format!("attach-session -t {session_name}"))
        .await;

    pane_output.send(b"EXTERNAL_BEFORE\r\n".to_vec());
    target.yield_and_drain_available().await;
    assert!(target.rendered().contains("EXTERNAL_BEFORE"));

    actor
        .command(format!(
            "refresh-client -t {target_pid} -A '%{pane_id}:off'"
        ))
        .await;
    pane_output.send(b"EXTERNAL_OFF\r\n".to_vec());
    target.yield_and_drain_available().await;
    actor.yield_and_drain_available().await;
    assert!(
        !target.rendered().contains("EXTERNAL_OFF"),
        "targeted off must not be followed by a stale refresh that restores the target demand"
    );
    assert!(
        actor.rendered().contains("EXTERNAL_OFF"),
        "the unrelated attached client must continue consuming the pane"
    );

    let _ = actor.close().await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            pane_output.wait_for_reader_demand(pane_id, None),
        )
        .await
        .is_err(),
        "targeted off must remove the target client's demand after unrelated clients detach"
    );

    target
        .command(format!("refresh-client -A '%{pane_id}:on'"))
        .await;
    assert!(
        tokio::time::timeout(
            CONTROL_TEST_TIMEOUT,
            pane_output.wait_for_reader_demand(pane_id, None),
        )
        .await
        .expect("on wakes the retired demand wait"),
        "on must restore demand for the target pane"
    );

    let _ = target.close().await;
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
    let _requester_access_guard =
        handler.begin_test_detached_requester_access(4242, AccessMode::ReadWrite);

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
    let _requester_access_guard =
        handler.begin_test_detached_requester_access(4242, AccessMode::ReadWrite);

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

// Ordering, not latency: the producer never idles, so the paused clock stays
// frozen while it floods and no timeout can expire on host load. The transport
// must reach %exit while the producer is still sending; an EOF starved by the
// events closes only after the bounded producer gives up.
#[tokio::test(start_paused = true)]
async fn eof_transition_is_not_starved_by_continuous_server_events() {
    // EOF wins after a few hundred events; a starved EOF needs the whole budget.
    const PRODUCER_EVENT_LIMIT: usize = 100_000;
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
    let _requester_access_guard =
        handler.begin_test_detached_requester_access(requester_pid, AccessMode::ReadWrite);
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

    let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let producer = tokio::spawn({
        let sent = Arc::clone(&sent);
        async move {
            for _ in 0..PRODUCER_EVENT_LIMIT {
                if event_tx.send(ControlServerEvent::Refresh).await.is_err() {
                    return;
                }
                sent.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    client_stream
        .shutdown()
        .await
        .expect("client write half closes");

    let mut rendered = Vec::new();
    read_control_to_end(&mut client_stream, &mut rendered).await;
    let sent_at_close = sent.load(Ordering::SeqCst);
    assert!(
        sent_at_close < PRODUCER_EVENT_LIMIT,
        "continuous server events cannot retain the EOF transport: it closed after {sent_at_close} events"
    );
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
        origin: ControlCommandOrigin::Initial {
            completes_batch: true,
        },
        owner_failed: None,
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

/// Virtual hold for the frame that is active when control input reaches EOF.
///
/// The EOF tests that use it run on a paused Tokio clock with no child process,
/// so the EOF grace and every timeout are virtual and time only advances while
/// all tasks are idle. "The transport closed before the active frame finished"
/// is then an ordering fact (the read ends before this much virtual time, and
/// before the frame's own effect is visible), not a race against host load.
/// A transport that waits for the frame fails at once, on its first idle turn.
const ACTIVE_FRAME_HOLD: Duration = Duration::from_secs(3_600);

/// Contents of buffer `name`, or `None` when show-buffer reports no such buffer.
async fn show_buffer(handler: &RequestHandler, name: &str) -> Option<Vec<u8>> {
    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some(name.to_owned()),
        }))
        .await;
    if matches!(response, Response::Error(_)) {
        return None;
    }
    let output = response
        .command_output()
        .unwrap_or_else(|| panic!("show-buffer {name} returns output: {response:?}"));
    Some(output.stdout().to_vec())
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct TestGuardTuple {
    time_secs: i64,
    command_number: u64,
    flags: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestGuardTerminal {
    End,
    Error,
}

#[derive(Debug)]
struct TestControlFrame {
    guard: TestGuardTuple,
    terminal: TestGuardTerminal,
    payload: Vec<String>,
    notifications: Vec<String>,
}

#[derive(Debug)]
struct TestControlTranscript {
    frames: Vec<TestControlFrame>,
    asynchronous_notifications: Vec<String>,
}

fn parse_strict_control_transcript(output: &str) -> TestControlTranscript {
    struct OpenFrame {
        guard: TestGuardTuple,
        payload: Vec<String>,
        notifications: Vec<String>,
    }

    let mut frames = Vec::new();
    let mut asynchronous_notifications = Vec::new();
    let mut current: Option<OpenFrame> = None;
    for line in output.lines() {
        if let Some(guard) = parse_guard_tuple(line, "%begin ") {
            assert!(
                current.is_none(),
                "nested control guard {guard:?} in {output:?}"
            );
            current = Some(OpenFrame {
                guard,
                payload: Vec::new(),
                notifications: Vec::new(),
            });
            continue;
        }
        let terminal = parse_guard_tuple(line, "%end ")
            .map(|guard| (guard, TestGuardTerminal::End))
            .or_else(|| {
                parse_guard_tuple(line, "%error ").map(|guard| (guard, TestGuardTerminal::Error))
            });
        if let Some((guard, terminal)) = terminal {
            let open = current
                .take()
                .unwrap_or_else(|| panic!("orphan terminal guard {guard:?} in {output:?}"));
            assert_eq!(
                open.guard, guard,
                "terminal tuple differs from its begin in {output:?}"
            );
            frames.push(TestControlFrame {
                guard,
                terminal,
                payload: open.payload,
                notifications: open.notifications,
            });
            continue;
        }
        if line.starts_with('%') {
            if let Some(open) = current.as_mut() {
                open.notifications.push(line.to_owned());
            } else {
                asynchronous_notifications.push(line.to_owned());
            }
        } else if let Some(open) = current.as_mut() {
            open.payload.push(line.to_owned());
        } else if !line.is_empty() {
            panic!("control payload escaped its guard: {line:?} in {output:?}");
        }
    }
    assert!(current.is_none(), "unclosed control guard in {output:?}");
    assert!(
        frames
            .windows(2)
            .all(|pair| pair[0].guard.command_number < pair[1].guard.command_number),
        "control command numbers are not strictly monotone in {output:?}"
    );
    TestControlTranscript {
        frames,
        asynchronous_notifications,
    }
}

fn assert_message_owned_once(
    transcript: &TestControlTranscript,
    expected_frame: usize,
    token: &str,
) {
    let expected_line = format!("%message {token}");
    let owned = transcript
        .frames
        .iter()
        .enumerate()
        .flat_map(|(index, frame)| {
            frame
                .notifications
                .iter()
                .filter(|line| *line == &expected_line)
                .map(move |_| index)
        })
        .collect::<Vec<_>>();
    let asynchronous = transcript
        .asynchronous_notifications
        .iter()
        .filter(|line| *line == &expected_line)
        .count();
    assert_eq!(
        owned.len() + asynchronous,
        1,
        "{expected_line:?} must be emitted exactly once: {transcript:?}"
    );
    assert_eq!(
        owned,
        [expected_frame],
        "{expected_line:?} must belong to frame {expected_frame}: {transcript:?}"
    );
}

fn assert_message_asynchronous_once(transcript: &TestControlTranscript, token: &str) {
    let expected_line = format!("%message {token}");
    let owned = transcript
        .frames
        .iter()
        .flat_map(|frame| &frame.notifications)
        .filter(|line| *line == &expected_line)
        .count();
    let asynchronous = transcript
        .asynchronous_notifications
        .iter()
        .filter(|line| *line == &expected_line)
        .count();
    assert_eq!(
        owned, 0,
        "{expected_line:?} must not enter a command guard: {transcript:?}"
    );
    assert_eq!(
        asynchronous, 1,
        "{expected_line:?} must remain one asynchronous notification: {transcript:?}"
    );
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

#[path = "format_subscriptions/client_tests.rs"]
mod format_subscription_clients;

// tmux 3.7b: only timestamps and command numbers are variable. Check paired
// guards before normalizing them, including unframed shell output in the result.
fn normalized_shell_control_transcript(rendered: &str) -> Vec<String> {
    let mut pending = None;
    let lines = rendered
        .lines()
        .map(|line| {
            if let Some(guard) = parse_guard_tuple(line, "%begin ") {
                assert!(pending.replace(guard.clone()).is_none(), "{rendered}");
                return format!("%begin {}", guard.flags);
            }
            for terminal in ["%end", "%error"] {
                if let Some(guard) = parse_guard_tuple(line, &format!("{terminal} ")) {
                    assert_eq!(pending.take(), Some(guard.clone()), "{rendered}");
                    return format!("{terminal} {}", guard.flags);
                }
            }
            line.to_owned()
        })
        .collect::<Vec<_>>();
    assert!(pending.is_none(), "{rendered}");
    lines
}

#[tokio::test]
async fn run_shell_output_follows_control_end_for_initial_and_stdin_queues() {
    for (input, initial_count, expected) in [
        ("run-shell 'printf SHELL'\n", 1, "%begin 0|%end 0|SHELL|%exit"),
        ("run 'printf SHELL'\n", 1, "%begin 0|%end 0|SHELL|%exit"),
        ("run-shell 'printf OUT; printf ERR >&2'\n", 1,
         "%begin 0|%end 0|OUT|%exit"),
        ("run-shell 'printf SHELL; exit 7'\n", 1,
         "%begin 0|%end 0|SHELL|'printf SHELL; exit 7' returned 7|%exit"),
        ("display-message -p BEFORE\nrun-shell 'printf SHELL'\ndisplay-message -p AFTER\n", 3,
         "%begin 0|BEFORE|%end 0|%begin 0|%end 0|SHELL|%begin 0|AFTER|%end 0|%exit"),
        ("display-message -p BEFORE ; run-shell 'printf SHELL' ; display-message -p AFTER\n\n", 0,
         "%begin 0|%end 0|%begin 1|BEFORE|%end 1|%begin 1|%end 1|SHELL|%begin 1|AFTER|%end 1|%exit"),
        ("run-shell -C \"display-message -p INNER\"\n", 1,
         "%begin 0|%end 0|%begin 0|INNER|%end 0|%exit"),
        ("if-shell -F 1 \"run-shell 'printf SHELL'\"\n", 1,
         "%begin 0|%end 0|%begin 0|%end 0|SHELL|%exit"),
        ("display-message -p BEFORE ; run-shell 'printf SHELL' ; display-message -p AFTER\n", 1,
         "%begin 0|BEFORE|%end 0|SHELL|%begin 0|AFTER|%end 0|%exit"),
    ] {
        let handler = Arc::new(RequestHandler::new());
        let rendered = run_registered_initial_control_batch(handler, 48_120, input.as_bytes().to_vec(), initial_count).await;
        assert_eq!(normalized_shell_control_transcript(&rendered), expected.split('|').collect::<Vec<_>>(), "{input}: {rendered}");
    }
}

#[tokio::test]
async fn run_shell_admission_error_stays_inside_control_error_guard() {
    let handler = Arc::new(RequestHandler::new());
    let rendered = run_registered_initial_control_batch(
        handler,
        48_121,
        b"run-shell -d -1 'printf MUST-NOT-RUN'\n".to_vec(),
        1,
    )
    .await;
    let transcript = parse_strict_control_transcript(&rendered);
    assert_eq!(transcript.frames.len(), 1, "{rendered}");
    assert_eq!(transcript.frames[0].terminal, TestGuardTerminal::Error);
    assert!(!transcript.frames[0].payload.is_empty(), "{rendered}");
    assert!(!rendered.contains("MUST-NOT-RUN"), "{rendered}");
}

#[path = "tests/session_destruction.rs"]
mod session_destruction;

#[tokio::test]
async fn sourced_runtime_diagnostics_omit_file_prefix_in_control_guards() {
    for initial_count in [2, 0] {
        let source =
            control_message_test_config("source-runtime-diagnostic", "resize-pane -x bad\n");
        let input = format!(
            "source-file {}\ndisplay-message -p AFTER-RUNTIME\n\n",
            source.display()
        );
        let rendered = run_registered_initial_control_batch(
            Arc::new(RequestHandler::new()),
            48_168,
            input.into_bytes(),
            initial_count,
        )
        .await;
        let transcript = parse_strict_control_transcript(&rendered);
        let offset = usize::from(initial_count == 0);
        assert_eq!(transcript.frames.len(), offset + 3, "{rendered:?}");
        assert_eq!(
            transcript.frames[offset].terminal,
            TestGuardTerminal::End,
            "{rendered:?}"
        );
        assert_eq!(
            transcript.frames[offset + 1].terminal,
            TestGuardTerminal::Error,
            "{rendered:?}"
        );
        assert_eq!(
            transcript.frames[offset + 1].payload,
            ["width invalid"],
            "{rendered:?}"
        );
        assert_eq!(
            transcript.frames[offset + 2].payload,
            ["AFTER-RUNTIME"],
            "{rendered:?}"
        );
        assert!(
            !rendered.contains(&source.display().to_string()),
            "{rendered:?}"
        );
        assert_eq!(
            transcript.frames[offset + 1].guard.flags,
            u8::from(initial_count == 0)
        );
    }
}

#[tokio::test]
async fn sourced_callback_parse_diagnostic_retains_file_prefix_in_control() {
    let source = control_message_test_config("source-callback-diagnostic", "run-shell -C '}'\n");
    let input = format!("source-file {}\n", source.display()).into_bytes();
    let rendered =
        run_registered_initial_control_batch(Arc::new(RequestHandler::new()), 48_169, input, 1)
            .await;
    assert!(
        rendered.contains(&format!("{}:1: ", source.display())),
        "{rendered:?}"
    );
}
