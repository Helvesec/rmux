use super::*;

#[tokio::test]
async fn pane_reader_demand_is_scoped_to_explicit_pane_off_state() {
    let registry = PaneOutputReadDemandRegistry::default();
    let sender = pane_output_channel_with_read_demand(registry.clone(), 4, 64);
    let attached_client = registry.acquire_attached_client();

    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            sender.wait_for_reader_demand(7, None)
        )
        .await
        .is_ok(),
        "an attached control client with no state for this pane must permit reads"
    );

    attached_client.set_pane_off(7, true);
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(10),
            sender.wait_for_reader_demand(7, None)
        )
        .await
        .is_err(),
        "an explicit off for this pane must suspend reads when it is the only client"
    );

    let pane_consumer = sender.acquire_read_demand();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        sender.wait_for_reader_demand(7, None),
    )
    .await
    .expect("a consuming pane subscription resumes reads");
    drop(pane_consumer);

    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(10),
            sender.wait_for_reader_demand(7, None)
        )
        .await
        .is_err(),
        "dropping the pane consumer restores the explicit off suspension"
    );

    attached_client.set_pane_off(7, false);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        sender.wait_for_reader_demand(7, None),
    )
    .await
    .expect("clearing pane off resumes reads");

    let unrestricted = registry.acquire_unrestricted();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        sender.wait_for_reader_demand(7, None),
    )
    .await
    .expect("no-output and normal attached consumers still permit reads");
    drop(unrestricted);
    drop(attached_client);

    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        sender.wait_for_reader_demand(7, None),
    )
    .await
    .expect("a headless control server keeps pane reads available");
}

#[tokio::test]
async fn pane_reader_demand_requires_every_attached_client_to_be_off() {
    let registry = PaneOutputReadDemandRegistry::default();
    let sender = pane_output_channel_with_read_demand(registry.clone(), 4, 64);
    let first = registry.acquire_attached_client();
    let second = registry.acquire_attached_client();
    first.set_pane_off(11, true);

    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        sender.wait_for_reader_demand(11, None),
    )
    .await
    .expect("one attached client without off state still permits reads");

    second.set_pane_off(11, true);
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(10),
            sender.wait_for_reader_demand(11, None)
        )
        .await
        .is_err(),
        "reads suspend only after every attached client prohibits consumption"
    );
}

#[tokio::test]
async fn pane_reader_demand_wait_ends_when_generation_is_retired() {
    let registry = PaneOutputReadDemandRegistry::default();
    let sender = pane_output_channel_with_read_demand(registry.clone(), 4, 64);
    let client = registry.acquire_attached_client();
    client.set_pane_off(13, true);
    sender.set_generation(1);

    let waiting = {
        let sender = sender.clone();
        tokio::spawn(async move { sender.wait_for_reader_demand(13, Some(1)).await })
    };
    tokio::task::yield_now().await;
    sender.retire_reader_for_generation(Some(1));
    assert!(
        !tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("retirement wake must be bounded")
            .expect("reader wait task joins")
    );

    sender.reset_generation(2);
    client.set_pane_off(13, false);
    assert!(tokio::time::timeout(
        std::time::Duration::from_secs(1),
        sender.wait_for_reader_demand(13, Some(2)),
    )
    .await
    .expect("reset generation reopens the reader boundary"));
}

#[test]
fn stale_generation_output_is_not_published() {
    let sender = pane_output_channel_with_limits(4, 64);
    sender.set_generation(1);
    let mut receiver = sender.subscribe();

    assert_eq!(
        sender.send_for_generation(Some(1), b"old".to_vec()),
        Some(0)
    );
    let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
        panic!("receiver should see the accepted generation");
    };
    assert_eq!(event.sequence(), 0);
    assert_eq!(event.bytes(), b"old");

    sender.reset_generation(2);
    assert_eq!(sender.send_for_generation(Some(1), b"stale".to_vec()), None);
    assert!(
        receiver.try_recv().is_none(),
        "stale generation output must not be retained or delivered"
    );

    assert_eq!(
        sender.send_for_generation(Some(2), b"fresh".to_vec()),
        Some(1)
    );
    let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
        panic!("receiver should see the fresh generation");
    };
    assert_eq!(event.sequence(), 1);
    assert_eq!(event.bytes(), b"fresh");
}

