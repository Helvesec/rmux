use super::super::transport_state::{ControlTransport, ControlTransportEvents};
use super::super::*;
use crate::pane_io::spawn_pane_output_reader;
use crate::pane_reader_runtime::PaneReaderRuntime;
use crate::pane_transcript::PaneTranscript;
use rmux_proto::{NewSessionRequest, Request, SessionId, SessionName, TerminalSize};
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn attached_parse_error_follows_already_published_pty_output() {
    let handler = Arc::new(RequestHandler::new());
    let name = SessionName::new("parse-error-fifo").unwrap();
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    let (pane_id, sender) = handler
        .control_session_panes(&name)
        .await
        .unwrap()
        .remove(0);
    let pid = 42_555;
    let (server_tx, server_events) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let closing = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let id = handler
        .register_control_with_closing(
            pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            server_tx,
            Arc::clone(&closing),
        )
        .await;
    let identity = ControlClientIdentity::new(pid, id);
    handler
        .set_control_subscription_identity_for_test(identity, name.clone(), SessionId::new(0))
        .await;
    assert_eq!(handler.control_session_name(pid).await, Some(name.clone()));
    let (stream, mut peer) = tokio::net::UnixStream::pair().unwrap();
    let transport_drain = ControlTransportDrain::capture(&stream).unwrap();
    let (read_half, write_half) = tokio::io::split(stream);
    let (_shutdown_tx, shutdown) = watch::channel(());
    let (shutdown_handle, _shutdown_requests) = crate::daemon::ShutdownHandle::new();
    let (pane_event_tx, pane_event_rx) = mpsc::channel(CONTROL_PANE_EVENT_CAPACITY);
    let mut transport = ControlTransport {
        handler: Arc::clone(&handler),
        control_identity: identity,
        lifecycle: ControlLifecycle {
            closing,
            shutdown_handle,
        },
        mode: ControlMode::Plain,
        shutdown,
        server_events,
        read_half,
        write_half,
        input_buffer: Vec::new(),
        queued_lines: VecDeque::new(),
        queued_input_bytes: 0,
        read_buffer: [0; 8192],
        shutdown_draining: false,
        input_closed: false,
        eof_queue_lease: None,
        eof_transition: None,
        eof_command_ready: false,
        current_command: None,
        current_response_frame: None,
        current_response_notifications: None,
        initial_command_completion_pending: false,
        shutdown_write_half_after_drain: false,
        eof_completion: ControlEofCompletion::default(),
        command_numbering: ControlCommandNumbering::after_initial_ack(),
        transport_drain,
        pane_event_rx,
        pane_event_tx: pane_event_tx.into(),
        events: ControlTransportEvents {
            subscriptions: HashMap::new(),
            reader_demands: HashMap::new(),
            pane_output_states: ControlPaneOutputStates::default(),
            format_subscriptions: ControlFormatSubscriptions::default(),
            format_subscription_tick: None,
            output_queue: ControlOutputQueue::default(),
            paused_panes: HashSet::new(),
            flags: ControlClientFlags::default(),
            unrestricted_read_demand: None,
            attached_client_read_presence: Some(
                handler.acquire_attached_pane_read_presence().await,
            ),
            deferred_server_events: DeferredServerEvents::default(),
            attachment: ControlSessionAttachment::new(Some(name.clone())),
        },
    };
    refresh_subscriptions(
        &handler,
        identity,
        Some(&name),
        PaneSubscriptionState {
            subscriptions: &mut transport.events.subscriptions,
            reader_demands: &mut transport.events.reader_demands,
            pane_output_states: &mut transport.events.pane_output_states,
        },
        transport.pane_event_tx.clone(),
        PaneSubscriptionStart::Now,
    )
    .await;
    assert!(transport.events.subscriptions.contains_key(&pane_id));

    // Publish through a native PTY reader while the attached transport is idle.
    // The observation cursor proves P precedes parsing, without a timing sleep.
    let mut observation = sender.subscribe();
    let (master, slave) = rmux_pty::PtyPair::open().unwrap().into_split();
    let mut slave = std::fs::File::from(slave.into_owned_fd());
    std::io::Write::write_all(&mut slave, b"P\n").unwrap();
    let reader = spawn_pane_output_reader(
        name,
        rmux_core::PaneId::new(pane_id),
        master,
        PaneTranscript::shared(32, TerminalSize { cols: 80, rows: 24 }),
        sender,
        None,
        None,
        None,
        PaneReaderRuntime::current().unwrap(),
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let rmux_core::events::OutputCursorItem::Event(event) = observation.recv().await {
                if event.bytes().windows(3).any(|bytes| bytes == b"P\r\n") {
                    break;
                }
            }
        }
    })
    .await
    .expect("P is published before the invalid command");
    let invalid = String::from("not-a-command-s5055");
    transport.queued_input_bytes = invalid.len();
    transport.queued_lines.push_back(invalid);
    assert!(!transport.start_queued_commands().await.unwrap());
    reader.abort();
    drop(transport);
    handler.finish_control(pid, id).await;
    let mut output = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        peer.read_to_string(&mut output),
    )
    .await
    .unwrap()
    .unwrap();
    let pane_line = format!("%output %{pane_id} P\\015\\012");
    let relevant: Vec<_> = output
        .lines()
        .filter(|line| {
            *line == pane_line || line.starts_with("parse error:") || line.starts_with("%error ")
        })
        .map(|line| {
            if line == pane_line {
                "P"
            } else if line.starts_with("parse error:") {
                "parse error"
            } else {
                "%error"
            }
        })
        .collect();
    assert_eq!(relevant, ["P", "parse error", "%error"], "{output}");
}
