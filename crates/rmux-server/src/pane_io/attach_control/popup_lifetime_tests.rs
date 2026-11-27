//! Reservations must follow the real receive/write consumer, including cancellation.
use super::*;
use crate::pane_io::{attach_transport::AttachTransport, control, wire};
use std::collections::VecDeque;
use std::time::Duration;
use tokio::io::AsyncReadExt;

fn channel() -> (
    AttachControlSender,
    mpsc::UnboundedReceiver<AttachControl>,
    Arc<AtomicUsize>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        tx,
        Arc::clone(&backlog),
        64,
        Arc::new(AtomicBool::new(false)),
    );
    (sender, rx, backlog)
}

fn repaint(generation: u64) -> OverlayFrame {
    OverlayFrame::persistent(vec![generation as u8; 31_244], 3, generation)
}

#[test]
fn popup_in_flight_reservation_survives_take_and_slot_drop() {
    let (sender, mut rx, backlog) = channel();
    sender.send_popup_overlay(7, repaint(1)).unwrap();
    let AttachControl::CoalescedPopupOverlay(slot) = rx.try_recv().unwrap() else {
        panic!("expected replaceable slot");
    };
    let frame = slot.take().unwrap();
    assert!(slot.take().is_none());
    drop(slot);
    assert_eq!(
        backlog.load(Ordering::Acquire),
        1,
        "in-flight bytes stay charged"
    );
    for generation in 2..=512 {
        sender.send_popup_overlay(7, repaint(generation)).unwrap();
        assert_eq!(backlog.load(Ordering::Acquire), 2);
        assert_eq!(rx.len(), 1, "one latest pending frame beside in-flight");
    }
    drop(frame);
    assert_eq!(backlog.load(Ordering::Acquire), 1);
    let latest = control::try_recv_attach_control(&mut rx, &backlog).unwrap();
    assert_eq!(latest.received_backlog_units(), 0);
    let AttachControl::Overlay(ref frame) = latest else {
        panic!("overlay")
    };
    assert_eq!(frame.overlay_generation, 512);
    assert_eq!(backlog.load(Ordering::Acquire), 1);
    drop(rx);
    assert_eq!(
        backlog.load(Ordering::Acquire),
        1,
        "receiver drop cannot release in-flight bytes"
    );
    drop(latest);
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[tokio::test(start_paused = true)]
async fn popup_slow_transport_retains_in_flight_charge_and_latest_frame() {
    let (sender, mut rx, backlog) = channel();
    sender.send_popup_overlay(7, repaint(1)).unwrap();
    let overlay = control::recv_attach_control(&mut VecDeque::new(), Some(&mut rx), &backlog)
        .await
        .unwrap();
    let (server, mut reader) = tokio::io::duplex(1024);
    let writer = tokio::spawn(async move {
        let AttachControl::Overlay(frame) = overlay else {
            panic!("overlay")
        };
        wire::emit_attach_bytes(&AttachTransport::from_io(server), &frame.frame).await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        !writer.is_finished(),
        "full peer backpressures the actual writer"
    );
    assert_eq!(backlog.load(Ordering::Acquire), 1);
    for generation in 2..=128 {
        sender.send_popup_overlay(7, repaint(generation)).unwrap();
        assert_eq!(rx.len(), 1);
        assert_eq!(backlog.load(Ordering::Acquire), 2);
    }
    let expected = rmux_proto::encode_attach_data(&repaint(1).frame).unwrap();
    let mut received = vec![0; expected.len()];
    reader.read_exact(&mut received).await.unwrap();
    writer.await.unwrap().unwrap();
    assert_eq!(
        received, expected,
        "slow reader receives the complete in-flight wire frame"
    );
    assert_eq!(backlog.load(Ordering::Acquire), 1);
    let latest = control::try_recv_attach_control(&mut rx, &backlog).unwrap();
    let AttachControl::Overlay(ref frame) = latest else {
        panic!("overlay")
    };
    assert_eq!(frame.overlay_generation, 128);
    drop(latest);
    assert_eq!(backlog.load(Ordering::Acquire), 0);
    assert!(!sender.is_closed());
}

#[tokio::test(start_paused = true)]
async fn popup_stalled_transport_keeps_existing_five_second_policy() {
    let (sender, mut rx, backlog) = channel();
    sender.send_popup_overlay(7, repaint(1)).unwrap();
    let overlay = control::try_recv_attach_control(&mut rx, &backlog).unwrap();
    let (server, _non_reader) = tokio::io::duplex(1024);
    let writer = tokio::spawn(async move {
        let AttachControl::Overlay(frame) = overlay else {
            panic!("overlay")
        };
        wire::emit_attach_bytes(&AttachTransport::from_io(server), &frame.frame).await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(4)).await;
    assert!(!writer.is_finished());
    assert_eq!(backlog.load(Ordering::Acquire), 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        writer.await.unwrap().unwrap_err().kind(),
        std::io::ErrorKind::TimedOut
    );
    assert_eq!(
        backlog.load(Ordering::Acquire),
        0,
        "timed-out frame releases its charge"
    );
}