#[test]
fn reset_generation_clears_retention_under_one_generation_invalidation() {
    let sender = pane_output_channel_with_limits(4, 64);
    sender.set_generation(1);
    sender.send(b"old".to_vec());
    let (_, (), mut receiver) = sender.capture_with_observer(|| ());

    sender.reset_generation(2);

    let Some(PaneObservationItem::Invalidated(invalidation)) = receiver.try_recv_observed() else {
        panic!("generation reset must invalidate recoverable observers");
    };
    assert_eq!(
        invalidation.reason,
        PaneInvalidationReason::GenerationChanged
    );
    assert_eq!(invalidation.boundary.generation, 2);
    assert_eq!(invalidation.boundary.next_output_sequence, 1);
    assert!(receiver.try_recv_observed().is_none());
}

#[test]
fn recoverable_receiver_observes_transcript_invalidation_without_output() {
    let sender = pane_output_channel_with_limits(4, 64);
    let transcript =
        crate::pane_transcript::PaneTranscript::shared(100, TerminalSize { cols: 8, rows: 3 });
    let (initial, (), mut receiver) = sender.capture_with_observer(|| ());

    sender.mutate_transcript(&transcript, PaneInvalidationReason::Resize, |transcript| {
        transcript.resize(TerminalSize { cols: 9, rows: 3 });
        ((), true)
    });

    let Some(PaneObservationItem::Invalidated(invalidation)) = receiver.try_recv_observed() else {
        panic!("resize must invalidate a recoverable receiver");
    };
    assert_eq!(invalidation.reason, PaneInvalidationReason::Resize);
    assert_eq!(
        invalidation.boundary.invalidation_revision,
        initial.invalidation_revision + 1
    );
    assert_eq!(
        invalidation.boundary.next_output_sequence,
        initial.next_output_sequence
    );
    assert_eq!(invalidation.boundary.generation, initial.generation);
    assert!(receiver.try_recv_observed().is_none());
}

#[test]
fn no_op_transcript_mutation_does_not_invalidate() {
    let sender = pane_output_channel_with_limits(4, 64);
    let transcript =
        crate::pane_transcript::PaneTranscript::shared(100, TerminalSize { cols: 8, rows: 3 });
    let (_, (), mut receiver) = sender.capture_with_observer(|| ());

    sender.mutate_transcript(&transcript, PaneInvalidationReason::Resize, |_transcript| {
        ((), false)
    });

    assert!(receiver.try_recv_observed().is_none());
}

#[test]
fn process_exit_before_invalidation_is_delivered_before_rebase() {
    let sender = pane_output_channel_with_limits(4, 64);
    let transcript =
        crate::pane_transcript::PaneTranscript::shared(100, TerminalSize { cols: 8, rows: 3 });
    let (_, (), mut receiver) = sender.capture_with_observer(|| ());

    sender.send(Vec::new());
    sender.mutate_transcript(&transcript, PaneInvalidationReason::Resize, |transcript| {
        transcript.resize(TerminalSize { cols: 9, rows: 3 });
        ((), true)
    });

    assert!(matches!(
        receiver.try_recv_observed(),
        Some(PaneObservationItem::ProcessExited { .. })
    ));
    assert!(matches!(
        receiver.try_recv_observed(),
        Some(PaneObservationItem::Invalidated(_))
    ));
}

#[test]
fn retained_process_exit_reports_its_exact_output_sequence() {
    let sender = pane_output_channel_with_limits(4, 64);
    let (_, (), mut receiver) = sender.capture_with_observer(|| ());
    let sequence = sender.send(Vec::new());

    assert!(matches!(
        receiver.try_recv_observed(),
        Some(PaneObservationItem::ProcessExited {
            output_sequence: Some(observed),
        }) if observed == sequence
    ));
    assert!(receiver.try_recv_observed().is_none());
}

