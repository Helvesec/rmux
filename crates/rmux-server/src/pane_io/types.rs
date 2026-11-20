mod attach;
mod events;
mod output_state;
mod read_demand;
mod receiver;
mod sender;
#[cfg(test)]
mod tests;

#[cfg(any(unix, windows))]
pub(crate) use attach::LiveAttachInputContext;
pub(super) use attach::OpenAttachTarget;
pub(crate) use attach::{AttachSessionUpgrade, AttachTarget, HandleOutcome};
pub(crate) use events::{
    PaneAlertCallback, PaneAlertEvent, PaneEffectSink, PaneExitCallback, PaneExitEvent,
};

mod publication;

#[cfg(all(test, windows))]
mod windows_reader_tests;

#[cfg(all(test, windows))]
mod windows_reader_probe;

use rmux_core::events::{OutputCursor, OutputCursorItem, OutputEvent, OutputRing};
#[cfg(test)]
use rmux_core::events::{DEFAULT_OUTPUT_RING_CAPACITY, DEFAULT_RECENT_LIVE_BUFFER_CAPACITY};
use rmux_core::TerminalPassthrough;
#[cfg(test)]
use rmux_proto::TerminalSize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
#[cfg(windows)]
use std::sync::Condvar;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};
use tokio::sync::{mpsc, Notify};

mod overlay_frame;
pub(crate) use overlay_frame::OverlayFrame;

#[derive(Clone)]
pub(crate) struct PaneOutputSender {
    inner: Arc<PaneOutputInner>,
}

#[derive(Debug, Default)]
struct PaneOutputReadWake {
    #[cfg(windows)]
    state: Mutex<()>,
    #[cfg(windows)]
    condvar: Condvar,
}

#[derive(Debug)]
struct PaneOutputDemandCounter {
    count: AtomicUsize,
    notify: Notify,
    wake: Arc<PaneOutputReadWake>,
}

#[derive(Debug)]
struct AttachedClientReadDemand {
    clients: Mutex<HashMap<u64, HashSet<u32>>>,
    next_id: AtomicU64,
    notify: Notify,
    wake: Arc<PaneOutputReadWake>,
}

/// Shared read demand for all pane readers owned by one request handler.
///
/// An unrestricted demand is used by a normal attached client, a control
/// client with `no-output`, or a headless control client. Attached control
/// clients are counted separately so an explicit `-A ...:off` can suspend a
/// pane only when no other attached consumer permits it.
#[derive(Clone, Debug)]
pub(crate) struct PaneOutputReadDemandRegistry {
    unrestricted: Arc<PaneOutputDemandCounter>,
    attached_clients: Arc<AttachedClientReadDemand>,
}

#[derive(Debug)]
pub(crate) struct PaneOutputReadDemandGuard {
    counter: Arc<PaneOutputDemandCounter>,
}

#[derive(Debug)]
pub(crate) struct PaneOutputAttachedClientReadDemandGuard {
    clients: Arc<AttachedClientReadDemand>,
    id: u64,
}

struct PaneOutputInner {
    state: Mutex<PaneOutputState>,
    #[cfg(unix)]
    control_cursors: Mutex<Vec<std::sync::Weak<AtomicU64>>>,
    #[cfg(unix)]
    control_progress: Notify,
    generation: AtomicU64,
    fast_epoch: AtomicU64,
    receiver_count: AtomicUsize,
    fast_receiver_count: AtomicUsize,
    fast_receivers: Mutex<Vec<mpsc::Sender<FastPaneOutput>>>,
    notify: Notify,
    read_demand: Arc<PaneOutputDemandCounter>,
    global_read_demand: PaneOutputReadDemandRegistry,
    reader_retired: AtomicBool,
    #[cfg(windows)]
    reader_draining: AtomicBool,
    #[cfg(all(test, windows))]
    reader_probe: Arc<windows_reader_probe::WindowsReaderProbe>,
    reader_lifecycle: Notify,
    reader_wake: Arc<PaneOutputReadWake>,
    #[cfg(test)]
    fast_send_pause: Mutex<Option<Arc<FastSendPause>>>,
}

#[cfg(test)]
struct FastSendPause {
    reached: std::sync::Barrier,
    release: std::sync::Barrier,
}

pub(crate) struct PaneOutputReceiver {
    inner: Arc<PaneOutputInner>,
    cursor: OutputCursor,
    #[cfg(unix)]
    control_cursor: Option<Arc<AtomicU64>>,
    passthrough_floor_sequence: u64,
    observed_invalidation_revision: u64,
    observed_process_exit_revision: u64,
    pending_preserved_process_exits: u64,
    pending_observed_output: Option<OutputCursorItem>,
    fast_rx: Option<mpsc::Receiver<FastPaneOutput>>,
}

