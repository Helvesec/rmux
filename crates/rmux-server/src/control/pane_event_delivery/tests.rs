use std::future::Future;
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use super::*;
use crate::pane_io::pane_output_channel;

mod completed_block_age_tests;

async fn poll_once(future: Pin<&mut impl Future<Output = ()>>) -> Poll<()> {
    let mut future = future;
    std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

#[tokio::test]
async fn terminal_flush_does_not_admit_pane_data_with_ready_or_blocked_writer() {
    use super::super::{
        handle_server_event, ControlOutputQueue, ControlServerEvent, ServerEventContext,
    };
    use crate::handler::{ControlClientIdentity, RequestHandler};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut terminals = Vec::new();
    for blocked in [false, true] {
        let (events_tx, mut events_rx) = mpsc::channel(8);
        let sender = PaneEventSender::from(events_tx);
        let mut flags = ControlClientFlags {
            pause_after_millis: Some(10_000),
            ..ControlClientFlags::default()
        };
        sender.set_pause_after(flags);
        sender
            .sender
            .send(PaneEvent::Data {
                pane_id: 7,
                epoch: 0,
                bytes: b"late-pane-data".to_vec(),
                received_at: std::time::Instant::now(),
                retention: None,
            })
            .await
            .unwrap();
        assert_eq!(events_rx.len(), 1, "valid pane data must already be queued");
        let mut queue = ControlOutputQueue::default();
        let prefix = if blocked {
            vec![b'p'; 2 * 1024 * 1024]
        } else {
            Vec::new()
        };
        queue.enqueue_stdout(prefix.clone());
        let (stream, mut peer) = tokio::net::UnixStream::pair().unwrap();
        stream.writable().await.unwrap();
        let (_read_half, mut writer) = tokio::io::split(stream);
        let handler = RequestHandler::new();
        let mut paused = Default::default();
        let mut states = Default::default();
        {
            let mut context = ServerEventContext {
                handler: &handler,
                control_identity: ControlClientIdentity::new(42_557, 1),
                requester_pid: 42_557,
                session_name: &mut None,
                subscriptions: &mut Default::default(),
                reader_demands: &mut Default::default(),
                pane_output_states: &mut states,
                format_subscriptions: &mut Default::default(),
                format_subscription_tick: &mut None,
                pane_event_tx: sender.clone(),
                pane_event_rx: &mut events_rx,
                output_queue: &mut queue,
                write_half: &mut writer,
                paused_panes: &mut paused,
                flags: &mut flags,
                unrestricted_read_demand: &mut None,
                attached_client_read_presence: &mut None,
                deferred: &mut Default::default(),
            };
            let exit = handle_server_event(ControlServerEvent::Exit(None), &mut context, false);
            tokio::pin!(exit);
            let first_poll = std::future::poll_fn(|cx| Poll::Ready(exit.as_mut().poll(cx))).await;
            if blocked {
                assert!(
                    first_poll.is_pending(),
                    "native writer must block before exit"
                );
                let drain_prefix = async {
                    let mut bytes = vec![0; prefix.len() + 1];
                    peer.read_exact(&mut bytes).await.unwrap();
                    assert_eq!(&bytes[..prefix.len()], prefix);
                    assert_eq!(bytes[prefix.len()], b'\n');
                };
                let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
                    tokio::join!(exit, drain_prefix)
                })
                .await
                .expect("terminal flush completes after writer unblocks");
                assert!(result.unwrap());
            } else {
                let result = match first_poll {
                    Poll::Ready(result) => result,
                    Poll::Pending => tokio::time::timeout(Duration::from_secs(5), exit)
                        .await
                        .expect("ready writer completes despite cooperative yields"),
                };
                assert!(result.unwrap());
            }
        }
        writer.shutdown().await.unwrap();
        let mut terminal = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), peer.read_to_end(&mut terminal))
            .await
            .unwrap()
            .unwrap();
        terminals.push((blocked, terminal, events_rx.len()));
        assert!(queue.blocks.is_empty());
        assert_eq!(queue.buffered_bytes, 0);
        assert!(paused.is_empty());
    }
    // Exercise both writer states before reporting either transcript failure.
    for (blocked, terminal, pending) in terminals {
        assert_eq!(
            terminal, b"%exit\n",
            "blocked={blocked}: no pane data may follow exit"
        );
        assert_eq!(pending, 1, "terminal flush must leave pane data unadmitted");
    }
}