#[test]
fn process_exit_after_invalidation_is_delivered_after_rebase() {
    let sender = pane_output_channel_with_limits(4, 64);
    let transcript =
        crate::pane_transcript::PaneTranscript::shared(100, TerminalSize { cols: 8, rows: 3 });
    let (_, (), mut receiver) = sender.capture_with_observer(|| ());

    sender.mutate_transcript(&transcript, PaneInvalidationReason::Resize, |transcript| {
        transcript.resize(TerminalSize { cols: 9, rows: 3 });
        ((), true)
    });
    sender.send(Vec::new());
    assert!(matches!(
        receiver.try_recv_observed(),
        Some(PaneObservationItem::Invalidated(_))
    ));
    let delivered_exit_revision = receiver.observed_process_exit_revision();

    let (_, (), mut replacement) = sender.capture_with_observer(|| ());
    replacement.preserve_process_exits_since(delivered_exit_revision);
    assert!(matches!(
        replacement.try_recv_observed(),
        Some(PaneObservationItem::ProcessExited { .. })
    ));
}

#[test]
fn replacement_receiver_preserves_exit_racing_with_rebase_capture() {
    let sender = pane_output_channel_with_limits(4, 64);
    let transcript =
        crate::pane_transcript::PaneTranscript::shared(100, TerminalSize { cols: 8, rows: 3 });
    let (_, (), mut receiver) = sender.capture_with_observer(|| ());

    sender.mutate_transcript(&transcript, PaneInvalidationReason::Resize, |transcript| {
        transcript.resize(TerminalSize { cols: 9, rows: 3 });
        ((), true)
    });
    assert!(matches!(
        receiver.try_recv_observed(),
        Some(PaneObservationItem::Invalidated(_))
    ));
    let delivered_exit_revision = receiver.observed_process_exit_revision();

    sender.send(Vec::new());
    let (_, (), mut replacement) = sender.capture_with_observer(|| ());
    replacement.preserve_process_exits_since(delivered_exit_revision);

    assert!(matches!(
        replacement.try_recv_observed(),
        Some(PaneObservationItem::ProcessExited { .. })
    ));
    assert!(replacement.try_recv_observed().is_none());
}

#[tokio::test]
async fn invalidation_wakes_recoverable_receiver() {
    let sender = pane_output_channel_with_limits(4, 64);
    let transcript =
        crate::pane_transcript::PaneTranscript::shared(100, TerminalSize { cols: 8, rows: 3 });
    let (_, (), mut receiver) = sender.capture_with_observer(|| ());
    let task = tokio::spawn(async move { receiver.recv_observed().await });
    tokio::task::yield_now().await;

    sender.mutate_transcript(
        &transcript,
        PaneInvalidationReason::ClearHistory,
        |transcript| {
            transcript.clear_history(false);
            ((), true)
        },
    );

    let item = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("invalidation must wake receiver")
        .expect("receiver task");
    assert!(matches!(
        item,
        PaneObservationItem::Invalidated(PaneInvalidation {
            reason: PaneInvalidationReason::ClearHistory,
            ..
        })
    ));
}

#[test]
fn recoverable_receiver_preserves_evicted_process_exit_before_gap() {
    let sender = pane_output_channel_with_limits(2, 64);
    let (_, (), mut receiver) = sender.capture_with_observer(|| ());

    sender.send(Vec::new());
    sender.send(b"one".to_vec());
    sender.send(b"two".to_vec());

    assert!(matches!(
        receiver.try_recv_observed(),
        Some(PaneObservationItem::ProcessExited { .. })
    ));
    assert!(matches!(
        receiver.try_recv_observed(),
        Some(PaneObservationItem::Output(OutputCursorItem::Gap(_)))
    ));
}

#[test]
fn recoverable_receiver_coalesces_evicted_and_retained_exit_markers_once_each() {
    let sender = pane_output_channel_with_limits(2, 64);
    let (_, (), mut receiver) = sender.capture_with_observer(|| ());

    sender.send(Vec::new());
    sender.send(Vec::new());
    sender.send(b"tail".to_vec());

    assert!(matches!(
        receiver.try_recv_observed(),
        Some(PaneObservationItem::ProcessExited { .. })
    ));
    assert!(matches!(
        receiver.try_recv_observed(),
        Some(PaneObservationItem::ProcessExited { .. })
    ));
    assert!(matches!(
        receiver.try_recv_observed(),
        Some(PaneObservationItem::Output(OutputCursorItem::Gap(_)))
    ));
    let Some(PaneObservationItem::Output(OutputCursorItem::Event(event))) =
        receiver.try_recv_observed()
    else {
        panic!("retained non-empty output must follow the gap");
    };
    assert_eq!(event.bytes(), b"tail");
    assert!(receiver.try_recv_observed().is_none());
}

