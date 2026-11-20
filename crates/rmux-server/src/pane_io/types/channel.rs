use super::{
    PaneOutputDemandCounter, PaneOutputInner, PaneOutputReadDemandRegistry, PaneOutputSender,
    PaneOutputState,
};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

pub(crate) fn pane_output_channel_with_read_demand(
    global_read_demand: PaneOutputReadDemandRegistry,
    event_capacity: usize,
    recent_byte_capacity: usize,
) -> PaneOutputSender {
    let reader_wake = global_read_demand.reader_wake();
    PaneOutputSender {
        inner: Arc::new(PaneOutputInner {
            state: Mutex::new(PaneOutputState::new(event_capacity, recent_byte_capacity)),
            #[cfg(unix)]
            control_cursors: Mutex::new(Vec::new()),
            #[cfg(unix)]
            control_progress: Notify::new(),
            generation: AtomicU64::new(0),
            fast_epoch: AtomicU64::new(0),
            receiver_count: AtomicUsize::new(0),
            fast_receiver_count: AtomicUsize::new(0),
            fast_receivers: Mutex::new(Vec::new()),
            notify: Notify::new(),
            read_demand: Arc::new(PaneOutputDemandCounter::new(Arc::clone(&reader_wake))),
            global_read_demand,
            reader_retired: AtomicBool::new(false),
            #[cfg(windows)]
            reader_draining: AtomicBool::new(false),
            #[cfg(all(test, windows))]
            reader_probe: Arc::default(),
            reader_lifecycle: Notify::new(),
            reader_wake,
            #[cfg(test)]
            fast_send_pause: Mutex::new(None),
        }),
    }
}