#[tokio::test]
async fn blocked_flush_releases_copy_budget_before_pause_age() {
    use super::super::{handle_pane_event, ControlOutputQueue, ServerEventContext};
    use crate::handler::{ControlClientIdentity, RequestHandler};
    use std::collections::HashSet;
    use tokio::io::{AsyncBufReadExt, BufReader};

    const CHUNKS: usize = 33;
    const BYTES: usize = 256 * 1024;
    let source = pane_output_channel();
    let receiver = source.subscribe();
    let (events_tx, mut events_rx) = mpsc::channel(8);
    let sender = PaneEventSender::from(events_tx);
    let mut flags = ControlClientFlags {
        pause_after_millis: Some(10_000),
        ..ControlClientFlags::default()
    };
    sender.set_pause_after(flags);
    let (_stop_tx, stop_rx) = oneshot::channel();
    sender.subscribe(7, 0, receiver, source.clone(), stop_rx);
    source.send(vec![b'x'; BYTES]);
    let mut queue = ControlOutputQueue::default();
    let mut paused = HashSet::new();
    let mut states = Default::default();
    handle_pane_event(
        events_rx.recv().await.unwrap(),
        &mut queue,
        &mut paused,
        &mut states,
        flags,
    )
    .unwrap();
    let (stream, peer) = tokio::net::UnixStream::pair().unwrap();
    let (_read_half, mut writer) = tokio::io::split(stream);
    let handler = RequestHandler::new();
    {
        let mut context = ServerEventContext {
            handler: &handler,
            control_identity: ControlClientIdentity::new(42_556, 1),
            requester_pid: 42_556,
            session_name: &mut None,
            subscriptions: &mut Default::default(),
            reader_demands: &mut Default::default(),
            pane_output_states: &mut states,
            format_subscriptions: &mut Default::default(),
            format_subscription_tick: &mut None,
            pane_event_tx: sender.clone(),
            pane_event_rx: &mut events_rx,
            output_queue: &mut queue,
            write_half: &mut writer,
            paused_panes: &mut paused,
            flags: &mut flags,
            unrestricted_read_demand: &mut None,
            attached_client_read_presence: &mut None,
            deferred: &mut Default::default(),
        };
        let flush = context.flush_output();
        tokio::pin!(flush);
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(flush.as_mut().poll(cx)))
                .await
                .is_pending(),
            "native socket must block before publication"
        );
        // Exercise the real 4 MiB copy budget and protected producer cursor.
        // No socket reads occur until all 8.25 MiB have been admitted.
        let produce = async {
            for _ in 1..CHUNKS {
                source.send(vec![b'x'; BYTES]);
                assert!(source.wait_for_control_admission(None).await);
            }
            let barrier = sender.pending_barrier().await;
            drop(barrier);
        };
        tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(3), produce) => {
                assert!(result.is_ok(), "blocked flush exhausted the copy budget before pause age");
            }
            result = &mut flush => panic!("unread socket unexpectedly drained: {result:?}"),
        }
        let drain = async {
            let mut peer = BufReader::new(peer);
            for _ in 0..CHUNKS {
                let mut line = Vec::new();
                peer.read_until(b'\n', &mut line).await.unwrap();
                assert!(line.starts_with(b"%extended-output %7 "));
                assert!(line.ends_with(&[vec![b'x'; BYTES], vec![b'\n']].concat()));
            }
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            let (written, ()) = tokio::join!(flush, drain);
            written.unwrap();
        })
        .await
        .expect("all admitted lines drain intact");
    }
    assert!(paused.is_empty(), "admission must not pause before age");
    assert_eq!(
        sender.pending_budget.available_permits(),
        PENDING_PANE_EVENT_BYTES
    );
    // A completed socket write must admit an already queued pause before
    // draining the remaining backlog, even when every next write is ready.
    queue.enqueue_pane_output(7, b"%extended-output %7 0 : first\n".to_vec(), None);
    queue.enqueue_pane_output(7, b"%extended-output %7 0 : stale\n".to_vec(), None);
    sender
        .sender
        .send(PaneEvent::PauseAfter {
            pane_id: 7,
            epoch: 0,
        })
        .await
        .unwrap();
    let mut ready_writer = Vec::new();
    super::super::flush_output_queue(
        &mut queue,
        &mut ready_writer,
        flags,
        &mut paused,
        Some((&mut events_rx, &mut states)),
    )
    .await
    .unwrap();
    assert_eq!(ready_writer, b"%extended-output %7 0 : first\n%pause %7\n");
    assert!(
        paused.contains(&7),
        "ready writes must not starve the queued pause"
    );
}