#[test]
fn invalidation_discards_a_pending_pre_rebase_gap() {
    let sender = pane_output_channel_with_limits(1, 64);
    let transcript =
        crate::pane_transcript::PaneTranscript::shared(100, TerminalSize { cols: 8, rows: 3 });
    let (_, (), mut receiver) = sender.capture_with_observer(|| ());
    sender.send(Vec::new());
    sender.send(b"tail".to_vec());
    assert!(matches!(
        receiver.try_recv_observed(),
        Some(PaneObservationItem::ProcessExited { .. })
    ));

    sender.mutate_transcript(&transcript, PaneInvalidationReason::Resize, |transcript| {
        transcript.resize(TerminalSize { cols: 9, rows: 3 });
        ((), true)
    });

    assert!(matches!(
        receiver.try_recv_observed(),
        Some(PaneObservationItem::Invalidated(PaneInvalidation {
            reason: PaneInvalidationReason::Resize,
            ..
        }))
    ));
    assert!(receiver.try_recv_observed().is_none());
}

#[test]
fn live_passthroughs_are_attached_to_existing_receivers() {
    let sender = pane_output_channel_with_limits(4, 64);
    let mut receiver = sender.subscribe();

    sender.send_for_generation_with_passthroughs(
        None,
        b"image".to_vec(),
        vec![TerminalPassthrough::kitty_graphics(1, 2, b"Gf=100;AAAA")],
    );

    let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
        panic!("receiver should see live output");
    };
    assert_eq!(event.bytes(), b"image");
    assert_eq!(event.passthroughs().len(), 1);
    assert_eq!(event.passthroughs()[0].cursor_x(), 1);
    assert_eq!(event.passthroughs()[0].payload(), b"Gf=100;AAAA");
}

#[test]
fn reserved_live_subscription_keeps_passthroughs_emitted_before_transport_open() {
    let sender = pane_output_channel_with_limits(4, 64);
    let (start_sequence, mut receiver) = sender.subscribe_live_from_now();
    assert_eq!(start_sequence, 0);

    sender.send_for_generation_with_passthroughs(
        None,
        b"frame".to_vec(),
        vec![TerminalPassthrough::kitty_graphics(
            0,
            0,
            b"reserved-live-image",
        )],
    );

    let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
        panic!("reserved live receiver should see pre-open output");
    };
    assert_eq!(event.passthroughs().len(), 1);
    assert_eq!(event.passthroughs()[0].payload(), b"reserved-live-image");
}

#[test]
fn detached_output_keeps_recent_recovery_buffer_for_late_waiters() {
    let sender = pane_output_channel_with_limits(4, 64);

    sender.send(b"detached".to_vec());

    {
        let state = sender
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        assert_eq!(state.ring.retained_len(), 1);
        assert_eq!(
            state.ring.recent_len(),
            b"detached".len(),
            "late waiters and lag reports need recent output even when no receiver was live"
        );
    }

    let mut receiver = sender.subscribe_from_oldest();
    let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
        panic!("oldest subscriptions should still replay retained detached events");
    };
    assert_eq!(event.sequence(), 0);
    assert_eq!(event.bytes(), b"detached");
}

#[test]
fn live_output_keeps_recent_recovery_buffer_for_slow_receivers() {
    let sender = pane_output_channel_with_limits(4, 64);
    let _receiver = sender.subscribe();

    sender.send(b"live".to_vec());

    let state = sender
        .inner
        .state
        .lock()
        .expect("pane output state mutex must not be poisoned");
    assert_eq!(state.ring.retained_len(), 1);
    assert_eq!(state.ring.recent_len(), b"live".len());
}

