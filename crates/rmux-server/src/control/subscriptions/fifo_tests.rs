use super::*;
use crate::pane_io::pane_output_channel;
use std::future::{poll_fn, Future};
use std::task::Poll;
use std::time::Duration;

fn subscribe(
    pane: u32,
    source: PaneOutputSender,
    sender: PaneEventSender,
    subscriptions: &mut HashMap<u32, PaneSubscription>,
    states: &mut ControlPaneOutputStates,
) {
    states.ensure_pane_epoch(pane);
    reconcile_pane_subscription(
        pane,
        source,
        subscriptions,
        &mut HashMap::new(),
        states,
        sender,
        &PaneSubscriptionStart::Oldest,
    );
}

#[tokio::test]
async fn fifo_budget_multipane_snapshots_stay_below_four_mib() {
    let (tx, mut rx) = mpsc::channel(64);
    let tx = PaneEventSender::from(tx);
    let mut subscriptions = HashMap::new();
    let mut states = ControlPaneOutputStates::default();
    for pane in 0..17 {
        let source = pane_output_channel();
        source.send(vec![b'a' + pane as u8; 256 * 1024]);
        subscribe(pane, source, tx.clone(), &mut subscriptions, &mut states);
    }
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert!(
        rx.len() <= 16,
        "17 x 256 KiB snapshots exceed the client budget: {}",
        rx.len()
    );
    // Release each consumed event and require all 17 snapshots, exactly once.
    let mut panes = HashSet::new();
    for _ in 0..17 {
        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let PaneEvent::Data { pane_id, bytes, .. } = event else {
            panic!("snapshot data")
        };
        assert_eq!(bytes, vec![b'a' + pane_id as u8; 256 * 1024]);
        assert!(panes.insert(pane_id));
    }
    clear_subscriptions(&mut subscriptions);
}

#[tokio::test]
async fn fifo_budget_saturated_channel_retains_until_consumption() {
    let (tx, mut rx) = mpsc::channel(1);
    let tx = PaneEventSender::from(tx);
    let source = pane_output_channel();
    source.send(vec![b'x'; 256 * 1024]);
    let mut subscriptions = HashMap::new();
    subscribe(
        1,
        source,
        tx.clone(),
        &mut subscriptions,
        &mut Default::default(),
    );
    let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let barrier = tx.pending_barrier();
    tokio::pin!(barrier);
    assert!(
        poll_fn(|cx| Poll::Ready(barrier.as_mut().poll(cx)))
            .await
            .is_pending(),
        "channel handoff must not release the retained-byte permit"
    );
    drop(event);
    tokio::time::timeout(Duration::from_secs(2), &mut barrier)
        .await
        .unwrap();
    clear_subscriptions(&mut subscriptions);
}

#[tokio::test]
async fn fifo_budget_barrier_includes_unscheduled_initial_snapshots() {
    let (tx, mut rx) = mpsc::channel(1);
    let tx = PaneEventSender::from(tx);
    let mut subscriptions = HashMap::new();
    let mut states = ControlPaneOutputStates::default();
    for pane in 0..17 {
        let source = pane_output_channel();
        source.send(vec![b'a'; 256 * 1024]);
        subscribe(pane, source, tx.clone(), &mut subscriptions, &mut states);
    }
    let mut queue = ControlOutputQueue::default();
    assert!(!tokio::time::timeout(
        Duration::from_secs(3),
        drain_ready_pane_events(
            &mut rx,
            &tx,
            &mut queue,
            &mut HashSet::new(),
            &mut states,
            ControlClientFlags::default()
        )
    )
    .await
    .unwrap()
    .unwrap());
    assert_eq!(
        queue.blocks.len(),
        17,
        "a response/notification/error barrier must include every earlier snapshot"
    );
    clear_subscriptions(&mut subscriptions);
}

#[tokio::test]
async fn fifo_budget_barrier_fences_later_live_output() {
    let (tx, mut rx) = mpsc::channel(4);
    let tx = PaneEventSender::from(tx);
    let source = pane_output_channel();
    let mut subscriptions = HashMap::new();
    subscribe(
        1,
        source.clone(),
        tx.clone(),
        &mut subscriptions,
        &mut Default::default(),
    );
    source.send(b"BEFORE".to_vec());
    let before = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let barrier = tx.pending_barrier();
    tokio::pin!(barrier);
    assert!(poll_fn(|cx| Poll::Ready(barrier.as_mut().poll(cx)))
        .await
        .is_pending());
    source.send(b"AFTER".to_vec());
    drop(before);
    let fence = tokio::time::timeout(Duration::from_secs(2), &mut barrier)
        .await
        .unwrap();
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        rx.len(),
        0,
        "later live output must not extend or cross the held reply barrier"
    );
    drop(fence);
    let after = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let PaneEvent::Data { bytes, .. } = after else {
        panic!("later live data")
    };
    assert_eq!(bytes, b"AFTER");
    clear_subscriptions(&mut subscriptions);
}

