use super::super::super::{flush_output_queue, handle_pane_event, ControlOutputQueue};
use super::*;
use std::collections::HashSet;
use std::io;
use std::task::Context;
use tokio::io::AsyncWrite;

struct PublishAfterSecondWrite {
    bytes: Vec<u8>,
    writes: usize,
    event: Option<PaneEvent>,
    events: mpsc::Sender<PaneEvent>,
    written: std::sync::mpsc::Sender<()>,
}

impl AsyncWrite for PublishAfterSecondWrite {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.bytes.extend_from_slice(bytes);
        self.writes += 1;
        if self.writes == 2 {
            let event = self.event.take().unwrap();
            self.events.try_send(event).unwrap();
            self.written.send(()).unwrap();
        }
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Asserts one `%extended-output` line, bounding its age by `max_age`.
/// The age is real milliseconds since the copy, so it is not a constant.
fn assert_extended_output(line: &[u8], pane_id: u32, payload: &str, max_age: Duration) {
    let text = String::from_utf8_lossy(line);
    let (age, rest) = text
        .strip_prefix(&format!("%extended-output %{pane_id} "))
        .and_then(|rest| rest.split_once(' '))
        .unwrap_or_else(|| panic!("not pane {pane_id} extended output: {text:?}"));
    let age: u128 = age
        .parse()
        .unwrap_or_else(|_| panic!("age is not decimal: {text:?}"));
    assert!(age <= max_age.as_millis(), "age exceeds the test: {text:?}");
    assert_eq!(rest, format!(": {payload}\n"), "{text:?}");
}

#[tokio::test(start_paused = true)]
async fn second_completed_block_releases_age_before_other_pane_drain() {
    let started = Instant::now();
    let flags = ControlClientFlags {
        pause_after_millis: Some(10_000),
        ..Default::default()
    };
    let source = pane_output_channel();
    let mut receiver = source.subscribe();
    receiver.protect_control_cursor();
    let (events_tx, mut events_rx) = mpsc::channel(1);
    let sender = PaneEventSender::from(events_tx.clone());
    sender.set_pause_after(flags);
    let delivery = Arc::new(Delivery {
        source: source.clone(),
        sent_next: AtomicU64::new(receiver.control_next_sequence()),
        inflight: Mutex::new(BTreeMap::new()),
        fences: Mutex::new(BTreeMap::new()),
        stopped: AtomicBool::new(false),
        progress: sender.progress.clone(),
    });
    let (_stop_tx, stop_rx) = oneshot::channel();
    let task = deliver_pane_events(7, 0, receiver, stop_rx, sender.clone(), delivery.clone());
    tokio::pin!(task);
    let mut queue = ControlOutputQueue::default();
    let mut paused = HashSet::new();
    let mut states = Default::default();
    for bytes in [b"A1", b"A2"] {
        source.send(bytes.to_vec());
        assert!(poll_once(task.as_mut()).await.is_pending());
        handle_pane_event(
            events_rx.try_recv().unwrap(),
            &mut queue,
            &mut paused,
            &mut states,
            flags,
        )
        .unwrap();
    }
    assert_eq!(delivery.inflight.lock().unwrap().len(), 2);
    for output in delivery.inflight.lock().unwrap().values_mut() {
        output.received_at = tokio::time::Instant::now().into_std() - Duration::from_millis(9900);
    }
    sender.progress.notify_waiters();
    assert!(poll_once(task.as_mut()).await.is_pending());
    assert_eq!(
        sender.pause_deadline(&delivery).unwrap() - tokio::time::Instant::now(),
        Duration::from_millis(100)
    );

    let other = Arc::new(Delivery {
        source: pane_output_channel(),
        sent_next: AtomicU64::new(1),
        inflight: Mutex::new(BTreeMap::from([(
            0,
            RetainedOutput {
                received_at: Instant::now(),
                admission: None,
            },
        )])),
        fences: Mutex::new(BTreeMap::new()),
        stopped: AtomicBool::new(false),
        progress: Arc::new(Notify::new()),
    });
    let event = PaneEvent::Data {
        pane_id: 8,
        epoch: 0,
        bytes: b"B".to_vec(),
        received_at: Instant::now(),
        retention: Some(PaneEventRetention {
            delivery: other.clone(),
            sequence: 0,
        }),
    };
    // Hold B admission while a separate runtime drains it after writing A2.
    // Channel capacity returning proves B was received, past A2's age release.
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let locker = std::thread::spawn(move || {
        let _guard = other.inflight.lock().unwrap();
        locked_tx.send(()).unwrap();
        let _ = release_rx.recv();
    });
    locked_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (written_tx, written_rx) = std::sync::mpsc::channel();
    let writer_events = events_tx.clone();
    let flush = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let mut writer = PublishAfterSecondWrite {
            bytes: Vec::new(),
            writes: 0,
            event: Some(event),
            events: writer_events,
            written: written_tx,
        };
        let result = runtime.block_on(flush_output_queue(
            &mut queue,
            &mut writer,
            flags,
            &mut paused,
            Some((&mut events_rx, &mut states)),
        ));
        (result, writer.bytes, queue.buffered_bytes, paused)
    });
    written_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("A1 and A2 written");
    let wait_until = Instant::now() + Duration::from_secs(5);
    while events_tx.capacity() == 0 && Instant::now() < wait_until {
        std::thread::sleep(Duration::from_millis(1));
    }
    let draining_b = events_tx.capacity() == 1;
    let completed_age_released = delivery.inflight.lock().unwrap().is_empty();
    tokio::time::advance(Duration::from_millis(200)).await;
    let live_cursor = poll_once(task.as_mut()).await.is_pending();
    // Always release the synchronous drain and join before asserting a failure.
    release_tx.send(()).unwrap();
    locker.join().unwrap();
    let (result, bytes, buffered, paused) = flush.join().unwrap();
    result.unwrap();
    assert!(
        draining_b,
        "B must be received before polling A's late timer"
    );
    assert!(
        completed_age_released,
        "fully written A2 kept its age during B drain"
    );
    assert!(live_cursor, "late timer must preserve A's live cursor");
    assert!(
        !paused.contains(&7),
        "completed output must not emit a stale pause"
    );
    let max_age = started.elapsed();
    let lines = bytes
        .split_inclusive(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 3, "{:?}", String::from_utf8_lossy(&bytes));
    assert_extended_output(lines[0], 7, "A1", max_age);
    assert_extended_output(lines[1], 7, "A2", max_age);
    assert_extended_output(lines[2], 8, "B", max_age);
    assert_eq!(buffered, 0);
}