#[test]
fn passthroughs_are_not_replayed_to_oldest_subscribers() {
    let sender = pane_output_channel_with_limits(4, 64);

    sender.send_for_generation_with_passthroughs(
        None,
        b"historic-image".to_vec(),
        vec![TerminalPassthrough::kitty_graphics(0, 0, b"Gf=100;AAAA")],
    );
    let mut receiver = sender.subscribe_from_oldest();

    let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
        panic!("receiver should replay retained bytes");
    };
    assert_eq!(event.bytes(), b"historic-image");
    assert!(
        event.passthroughs().is_empty(),
        "kitty passthrough is live-only and must not replay from retained output"
    );
}

#[test]
fn passthroughs_are_not_retained_without_live_receivers() {
    let sender = pane_output_channel_with_limits(4, 64);

    sender.send_for_generation_with_passthroughs(
        None,
        b"detached-image".to_vec(),
        vec![TerminalPassthrough::kitty_graphics(
            0,
            0,
            vec![b'A'; 1024 * 1024],
        )],
    );

    let state = sender
        .inner
        .state
        .lock()
        .expect("pane output state mutex must not be poisoned");
    assert!(state.passthroughs.is_empty());
    assert_eq!(state.retained_passthrough_bytes, 0);
    assert_eq!(
        state.ring.retained_len(),
        1,
        "plain pane bytes stay replayable"
    );
}

#[test]
fn live_passthrough_retention_has_an_aggregate_byte_budget() {
    let sender = pane_output_channel_with_limits(8, 128);
    let mut receiver = sender.subscribe();
    let payload_bytes = 5 * 1024 * 1024;

    for index in 0..4_u8 {
        sender.send_for_generation_with_passthroughs(
            None,
            vec![index],
            vec![TerminalPassthrough::kitty_graphics(
                0,
                0,
                vec![index; payload_bytes],
            )],
        );
    }

    {
        let state = sender
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        assert!(
            state.retained_passthrough_bytes <= PANE_OUTPUT_PASSTHROUGH_BYTE_CAPACITY,
            "retained live-only side effects must stay within the per-pane byte budget"
        );
        assert_eq!(state.passthroughs.len(), 3);
    }

    let Some(OutputCursorItem::Event(first)) = receiver.try_recv() else {
        panic!("receiver should see the first pane-output event");
    };
    assert!(
        first.passthroughs().is_empty(),
        "the byte budget should evict the oldest side effect"
    );
    let mut last = first;
    while let Some(OutputCursorItem::Event(event)) = receiver.try_recv() {
        last = event;
    }
    assert_eq!(last.passthroughs().len(), 1);
    assert_eq!(last.passthroughs()[0].payload().len(), payload_bytes);
}

#[test]
fn live_passthrough_retention_is_bounded() {
    let sender = pane_output_channel_with_limits(PANE_OUTPUT_PASSTHROUGH_CAPACITY + 2, 1024);
    let mut receiver = sender.subscribe();

    for index in 0..=PANE_OUTPUT_PASSTHROUGH_CAPACITY {
        sender.send_for_generation_with_passthroughs(
            None,
            format!("event-{index}").into_bytes(),
            vec![TerminalPassthrough::kitty_graphics(
                0,
                0,
                format!("Gf=100;{index}").into_bytes(),
            )],
        );
    }

    let Some(OutputCursorItem::Event(first)) = receiver.try_recv() else {
        panic!("receiver should see the first retained event");
    };
    assert_eq!(first.sequence(), 0);
    assert!(
        first.passthroughs().is_empty(),
        "old live passthrough side effects should be dropped when the bounded queue rotates"
    );

    let mut latest = first;
    while let Some(OutputCursorItem::Event(event)) = receiver.try_recv() {
        latest = event;
    }
    assert_eq!(latest.sequence(), PANE_OUTPUT_PASSTHROUGH_CAPACITY as u64);
    assert_eq!(latest.passthroughs().len(), 1);
}

