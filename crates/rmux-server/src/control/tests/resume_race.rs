use super::*;
use crate::control::{
    apply_control_pane_output_actions, ControlPaneOutputAction, ControlPaneOutputActionKind,
    DeferredServerEvents, ServerEventContext,
};
use crate::pane_io::spawn_pane_output_reader;
use crate::pane_reader_runtime::PaneReaderRuntime;
use crate::pane_transcript::PaneTranscript;
use rmux_core::events::OutputCursorItem;
use std::future::{poll_fn, Future};
use std::task::Poll;

#[tokio::test]
async fn resume_subscribes_before_waking_unread_pty_output() {
    let handler = Arc::new(RequestHandler::new());
    let name = SessionName::new("resume-causal-boundary").unwrap();
    let (pane_id, sender) = silent_session_pane(&handler, &name).await;
    let pid = 42_522;
    let (server_tx, _server_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let id = handler
        .register_control_with_closing(
            pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            server_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    let identity = ControlClientIdentity::new(pid, id);
    handler
        .set_control_subscription_identity_for_test(identity, name.clone(), SessionId::new(0))
        .await;
    let (stream, _peer) = UnixStream::pair().unwrap();
    let (_, writer) = tokio::io::split(stream);
    let mut writer = crate::control::ControlWriter::new(writer, false);
    let mut session_name = Some(name.clone());
    let mut subscriptions = Default::default();
    let mut reader_demands = Default::default();
    let mut states = ControlPaneOutputStates::default();
    let mut formats = Default::default();
    let mut tick = None;
    let (event_tx, mut event_rx) = mpsc::channel(16);
    let mut queue = ControlOutputQueue::default();
    let mut paused = Default::default();
    let mut flags = ControlClientFlags::default();
    let mut unrestricted = None;
    let mut presence = Some(handler.acquire_attached_pane_read_presence().await);
    let mut deferred = DeferredServerEvents::default();
    let mut context = ServerEventContext {
        handler: &handler,
        control_identity: identity,
        requester_pid: pid,
        session_name: &mut session_name,
        subscriptions: &mut subscriptions,
        reader_demands: &mut reader_demands,
        pane_output_states: &mut states,
        format_subscriptions: &mut formats,
        format_subscription_tick: &mut tick,
        pane_event_tx: event_tx.into(),
        pane_event_rx: &mut event_rx,
        output_queue: &mut queue,
        write_half: &mut writer,
        paused_panes: &mut paused,
        flags: &mut flags,
        unrestricted_read_demand: &mut unrestricted,
        attached_client_read_presence: &mut presence,
        deferred: &mut deferred,
    };
    // Establish a real cursor and acknowledge the previously delivered baseline.
    refresh_subscriptions(
        &handler,
        identity,
        Some(&name),
        PaneSubscriptionState {
            subscriptions: context.subscriptions,
            reader_demands: context.reader_demands,
            pane_output_states: context.pane_output_states,
        },
        context.pane_event_tx.clone(),
        PaneSubscriptionStart::Now,
    )
    .await;
    assert!(context.subscriptions.contains_key(&pane_id));
    sender.send(b"BASE\n".to_vec());
    let baseline = tokio::time::timeout(CONTROL_TEST_TIMEOUT, context.pane_event_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let PaneEvent::Data { bytes, .. } = baseline else {
        panic!("baseline data")
    };
    let mut delivered = bytes;
    apply_control_pane_output_actions(
        vec![ControlPaneOutputAction::new(
            pane_id,
            ControlPaneOutputActionKind::Off,
        )],
        &mut context,
    )
    .await;
    assert!(context.subscriptions.is_empty());
    let mut demanded = Box::pin(sender.wait_for_reader_demand(pane_id, None));
    assert!(poll_fn(|cx| Poll::Ready(demanded.as_mut().poll(cx)))
        .await
        .is_pending());
    drop(demanded);

    // Queue actual PTY bytes before any reader starts: they cannot already be
    // retained output. An observation cursor does not acquire read demand.
    let pty = rmux_pty::PtyPair::open().unwrap();
    let (master, slave) = pty.into_split();
    let mut slave = std::fs::File::from(slave.into_owned_fd());
    std::io::Write::write_all(&mut slave, b"UNREAD\n").unwrap();
    let mut observation = sender.subscribe();
    assert!(observation.try_recv().is_none());
    let transcript = PaneTranscript::shared(32, rmux_proto::TerminalSize { cols: 80, rows: 24 });

    // The first real await in targeted reconciliation locks HandlerState. Hold
    // it and poll ON once, forcing the disputed wake-before-subscribe interval.
    let lookup_lock = handler.hold_control_subscription_lookup_for_test().await;
    let mut resume = Box::pin(apply_control_pane_output_actions(
        vec![ControlPaneOutputAction::new(
            pane_id,
            ControlPaneOutputActionKind::On,
        )],
        &mut context,
    ));
    assert!(poll_fn(|cx| Poll::Ready(resume.as_mut().poll(cx)))
        .await
        .is_pending());
    let mut demanded = Box::pin(sender.wait_for_reader_demand(pane_id, None));
    let woke_before_lookup = poll_fn(|cx| Poll::Ready(demanded.as_mut().poll(cx)))
        .await
        .is_ready();
    drop(demanded);
    let reader = spawn_pane_output_reader(
        name.clone(),
        rmux_core::PaneId::new(pane_id),
        master,
        transcript,
        sender.clone(),
        None,
        None,
        None,
        PaneReaderRuntime::current().expect("native reader runtime"),
    );
    if woke_before_lookup {
        let published = tokio::time::timeout(CONTROL_TEST_TIMEOUT, observation.recv())
            .await
            .unwrap();
        let OutputCursorItem::Event(event) = published else {
            panic!("unread publication")
        };
        assert!(event.bytes().windows(6).any(|bytes| bytes == b"UNREAD"));
        eprintln!("causal trace: OFF -> PTY queued -> ON wakes -> lookup blocked -> native reader publishes UNREAD -> subscribe");
    } else {
        assert!(observation.try_recv().is_none());
        eprintln!("causal trace: OFF -> PTY queued -> ON lookup blocked; reader demand still pending -> subscribe -> wake");
    }
    drop(lookup_lock);
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, resume)
        .await
        .unwrap();
    if !woke_before_lookup {
        let published = tokio::time::timeout(CONTROL_TEST_TIMEOUT, observation.recv())
            .await
            .unwrap();
        let OutputCursorItem::Event(event) = published else {
            panic!("unread publication")
        };
        assert!(event.bytes().windows(6).any(|bytes| bytes == b"UNREAD"));
    }
    // The next record is a positive cursor barrier; absence of UNREAD is not
    // inferred from a sleep or a missing frame alone.
    sender.send(b"AFTER\n".to_vec());
    tokio::time::timeout(CONTROL_TEST_TIMEOUT, async {
        loop {
            let event = context.pane_event_rx.recv().await.unwrap();
            if let PaneEvent::Data { bytes, epoch, .. } = event {
                if context.pane_output_states.accepts_event(pane_id, epoch) {
                    delivered.extend_from_slice(&bytes);
                    if delivered.windows(5).any(|bytes| bytes == b"AFTER") {
                        break;
                    }
                }
            }
        }
    })
    .await
    .unwrap();
    reader.abort();
    let rendered = String::from_utf8(delivered).unwrap().replace('\r', "");
    assert_eq!(
        rendered, "BASE\nUNREAD\nAFTER\n",
        "native unread bytes must survive resume once, without baseline replay"
    );
}

/// The session pane's own reader publishes on the `sender` this test observes.
/// A login shell prints its prompt whenever it gets the CPU; when that lands
/// while OFF holds reading, ON wakes both readers and the prompt can be the
/// first publication instead of `UNREAD`. A pane that never writes leaves the
/// cursor to this test's bytes alone.
async fn silent_session_pane(
    handler: &RequestHandler,
    name: &SessionName,
) -> (u32, crate::pane_io::PaneOutputSender) {
    let created = handler
        .handle(Request::NewSessionExt(Box::new(
            rmux_proto::NewSessionExtRequest {
                session_name: Some(name.clone()),
                working_directory: None,
                detached: true,
                size: None,
                environment: None,
                group_target: None,
                attach_if_exists: false,
                detach_other_clients: false,
                kill_other_clients: false,
                flags: None,
                window_name: None,
                print_session_info: false,
                print_format: None,
                command: Some(
                    ["/bin/sh", "-c", "exec sleep 600"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                ),
                process_command: None,
                client_environment: None,
                skip_environment_update: false,
            },
        )))
        .await;
    crate::session_fixture::assert_new_session(&created);
    handler
        .control_session_panes(name)
        .await
        .expect("session output exists")
        .into_iter()
        .next()
        .expect("session has a pane")
}