/// One pane-output item together with the generation observed at its drain
/// linearization point.
///
/// Both fields are captured while holding `PaneOutputInner::state`, the same
/// mutex that orders publication and generation resets. Attach batching can
/// therefore carry this generation through later awaits without relabeling
/// old work as belonging to a replacement process.
#[derive(Debug)]
pub(in crate::pane_io) struct DrainedPaneOutput {
    pub(in crate::pane_io) generation: u64,
    pub(in crate::pane_io) item: OutputCursorItem,
}

#[derive(Debug, Clone)]
struct FastPaneOutput {
    epoch: u64,
    sequence: u64,
    bytes: Arc<[u8]>,
}

struct PaneOutputState {
    ring: OutputRing,
    passthroughs: VecDeque<PaneOutputPassthroughs>,
    retained_passthrough_bytes: usize,
    invalidation_revision: u64,
    process_exit_revision: u64,
    process_exit_revision_at_invalidation: u64,
    last_invalidation_reason: PaneInvalidationReason,
}

struct PaneOutputPassthroughs {
    sequence: u64,
    passthroughs: Vec<TerminalPassthrough>,
}

const PANE_OUTPUT_PASSTHROUGH_CAPACITY: usize = 16;
const PANE_OUTPUT_PASSTHROUGH_BYTE_CAPACITY: usize = 16 * 1024 * 1024;
const FAST_PANE_OUTPUT_MAX_BYTES: usize = 16 * 1024;
const FAST_PANE_OUTPUT_CHANNEL_CAPACITY: usize = 64;

/// Cold-path mutations that make continuation from an older renderer
/// checkpoint unsafe even when no pane-output bytes were published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneInvalidationReason {
    Initial,
    Resize,
    ClearHistory,
    ParserStateExpired,
    TerminalReset,
    TranscriptMutation,
    GenerationChanged,
}

/// One atomic cut through pane process identity, output and invalidations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneBoundary {
    pub(crate) generation: u64,
    pub(crate) next_output_sequence: u64,
    pub(crate) invalidation_revision: u64,
    pub(crate) process_exit_revision: u64,
}

/// An invalidation observed by a recoverable pane-output consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneInvalidation {
    pub(crate) boundary: PaneBoundary,
    pub(crate) reason: PaneInvalidationReason,
}

/// Output plus cold-path invalidations ignored by legacy raw subscribers.
#[derive(Debug)]
pub(crate) enum PaneObservationItem {
    Output(OutputCursorItem),
    Invalidated(PaneInvalidation),
    ProcessExited { output_sequence: Option<u64> },
}

fn generation_matches(current: u64, generation: Option<u64>) -> bool {
    match generation {
        None => true,
        Some(generation) => current == generation,
    }
}

fn fast_output_candidate(
    fast_receiver_count: usize,
    bytes: &Arc<[u8]>,
    passthroughs: &[TerminalPassthrough],
) -> Option<Arc<[u8]>> {
    if fast_receiver_count == 0
        || !passthroughs.is_empty()
        || bytes.len() > FAST_PANE_OUTPUT_MAX_BYTES
    {
        return None;
    }
    Some(Arc::clone(bytes))
}

#[cfg(test)]
pub(crate) fn pane_output_channel() -> PaneOutputSender {
    pane_output_channel_with_read_demand(
        PaneOutputReadDemandRegistry::default(),
        DEFAULT_OUTPUT_RING_CAPACITY,
        DEFAULT_RECENT_LIVE_BUFFER_CAPACITY,
    )
}

#[cfg(test)]
pub(crate) fn pane_output_channel_with_limits(
    event_capacity: usize,
    recent_byte_capacity: usize,
) -> PaneOutputSender {
    pane_output_channel_with_read_demand(
        PaneOutputReadDemandRegistry::default(),
        event_capacity,
        recent_byte_capacity,
    )
}

mod channel;
mod control_cursor;
pub(crate) use channel::pane_output_channel_with_read_demand;

impl PaneOutputSender {
    #[allow(dead_code)]
    pub(crate) fn subscribe_from_sequence(&self, sequence: u64) -> PaneOutputReceiver {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        self.receiver(
            OutputRing::cursor_from_sequence(sequence),
            state.next_sequence(),
            state.invalidation_revision,
            state.process_exit_revision,
            None,
        )
    }

    #[cfg_attr(not(all(any(unix, windows), feature = "web")), allow(dead_code))]
    pub(crate) fn capture_with_next_sequence<T>(&self, capture: impl FnOnce() -> T) -> (u64, T) {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        let captured = capture();
        (state.next_sequence(), captured)
    }
}