#[test]
fn receiver_count_tracks_live_subscribers() {
    let sender = pane_output_channel_with_limits(4, 64);
    assert_eq!(sender.inner.receiver_count.load(Ordering::Relaxed), 0);
    assert_eq!(sender.inner.fast_receiver_count.load(Ordering::Relaxed), 0);

    let first = sender.subscribe();
    assert_eq!(sender.inner.receiver_count.load(Ordering::Relaxed), 1);
    assert_eq!(sender.inner.fast_receiver_count.load(Ordering::Relaxed), 0);
    {
        let _second = sender.subscribe_from_oldest();
        assert_eq!(sender.inner.receiver_count.load(Ordering::Relaxed), 2);
        assert_eq!(sender.inner.fast_receiver_count.load(Ordering::Relaxed), 0);
        let _live = sender.subscribe_live_from_sequence(0);
        assert_eq!(sender.inner.receiver_count.load(Ordering::Relaxed), 3);
        assert_eq!(sender.inner.fast_receiver_count.load(Ordering::Relaxed), 1);
    }
    assert_eq!(sender.inner.receiver_count.load(Ordering::Relaxed), 1);
    assert_eq!(sender.inner.fast_receiver_count.load(Ordering::Relaxed), 0);

    drop(first);
    assert_eq!(sender.inner.receiver_count.load(Ordering::Relaxed), 0);
    assert_eq!(sender.inner.fast_receiver_count.load(Ordering::Relaxed), 0);
}

#[test]
fn dropped_live_receivers_are_pruned_without_new_output() {
    let sender = pane_output_channel_with_limits(4, 64);

    for _ in 0..32 {
        drop(sender.subscribe_live_from_sequence(0));
    }

    assert_eq!(sender.inner.receiver_count.load(Ordering::Relaxed), 0);
    assert_eq!(sender.inner.fast_receiver_count.load(Ordering::Relaxed), 0);
    assert!(
        sender
            .inner
            .fast_receivers
            .lock()
            .expect("pane output fast receiver list must not be poisoned")
            .is_empty(),
        "closed fast senders must not accumulate while panes are quiet"
    );
}

#[tokio::test]
async fn live_receiver_consumes_fast_output() {
    let sender = pane_output_channel_with_limits(4, 64);
    let mut receiver = sender.subscribe_live_from_sequence(0);

    assert_eq!(sender.send(b"fast".to_vec()), 0);

    let item = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
        .await
        .expect("live receiver should not block on fast output");
    let OutputCursorItem::Event(event) = item else {
        panic!("live receiver should see the fast output event");
    };
    assert_eq!(event.sequence(), 0);
    assert_eq!(event.bytes(), b"fast");
    assert_eq!(receiver.cursor().next_sequence(), 1);
}

#[tokio::test]
async fn live_receiver_uses_bounded_ring_for_large_output() {
    let sender = pane_output_channel_with_limits(4, 64);
    let mut receiver = sender.subscribe_live_from_sequence(0);
    let large = vec![b'x'; FAST_PANE_OUTPUT_MAX_BYTES + 1];

    assert_eq!(sender.send(large.clone()), 0);
    assert!(
        receiver
            .fast_rx
            .as_mut()
            .expect("live receiver should have fast channel")
            .try_recv()
            .is_err(),
        "large output must not be duplicated into the fast receiver queue"
    );

    let item = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
        .await
        .expect("live receiver should fall back to retained output");
    let OutputCursorItem::Event(event) = item else {
        panic!("large retained event should be delivered from the bounded ring");
    };
    assert_eq!(event.sequence(), 0);
    assert_eq!(event.bytes(), large.as_slice());
    assert_eq!(receiver.cursor().next_sequence(), 1);
}

#[test]
fn live_fast_output_queue_capacity_is_bounded() {
    let sender = pane_output_channel_with_limits(256, 64);
    let receiver = sender.subscribe_live_from_sequence(0);

    for _ in 0..(FAST_PANE_OUTPUT_CHANNEL_CAPACITY + 8) {
        sender.send(b"x".to_vec());
    }

    let fast_rx = receiver.fast_rx.as_ref().expect("live receiver");
    assert_eq!(fast_rx.len(), FAST_PANE_OUTPUT_CHANNEL_CAPACITY);
    assert_eq!(fast_rx.max_capacity(), FAST_PANE_OUTPUT_CHANNEL_CAPACITY);
}