#[tokio::test]
async fn fifo_budget_lagged_event_retains_until_consumption() {
    let (tx, mut rx) = mpsc::channel(1);
    let tx = PaneEventSender::from(tx);
    let source = crate::pane_io::pane_output_channel_with_limits(1, 64);
    let mut subscriptions = HashMap::new();
    subscribe(
        1,
        source.clone(),
        tx.clone(),
        &mut subscriptions,
        &mut Default::default(),
    );
    source.send(b"evicted".to_vec());
    source.send(b"retained".to_vec());
    let lagged = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lagged.pane_id(), 1);
    assert!(!matches!(lagged, PaneEvent::Data { .. }));
    let data = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&data, PaneEvent::Data { bytes, .. } if bytes == b"retained"));
    drop(data);
    let barrier = tx.pending_barrier();
    tokio::pin!(barrier);
    assert!(
        poll_fn(|cx| Poll::Ready(barrier.as_mut().poll(cx)))
            .await
            .is_pending(),
        "lag/error events must keep their admission until consumption too"
    );
    drop(lagged);
    tokio::time::timeout(Duration::from_secs(2), &mut barrier)
        .await
        .unwrap();
    clear_subscriptions(&mut subscriptions);
}

#[tokio::test]
async fn fifo_budget_ready_delivery_allows_a_waiting_peer_to_run() {
    const EVENTS: usize = 2000;
    let (tx, mut rx) = mpsc::channel(4096);
    let tx = PaneEventSender::from(tx);
    let source = pane_output_channel();
    let mut subscriptions = HashMap::new();
    subscribe(
        1,
        source.clone(),
        tx,
        &mut subscriptions,
        &mut Default::default(),
    );
    for index in 0..EVENTS {
        source.send(format!("{index:04}").into_bytes());
    }
    // Receiving the first record positively establishes that delivery started.
    // This task is already runnable then; a drainer must not monopolize its
    // worker until the entire retained ring is sent, starving sibling cursors.
    let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&first, PaneEvent::Data { bytes, .. } if bytes == b"0000"));
    assert!(
        rx.len() < EVENTS - 1,
        "ready delivery exhausted the whole source before its waiting peer ran: {}",
        rx.len()
    );
    clear_subscriptions(&mut subscriptions);
}

// A stalled control cursor must leave its client usable; the native oracle
// exercises the same 32 MiB burst with stdin open and a continuously read peer.
#[tokio::test]
async fn control_cursor_gap_keeps_slow_client_and_fast_peer_alive() {
    let source = pane_output_channel();
    let mut slow = source.subscribe();
    let mut fast = source.subscribe();
    let mut fast_bytes = 0;
    for _ in 0..8192 {
        source.send(vec![b'x'; 4096]);
        let OutputCursorItem::Event(event) = fast.try_recv().expect("fast peer output") else {
            panic!("fast peer must not lose output");
        };
        assert_eq!(event.bytes(), &[b'x'; 4096]);
        fast_bytes += event.byte_len();
    }
    assert_eq!(fast_bytes, 32 * 1024 * 1024);
    let gap = slow.try_recv().expect("stalled cursor reports its gap");
    assert!(matches!(gap, OutputCursorItem::Gap(_)));
    let mut queue = ControlOutputQueue::default();
    let mut paused = HashSet::new();
    let mut states = ControlPaneOutputStates::default();
    let epoch = states.ensure_pane_epoch(7);
    assert!(
        !handle_pane_event(
            cursor_item_event(7, epoch, gap),
            &mut queue,
            &mut paused,
            &mut states,
            ControlClientFlags::default(),
        )
        .expect("gap handling"),
        "cursor retention must not terminate the client"
    );
    queue.enqueue_line(b"%begin 1 1 1\nALIVE\n%end 1 1 1\n".to_vec(), false);
    let (mut writer, mut reader) = tokio::io::duplex(1024);
    super::super::flush_output_queue(
        &mut queue,
        &mut writer,
        ControlClientFlags::default(),
        &mut paused,
        None,
    )
    .await
    .expect("reply flushes");
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    writer.shutdown().await.expect("fixture writer closes");
    let mut rendered = Vec::new();
    reader
        .read_to_end(&mut rendered)
        .await
        .expect("reply reads");
    assert!(!rendered.windows(5).any(|part| part == b"%exit"));
    assert!(rendered.ends_with(b"%begin 1 1 1\nALIVE\n%end 1 1 1\n"));
}
