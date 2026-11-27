use std::sync::atomic::Ordering;

use super::{
    subtract_backlog, ControlCmd, DataCmd, OutboundQueueResult, WebSocketOutbound,
    VIEWER_CHANNEL_CAP, WEB_OUTBOUND_BYTES_MAX,
};

#[tokio::test]
async fn data_slot_accounts_backlog_before_publication() {
    let (outbound, mut data_rx, _control_rx) = WebSocketOutbound::test_channels();

    let permit = outbound
        .reserve_data_slot(4)
        .expect("reserve accounted data slot");

    assert_eq!(outbound.backlog_bytes.load(Ordering::Acquire), 4);
    assert!(data_rx.try_recv().is_err());

    permit.send(DataCmd::Frame {
        bytes: vec![b'r', b'm', b'u', b'x'],
        epoch: 0,
    });
    let command = data_rx.recv().await.expect("published data command");
    let DataCmd::Frame { bytes, epoch } = command else {
        panic!("expected frame command");
    };
    assert_eq!(bytes, b"rmux");
    assert_eq!(epoch, 0);
    subtract_backlog(outbound.backlog_bytes.as_ref(), bytes.len());
    assert_eq!(outbound.backlog_bytes.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn dropping_unsent_data_slot_rolls_back_accounting() {
    let (outbound, mut data_rx, _control_rx) = WebSocketOutbound::test_channels();

    let permit = outbound
        .reserve_data_slot(4)
        .expect("reserve accounted data slot");
    assert_eq!(outbound.backlog_bytes.load(Ordering::Acquire), 4);

    drop(permit);

    assert_eq!(outbound.backlog_bytes.load(Ordering::Acquire), 0);
    assert!(data_rx.try_recv().is_err());
}

#[tokio::test]
async fn keyframe_replaces_latest_even_when_data_queue_is_full() {
    let (outbound, _data_rx, mut control_rx) = WebSocketOutbound::test_channels();

    for _ in 0..VIEWER_CHANNEL_CAP {
        assert_eq!(
            outbound.queue_frame(vec![b'x']),
            OutboundQueueResult::Queued
        );
    }
    assert_eq!(outbound.queue_frame(vec![b'y']), OutboundQueueResult::Full);

    assert_eq!(
        outbound.queue_keyframe(vec![vec![b'o', b'l', b'd']]),
        OutboundQueueResult::Queued
    );
    assert_eq!(
        outbound.queue_keyframe(vec![vec![b'n', b'e', b'w']]),
        OutboundQueueResult::Queued
    );

    assert!(matches!(control_rx.try_recv(), Ok(ControlCmd::Keyframe)));
    assert!(control_rx.try_recv().is_err());
    let latest = outbound.latest_keyframe.lock().expect("keyframe lock");
    let latest = latest.as_ref().expect("latest keyframe retained");
    assert_eq!(latest.frames, vec![vec![b'n', b'e', b'w']]);
    assert_eq!(latest.epoch, outbound.latest_epoch.load(Ordering::Acquire));
}

#[tokio::test]
async fn keyframe_then_data_respects_shared_backlog_limit() {
    let (outbound, _data_rx, mut control_rx) = WebSocketOutbound::test_channels();

    assert_eq!(
        outbound.queue_keyframe(vec![vec![0; WEB_OUTBOUND_BYTES_MAX]]),
        OutboundQueueResult::Queued
    );
    assert_eq!(
        outbound.backlog_bytes.load(Ordering::Acquire),
        WEB_OUTBOUND_BYTES_MAX
    );
    assert_eq!(
        outbound.queue_frame(vec![0]),
        OutboundQueueResult::Backpressure
    );
    assert!(matches!(control_rx.try_recv(), Ok(ControlCmd::Keyframe)));
}

#[tokio::test]
async fn replacing_latest_keyframe_transfers_its_backlog_reservation() {
    let (outbound, _data_rx, mut control_rx) = WebSocketOutbound::test_channels();

    assert_eq!(
        outbound.queue_keyframe(vec![vec![b'o'; WEB_OUTBOUND_BYTES_MAX]]),
        OutboundQueueResult::Queued
    );
    assert_eq!(
        outbound.queue_keyframe(vec![vec![b'n'; WEB_OUTBOUND_BYTES_MAX]]),
        OutboundQueueResult::Queued
    );

    assert_eq!(
        outbound.backlog_bytes.load(Ordering::Acquire),
        WEB_OUTBOUND_BYTES_MAX
    );
    assert!(matches!(control_rx.try_recv(), Ok(ControlCmd::Keyframe)));
    assert!(control_rx.try_recv().is_err());
    let latest = outbound.latest_keyframe.lock().expect("keyframe lock");
    assert_eq!(latest.as_ref().expect("latest keyframe").frames[0][0], b'n');
}

#[tokio::test]
async fn in_flight_keyframe_remains_in_the_shared_backlog_budget() {
    let (outbound, _data_rx, mut control_rx) = WebSocketOutbound::test_channels();

    assert_eq!(
        outbound.queue_keyframe(vec![vec![0; WEB_OUTBOUND_BYTES_MAX - 1]]),
        OutboundQueueResult::Queued
    );
    assert!(matches!(control_rx.try_recv(), Ok(ControlCmd::Keyframe)));
    outbound
        .keyframe_wakeup_pending
        .store(false, Ordering::Release);
    let in_flight = outbound
        .latest_keyframe
        .lock()
        .expect("keyframe lock")
        .take()
        .expect("in-flight keyframe");

    assert_eq!(
        outbound.queue_keyframe(vec![vec![0; 2]]),
        OutboundQueueResult::Backpressure
    );
    assert_eq!(
        outbound.queue_keyframe(vec![vec![0]]),
        OutboundQueueResult::Queued
    );
    assert_eq!(
        outbound.backlog_bytes.load(Ordering::Acquire),
        WEB_OUTBOUND_BYTES_MAX
    );

    drop(in_flight);
    assert_eq!(outbound.backlog_bytes.load(Ordering::Acquire), 1);
    drop(
        outbound
            .latest_keyframe
            .lock()
            .expect("keyframe lock")
            .take(),
    );
    assert_eq!(outbound.backlog_bytes.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn snapshots_respect_backlog_byte_limit() {
    let (outbound, _data_rx, _control_rx) = WebSocketOutbound::test_channels();

    assert_eq!(
        outbound.queue_frame(vec![0; WEB_OUTBOUND_BYTES_MAX]),
        OutboundQueueResult::Queued
    );
    assert_eq!(
        outbound.queue_frame(vec![0]),
        OutboundQueueResult::Backpressure
    );
    assert_eq!(
        outbound.queue_snapshot(vec![0]),
        OutboundQueueResult::Backpressure
    );
    assert_eq!(
        outbound.backlog_bytes.load(Ordering::Acquire),
        WEB_OUTBOUND_BYTES_MAX
    );
}

#[tokio::test]
async fn keyframes_respect_backlog_byte_limit_without_advancing_epoch() {
    let (outbound, _data_rx, mut control_rx) = WebSocketOutbound::test_channels();

    assert_eq!(
        outbound.queue_frame(vec![0; WEB_OUTBOUND_BYTES_MAX]),
        OutboundQueueResult::Queued
    );
    assert_eq!(
        outbound.queue_keyframe(vec![vec![0]]),
        OutboundQueueResult::Backpressure
    );

    assert_eq!(outbound.latest_epoch.load(Ordering::Acquire), 0);
    assert!(control_rx.try_recv().is_err());
    let latest = outbound.latest_keyframe.lock().expect("keyframe lock");
    assert!(latest.is_none());
}

#[tokio::test]
async fn oversized_keyframes_are_rejected_without_epoch_gap() {
    let (outbound, _data_rx, mut control_rx) = WebSocketOutbound::test_channels();

    assert_eq!(
        outbound.queue_keyframe(vec![vec![0; WEB_OUTBOUND_BYTES_MAX], vec![0]]),
        OutboundQueueResult::Backpressure
    );

    assert_eq!(outbound.latest_epoch.load(Ordering::Acquire), 0);
    assert!(control_rx.try_recv().is_err());
    let latest = outbound.latest_keyframe.lock().expect("keyframe lock");
    assert!(latest.is_none());
}

#[tokio::test]
async fn write_ping_enqueues_control_ping_with_payload() {
    let (outbound, _data_rx, mut control_rx) = WebSocketOutbound::test_channels();
    let writer = tokio::spawn(async move { outbound.write_ping(b"rmux").await });

    let command = control_rx.recv().await.expect("control command");
    match command {
        ControlCmd::Ping { payload, done } => {
            assert_eq!(payload, b"rmux");
            done.send(Ok(())).expect("complete ping write");
        }
        _ => panic!("expected ping command"),
    }

    writer.await.expect("ping task should not panic").unwrap();
}