#[test]
fn live_receiver_batches_fast_output_before_retained_fallback() {
    let sender = pane_output_channel_with_limits(256, 64);
    let mut receiver = sender.subscribe_live_from_sequence(0);

    for byte in [b'a', b'b', b'c'] {
        assert_eq!(sender.send(vec![byte]), u64::from(byte - b'a'));
    }

    let batch = receiver.try_recv_batch(8);
    let events = batch
        .into_iter()
        .map(|item| {
            let OutputCursorItem::Event(event) = item else {
                panic!("fast output should not report a gap");
            };
            (event.sequence(), event.bytes().to_vec())
        })
        .collect::<Vec<_>>();

    assert_eq!(
        events,
        vec![(0, b"a".to_vec()), (1, b"b".to_vec()), (2, b"c".to_vec())]
    );
    assert_eq!(receiver.cursor().next_sequence(), 3);
    assert!(
        receiver.try_recv().is_none(),
        "batching fast output must not replay the same retained events"
    );
}

#[tokio::test]
async fn live_fast_output_is_invalidated_when_retained_output_is_cleared() {
    let sender = pane_output_channel_with_limits(4, 64);
    let mut receiver = sender.subscribe_live_from_sequence(0);

    assert_eq!(sender.send(b"stale".to_vec()), 0);
    sender.clear_retained();
    assert_eq!(sender.send(b"fresh".to_vec()), 1);

    let item = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
        .await
        .expect("live receiver should report the retained-output gap");
    let OutputCursorItem::Gap(gap) = item else {
        panic!("stale fast output must not be delivered after clear_retained");
    };
    assert_eq!(gap.expected_sequence(), 0);
    assert_eq!(gap.resume_sequence(), 1);

    let item = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
        .await
        .expect("live receiver should resume with fresh output");
    let OutputCursorItem::Event(event) = item else {
        panic!("live receiver should resume with fresh output");
    };
    assert_eq!(event.sequence(), 1);
    assert_eq!(event.bytes(), b"fresh");
}

#[test]
fn fast_output_captured_before_clear_is_rejected_after_clear() {
    let sender = pane_output_channel_with_limits(4, 64);
    let mut receiver = sender.subscribe_live_from_sequence(0);
    let stale_epoch = sender.inner.fast_epoch.load(Ordering::Acquire);

    sender.clear_retained();
    assert!(sender.try_send_fast_output(stale_epoch, 0, Arc::<[u8]>::from(&b"stale"[..]),));

    assert!(
        receiver.try_recv_fast().is_none(),
        "a fast item captured before the retained-output boundary must be rejected"
    );
    assert_eq!(receiver.cursor().next_sequence(), 0);
}

fn assert_clear_invalidates_paused_fast_send(publish_with_side_effects: bool) {
    let sender = pane_output_channel_with_limits(4, 64);
    let mut receiver = sender.subscribe_live_from_sequence(0);
    let pause = sender.install_fast_send_pause();
    let publisher = sender.clone();
    let publishing = std::thread::spawn(move || {
        if publish_with_side_effects {
            publisher
                .publish_for_generation(None, b"stale".to_vec(), |_| ((), Vec::new()))
                .map(|(sequence, ())| sequence)
        } else {
            publisher.send_for_generation(None, b"stale".to_vec())
        }
    });

    pause.reached.wait();
    sender.clear_retained();
    pause.release.wait();
    assert_eq!(publishing.join().expect("publisher thread joins"), Some(0));

    assert!(
        receiver.try_recv_fast().is_none(),
        "clear must invalidate the paused old-generation fast item"
    );
    let Some(OutputCursorItem::Gap(gap)) = receiver.try_recv() else {
        panic!("clear must replace the old-generation retained item with a gap");
    };
    assert_eq!(gap.expected_sequence(), 0);
    assert_eq!(gap.resume_sequence(), 1);
}

#[test]
fn push_captures_fast_epoch_before_releasing_output_state() {
    assert_clear_invalidates_paused_fast_send(false);
}

#[test]
fn publish_captures_fast_epoch_before_releasing_output_state() {
    assert_clear_invalidates_paused_fast_send(true);
}
