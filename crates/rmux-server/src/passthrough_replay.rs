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

/// Alt-screen *enter* sequence. Forwarded inner-PTY bytes may
/// legitimately contain this (vim, less, htop all toggle alt-screen
/// when run *inside* a passthrough session — that's the user's call).
/// rmux's own server-generated frames must never contain it, or
/// passthrough mode would silently break the host-scrollback promise.
pub(crate) const ALT_SCREEN_ENTER: &[u8] = b"\x1b[?1049h";

/// Alt-screen-exit (`\x1b[?1049l`). Emitted by curses programs (vi,
/// less, htop, nvim) on shutdown — also a strong "fullscreen program
/// just exited" signal: the alt-buffer's contents are about to become
/// irrelevant (the terminal flips back to main), and any in-flight
/// terminal queries are orphaned.
pub(crate) const ALT_SCREEN_EXIT: &[u8] = b"\x1b[?1049l";

/// Returns true if `bytes` contains the alt-screen-enter sequence.
/// Used by debug asserts inside the passthrough forwarder to catch
/// any future code that accidentally emits chrome from the server
/// side instead of forwarding it from the inner PTY.
pub(crate) fn contains_alt_screen_enter(bytes: &[u8]) -> bool {
    bytes
        .windows(ALT_SCREEN_ENTER.len())
        .any(|window| window == ALT_SCREEN_ENTER)
}

/// Returns true if `bytes` contains the alt-screen-exit sequence.
pub(crate) fn contains_alt_screen_exit(bytes: &[u8]) -> bool {
    bytes
        .windows(ALT_SCREEN_EXIT.len())
        .any(|window| window == ALT_SCREEN_EXIT)
}

/// Mirrors the host terminal's alt-screen state by scanning `bytes`
/// for `?1049h` / `?1049l` and applying each toggle in order.
///
/// `bytes` is what the forwarder just emitted to the client (snapshot
/// replay, raw inner-PTY chunk, explicit transition, overlay frame).
/// Each `?1049h` flips the host into alt; each `?1049l` flips it
/// back. The final state after the scan is what the host is in.
///
/// Why scan rather than set explicitly at each emit site: the snapshot
/// prefix and the inner program's own bytes both legitimately contain
/// toggles, and they arrive interleaved in long raw buffers. A single
/// scan keeps one source of truth (the bytes we actually emitted)
/// without forcing every emit site to remember to update the flag.
pub(crate) fn update_host_alt_screen(bytes: &[u8], host_in_alt_screen: &mut bool) {
    debug_assert_eq!(
        ALT_SCREEN_ENTER.len(),
        ALT_SCREEN_EXIT.len(),
        "enter/exit must share length so a single window walk covers both",
    );
    let sequence_len = ALT_SCREEN_ENTER.len();
    if bytes.len() < sequence_len {
        return;
    }
    let mut i = 0;
    while i + sequence_len <= bytes.len() {
        let window = &bytes[i..i + sequence_len];
        if window == ALT_SCREEN_ENTER {
            *host_in_alt_screen = true;
            i += sequence_len;
        } else if window == ALT_SCREEN_EXIT {
            *host_in_alt_screen = false;
            i += sequence_len;
        } else {
            i += 1;
        }
    }
}

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