#[tokio::test(start_paused = true)]
async fn blocked_output_keeps_pause_age_after_channel_consumption() {
    use super::super::{
        flush_output_queue, handle_pane_event, ControlOutputQueue, ControlPaneOutputStates,
    };
    use std::collections::HashSet;
    use tokio::io::AsyncReadExt;

    let source = pane_output_channel();
    let mut receiver = source.subscribe();
    receiver.protect_control_cursor();
    let (events_tx, mut events_rx) = mpsc::channel(8);
    let sender = PaneEventSender::from(events_tx);
    let flags = ControlClientFlags {
        pause_after_millis: Some(10_000),
        ..ControlClientFlags::default()
    };
    sender.set_pause_after(flags);
    let delivery = Arc::new(Delivery {
        source: source.clone(),
        sent_next: AtomicU64::new(receiver.control_next_sequence()),
        inflight: Mutex::new(BTreeMap::new()),
        fences: Mutex::new(BTreeMap::new()),
        stopped: AtomicBool::new(false),
        progress: sender.progress.clone(),
    });
    sender
        .sources
        .lock()
        .expect("pane delivery sources")
        .push(Arc::downgrade(&delivery));
    let (_stop_tx, stop_rx) = oneshot::channel();
    let task = deliver_pane_events(7, 0, receiver, stop_rx, sender.clone(), delivery.clone());
    tokio::pin!(task);
    source.send(b"blocked-output".to_vec());
    assert!(poll_once(task.as_mut()).await.is_pending());
    let event = events_rx.try_recv().expect("output admitted");
    // Put this admission just before its deadline without aging later output.
    delivery
        .inflight
        .lock()
        .expect("pane admissions")
        .first_entry()
        .expect("retained output")
        .get_mut()
        .received_at = tokio::time::Instant::now().into_std() - Duration::from_millis(9900);
    let mut queue = ControlOutputQueue::default();
    let mut paused = HashSet::new();
    let mut states = ControlPaneOutputStates::default();
    assert!(!handle_pane_event(event, &mut queue, &mut paused, &mut states, flags).unwrap());
    // Command fences concern admission into the output queue, so they must
    // complete even though the client has not yet accepted that output.
    let barrier = tokio::time::timeout(Duration::from_millis(1), sender.pending_barrier())
        .await
        .expect("command admission fence must finish before the write drains");
    drop(barrier);
    let (mut writer, mut reader) = tokio::io::duplex(1);
    let flush = flush_output_queue(&mut queue, &mut writer, flags, &mut paused, None);
    tokio::pin!(flush);
    assert!(
        std::future::poll_fn(|cx| Poll::Ready(flush.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    assert!(
        sender.pause_deadline(&delivery).is_some(),
        "a partial output write must keep the original pause age"
    );
    assert!(poll_once(task.as_mut()).await.is_pending());
    tokio::time::advance(Duration::from_millis(200)).await;
    assert!(poll_once(task.as_mut()).await.is_ready());
    assert!(matches!(
        events_rx
            .try_recv()
            .expect("pause is queued behind the write"),
        PaneEvent::PauseAfter {
            pane_id: 7,
            epoch: 0
        }
    ));
    source.send(b"producer-unblocked".to_vec());
    assert!(source.wait_for_control_admission(None).await);
    let drain = async {
        let mut bytes = Vec::new();
        loop {
            let byte = reader.read_u8().await.unwrap();
            bytes.push(byte);
            if byte == b'\n' {
                return bytes;
            }
        }
    };
    let (written, bytes) = tokio::join!(flush, drain);
    written.expect("the partial control line finishes");
    assert!(bytes.starts_with(b"%extended-output %7 "));
    assert!(bytes.ends_with(b" : blocked-output\n"));
    assert!(sender.pause_deadline(&delivery).is_none());
}

#[tokio::test(start_paused = true)]
async fn drained_before_pause_deadline_survives_late_timer_poll() {
    let source = pane_output_channel();
    let mut receiver = source.subscribe();
    receiver.protect_control_cursor();
    let (events_tx, mut events_rx) = mpsc::channel(8);
    let sender = PaneEventSender::from(events_tx);
    sender.set_pause_after(ControlClientFlags {
        pause_after_millis: Some(10_000),
        ..ControlClientFlags::default()
    });
    let delivery = Arc::new(Delivery {
        source: source.clone(),
        sent_next: AtomicU64::new(receiver.control_next_sequence()),
        inflight: Mutex::new(BTreeMap::new()),
        fences: Mutex::new(BTreeMap::new()),
        stopped: AtomicBool::new(false),
        progress: sender.progress.clone(),
    });
    let (_stop_tx, stop_rx) = oneshot::channel();
    let task = deliver_pane_events(7, 3, receiver, stop_rx, sender.clone(), delivery.clone());
    tokio::pin!(task);

    source.send(b"before-drain".to_vec());
    assert!(poll_once(task.as_mut()).await.is_pending());
    let first = events_rx.try_recv().expect("first output admitted");
    assert!(matches!(&first, PaneEvent::Data { bytes, .. } if bytes == b"before-drain"));
    // Admissions use the real clock. Age only this record to 9.9s so a
    // later admission is not artificially aged by advancing Tokio by 10s.
    delivery
        .inflight
        .lock()
        .expect("pane admissions")
        .first_entry()
        .expect("first output retained")
        .get_mut()
        .received_at = tokio::time::Instant::now().into_std() - Duration::from_millis(9900);
    sender.progress.notify_waiters();
    assert!(poll_once(task.as_mut()).await.is_pending());
    let deadline = sender
        .pause_deadline(&delivery)
        .expect("inflight timer armed");
    assert_eq!(
        deadline - tokio::time::Instant::now(),
        Duration::from_millis(100)
    );
    drop(first);
    assert!(delivery
        .inflight
        .lock()
        .expect("pane admissions")
        .is_empty());
    // Keep delivery unpolled between the final drain at 9.9s and 10.1s.
    // Both its old timer and its progress notification will then be ready.
    tokio::time::advance(Duration::from_millis(200)).await;
    let late_poll = poll_once(task.as_mut()).await;
    assert!(
        matches!(events_rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "a drained client must not receive %pause from its stale timer"
    );
    assert!(late_poll.is_pending(), "delivery keeps its live cursor");

    source.send(b"after-deadline".to_vec());
    assert!(poll_once(task.as_mut()).await.is_pending());
    let next = events_rx
        .try_recv()
        .expect("later output remains subscribed");
    assert!(
        matches!(&next, PaneEvent::Data { bytes, pane_id: 7, epoch: 3, .. }
        if bytes == b"after-deadline")
    );
    drop(next);
}

#[tokio::test]
async fn ready_drain_allows_pause_delivery_before_backlog_finishes() {
    use super::super::{flush_output_queue, ControlOutputQueue, ControlPaneOutputStates};
    use std::collections::HashSet;

    let mut queue = ControlOutputQueue::default();
    let line = b"%extended-output %7 0 : producer-output\n";
    for _ in 0..1024 {
        queue.enqueue_pane_output(7, line.to_vec(), None);
    }
    let (events, mut receiver) = mpsc::channel(8);
    let mut writer = Vec::new();
    let mut paused = HashSet::new();
    let mut states = ControlPaneOutputStates::default();
    // An expired pause task shares the worker with an already writable drain.
    // It must run before that drain has exhausted all of the stale output.
    let pause = async {
        events
            .send(PaneEvent::PauseAfter {
                pane_id: 7,
                epoch: 0,
            })
            .await
            .unwrap();
    };
    let (result, ()) = tokio::join!(
        biased;
        flush_output_queue(
            &mut queue,
            &mut writer,
            ControlClientFlags { pause_after_millis: Some(10_000), ..Default::default() },
            &mut paused,
            Some((&mut receiver, &mut states)),
        ),
        pause,
    );
    result.unwrap();
    assert!(
        paused.contains(&7),
        "ready drain starved the expired pause task"
    );
    assert!(writer.ends_with(b"%pause %7\n"));
    assert!(
        writer.len() < 1024 * line.len(),
        "pause must discard stale backlog"
    );
    assert_eq!(queue.buffered_bytes, 0);
}
