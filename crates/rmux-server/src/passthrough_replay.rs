//! Server-side glue around [`rmux_core::PassthroughReplayLog`].
//!
//! The core type is a pure data structure; this module:
//! * provides a shared `Arc<Mutex<_>>` alias used by the reader, the
//!   handler state, and the attach forwarder;
//! * carries the budget-aware tee that pane readers call on every
//!   byte chunk emitted by the inner PTY in passthrough sessions;
//! * resolves the per-server `passthrough-replay-bytes` budget into a
//!   bytes count when allocating new logs.

use std::sync::{Arc, Mutex};

use rmux_core::{
    OptionStore, PassthroughReplayLog, DEFAULT_PASSTHROUGH_REPLAY_BUDGET,
};
use rmux_proto::OptionName;

use crate::pane_transcript::SharedPaneTranscript;

/// Shared, lockable replay log. One per pane in a passthrough session.
pub(crate) type SharedPassthroughReplayLog = Arc<Mutex<PassthroughReplayLog>>;

/// Resolves the configured server-scope replay budget. Falls back to
/// the core default if the option is unset or non-numeric.
pub(crate) fn resolve_replay_budget(options: &OptionStore) -> usize {
    options
        .resolve(None, OptionName::PassthroughReplayBytes)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_PASSTHROUGH_REPLAY_BUDGET)
}

/// Allocates a fresh per-pane replay log sized to the configured budget.
#[must_use]
pub(crate) fn new_shared_log(options: &OptionStore) -> SharedPassthroughReplayLog {
    Arc::new(Mutex::new(PassthroughReplayLog::new(resolve_replay_budget(
        options,
    ))))
}

/// Tees a slice of inner-PTY bytes into the pane's replay log and, if
/// the log has exceeded budget, refreshes its snapshot from the
/// pane's current grid state.
///
/// Cheap no-op when `log` is `None` — callers can pass through
/// regardless of whether the owning session is passthrough.
pub(crate) fn append_to_log(
    log: Option<&SharedPassthroughReplayLog>,
    transcript: &SharedPaneTranscript,
    bytes: &[u8],
) {
    let Some(log) = log else {
        return;
    };
    let mut log = log
        .lock()
        .expect("passthrough replay log mutex must not be poisoned");
    log.append(bytes);
    if log.over_budget() {
        let screen = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .clone_screen();
        log.refresh_snapshot(&screen);
    }
}
