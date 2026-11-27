use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(super) struct Pause {
    pub(super) reached: tokio::sync::Notify,
    pub(super) release: tokio::sync::Notify,
}

static PAUSES: Mutex<Vec<(String, bool, Arc<Pause>)>> = Mutex::new(Vec::new());

pub(super) fn install(share_id: &str, keyframe: bool) -> Arc<Pause> {
    let pause = Arc::new(Pause::default());
    PAUSES.lock().expect("Web selection delivery pauses").push((
        share_id.to_owned(),
        keyframe,
        Arc::clone(&pause),
    ));
    pause
}

pub(super) async fn before_queue(share_id: &str, keyframe: bool) {
    let pause = {
        let mut pauses = PAUSES.lock().expect("Web selection delivery pauses");
        pauses
            .iter()
            .position(|(id, snapshot, _)| id == share_id && *snapshot == keyframe)
            .map(|index| pauses.swap_remove(index).2)
    };
    if let Some(pause) = pause {
        pause.reached.notify_one();
        pause.release.notified().await;
    }
}

struct RawQueueWitness {
    share_id: String,
    marker: Vec<u8>,
    queued: Arc<tokio::sync::Notify>,
}

static RAW_QUEUE_WITNESSES: Mutex<Vec<RawQueueWitness>> = Mutex::new(Vec::new());

pub(super) fn observe_raw_queue(share_id: &str, marker: &[u8]) -> Arc<tokio::sync::Notify> {
    let queued = Arc::new(tokio::sync::Notify::new());
    RAW_QUEUE_WITNESSES
        .lock()
        .expect("raw queue witnesses")
        .push(RawQueueWitness {
            share_id: share_id.to_owned(),
            marker: marker.to_vec(),
            queued: Arc::clone(&queued),
        });
    queued
}

pub(super) fn raw_queued(share_id: &str, payload: &[u8]) {
    let witness = {
        let mut witnesses = RAW_QUEUE_WITNESSES.lock().expect("raw queue witnesses");
        witnesses
            .iter()
            .position(|witness| {
                witness.share_id == share_id
                    && payload
                        .windows(witness.marker.len())
                        .any(|part| part == witness.marker)
            })
            .map(|index| witnesses.swap_remove(index))
    };
    if let Some(witness) = witness {
        witness.queued.notify_one();
    }
}

// The session loop is held here while its FIFO data watermark reaches the
// client. Unlike pong (a priority control frame), this watermark cannot pass
// previously queued raw frames. No terminal producer can enqueue behind it
// until the test has invalidated the selection and releases the loop.
pub(super) struct DrainBoundary {
    pub(super) marker: Vec<u8>,
    pub(super) release: tokio::sync::Notify,
}

static DRAIN_BOUNDARIES: Mutex<Vec<(String, Arc<DrainBoundary>)>> = Mutex::new(Vec::new());

pub(super) fn install_drain(share_id: &str) -> Arc<DrainBoundary> {
    let boundary = Arc::new(DrainBoundary {
        marker: format!("fixture-drain-{share_id}").into_bytes(),
        release: tokio::sync::Notify::new(),
    });
    DRAIN_BOUNDARIES
        .lock()
        .expect("Web drain boundaries")
        .push((share_id.to_owned(), Arc::clone(&boundary)));
    boundary
}

pub(super) async fn drain_before_pong(
    share_id: &str,
    ping: &[u8],
    outbound: &super::WebSocketOutbound,
) {
    let boundary = {
        let mut boundaries = DRAIN_BOUNDARIES.lock().expect("Web drain boundaries");
        boundaries
            .iter()
            .position(|(id, boundary)| id == share_id && boundary.marker == ping)
            .map(|index| boundaries.swap_remove(index).1)
    };
    if let Some(boundary) = boundary {
        let mut payload = vec![0x01];
        payload.extend_from_slice(&boundary.marker);
        assert_eq!(
            outbound.queue_frame(payload),
            crate::web::outbound::OutboundQueueResult::Queued
        );
        boundary.release.notified().await;
    }
}
