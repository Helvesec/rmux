use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

// `malloc_trim` is process-global and can dominate interactive PTY latency when
// called after many tiny reads. Keep it as a coarse pressure release for real
// output volume instead of a hot-loop tax on keypress echoes.
pub(super) const PANE_READ_BYTES_BEFORE_HEAP_TRIM: usize = 8 * 1024 * 1024;
const PANE_HEAP_TRIM_MIN_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
struct HeapTrimState {
    pending_bytes: usize,
    last_trim_at: Option<Instant>,
}

fn heap_trim_state() -> &'static Mutex<HeapTrimState> {
    static STATE: OnceLock<Mutex<HeapTrimState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HeapTrimState::default()))
}

pub(super) fn maybe_trim_process_heap_after(bytes: usize) {
    let Ok(mut state) = heap_trim_state().try_lock() else {
        return;
    };
    state.pending_bytes = state.pending_bytes.saturating_add(bytes);
    if state.pending_bytes < PANE_READ_BYTES_BEFORE_HEAP_TRIM {
        return;
    }
    let now = Instant::now();
    if state
        .last_trim_at
        .is_some_and(|last| now.saturating_duration_since(last) < PANE_HEAP_TRIM_MIN_INTERVAL)
    {
        return;
    }
    state.pending_bytes = 0;
    state.last_trim_at = Some(now);
    drop(state);
    drop(tokio::task::spawn_blocking(
        rmux_os::memory::trim_process_heap,
    ));
}
