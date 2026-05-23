//! Per-window byte log + grid-derived snapshot for passthrough sessions.
//!
//! In a passthrough session each window keeps a bounded raw byte log of what
//! its inner PTY emitted since some safe checkpoint. When the active window
//! changes (or the client (re)attaches), the log is replayed verbatim so the
//! host terminal redraws the window's recent history.
//!
//! Raw byte slices are not safely truncatable mid-stream: a cut could land
//! inside a CSI sequence, after an unbalanced `?1049h`, or assume SGR state
//! implied by earlier bytes. So on overflow we don't drop oldest bytes; we
//! synthesise a fresh **snapshot prefix** from the live screen grid (a reset
//! plus a repaint of the current visible state) and start the raw log over.
//!
//! Replay = `[snapshot] ++ [raw_log_since_snapshot]`.
//!
//! The snapshot only restores the visible viewport. Scrollback that fell off
//! the snapshot horizon is gone — accepted trade-off, since the host
//! terminal's own scrollback covers the live stream depth.

use rmux_proto::{OptionName, RmuxError};

use crate::grid::GridRenderOptions;
use crate::identity::SessionName;
use crate::options::OptionStore;
use crate::screen::Screen;
use crate::transcript::ScreenCaptureRange;

/// Default per-window replay budget (1 MiB) — typically holds hours of a
/// streaming TUI like `claude`.
pub const DEFAULT_PASSTHROUGH_REPLAY_BUDGET: usize = 1024 * 1024;

/// Returns true if `session_name` resolves the `passthrough` option to `on`.
///
/// A session in passthrough mode has different attach semantics (no
/// alt-screen on the host terminal, no chrome, inner PTY bytes forwarded
/// verbatim) and disallows pane operations entirely. The option is set at
/// session creation and is treated as immutable thereafter.
#[must_use]
pub fn is_passthrough_session(options: &OptionStore, session_name: &SessionName) -> bool {
    matches!(
        options.resolve(Some(session_name), OptionName::Passthrough),
        Some("on")
    )
}

/// Returns `Err` if the addressed session is in passthrough mode, with an
/// error message naming the rejected operation.
///
/// Use this at the top of pane-operation handlers (split-window,
/// select-pane, swap-pane, kill-pane, break-pane, join-pane, pipe-pane,
/// resize-pane, display-panes, select-layout, next-layout,
/// previous-layout) so passthrough sessions get a stable rejection
/// instead of silently corrupting their single-pane invariants.
pub fn reject_pane_op_if_passthrough(
    options: &OptionStore,
    session_name: &SessionName,
    op: &str,
) -> Result<(), RmuxError> {
    if is_passthrough_session(options, session_name) {
        Err(RmuxError::Message(format!(
            "{op}: not available in passthrough sessions"
        )))
    } else {
        Ok(())
    }
}

/// A bounded byte log that survives mid-stream truncation by replacing the
/// dropped prefix with a snapshot of the screen state at truncation time.
#[derive(Debug, Clone, Default)]
pub struct PassthroughReplayLog {
    snapshot: Vec<u8>,
    raw_log: Vec<u8>,
    budget: usize,
}

impl PassthroughReplayLog {
    /// Creates an empty log with the given raw-log budget in bytes.
    ///
    /// A zero budget means "snapshot only, never retain raw bytes" — every
    /// append forces a snapshot refresh. Useful for tests and pathological
    /// configurations; in practice use [`DEFAULT_PASSTHROUGH_REPLAY_BUDGET`].
    #[must_use]
    pub fn new(budget: usize) -> Self {
        Self {
            snapshot: Vec::new(),
            raw_log: Vec::new(),
            budget,
        }
    }

    /// Appends raw inner-PTY bytes to the log without any snapshot decisions.
    ///
    /// The caller is responsible for refreshing the snapshot (via
    /// [`Self::refresh_snapshot`]) when [`Self::over_budget`] returns true.
    /// This split keeps the log unaware of `Screen` ownership while letting
    /// the integration layer batch up snapshot work.
    pub fn append(&mut self, bytes: &[u8]) {
        self.raw_log.extend_from_slice(bytes);
    }

    /// Returns true when the raw log has exceeded its budget and a snapshot
    /// refresh is overdue.
    #[must_use]
    pub fn over_budget(&self) -> bool {
        self.raw_log.len() > self.budget
    }

    /// Replaces the snapshot from `screen` and clears the raw log.
    ///
    /// Call this when [`Self::over_budget`] reports true, or proactively at
    /// reset points (e.g. SIGWINCH, explicit clear) to keep replay cheap.
    pub fn refresh_snapshot(&mut self, screen: &Screen) {
        self.snapshot = render_screen_snapshot(screen);
        self.raw_log.clear();
    }

    /// Resets log + snapshot. Used when the inner window is replaced (e.g.
    /// `respawn-pane`) or the window itself is closed.
    pub fn clear(&mut self) {
        self.snapshot.clear();
        self.raw_log.clear();
    }

    /// Returns the bytes a client should write to the host terminal to
    /// reproduce the window's current visible state followed by anything
    /// emitted since the last snapshot.
    ///
    /// Cheap (no allocation if the caller borrows the two slices and writes
    /// them sequentially via [`Self::replay_parts`]).
    #[must_use]
    pub fn replay_bytes(&self) -> Vec<u8> {
        let mut buffer = Vec::with_capacity(self.snapshot.len() + self.raw_log.len());
        buffer.extend_from_slice(&self.snapshot);
        buffer.extend_from_slice(&self.raw_log);
        buffer
    }

    /// Borrows the snapshot and raw log without copying.
    ///
    /// Callers that have a sink (socket, file) should prefer this over
    /// [`Self::replay_bytes`] to avoid one round-trip through the allocator.
    #[must_use]
    pub fn replay_parts(&self) -> (&[u8], &[u8]) {
        (&self.snapshot, &self.raw_log)
    }

    /// Returns the raw log length in bytes. Useful for telemetry/tests.
    #[must_use]
    pub fn raw_log_len(&self) -> usize {
        self.raw_log.len()
    }

    /// Returns the snapshot length in bytes. Useful for telemetry/tests.
    #[must_use]
    pub fn snapshot_len(&self) -> usize {
        self.snapshot.len()
    }

    /// Sets a new budget. Existing log contents are not truncated; the
    /// new budget kicks in on the next [`Self::over_budget`] check.
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
    }
}

/// Renders `screen`'s visible state as a self-contained byte sequence that,
/// when written to a fresh terminal, reproduces the inner program's
/// current display.
///
/// Emits, in order:
/// 1. Exit alt-screen if set (`ESC [ ? 1049 l`) — passthrough never wants
///    the host in alt-screen.
/// 2. Soft reset (`ESC [ ! p`) to drop DEC private modes the previous
///    occupant may have left on.
/// 3. SGR reset (`ESC [ 0 m`).
/// 4. Clear screen + cursor home (`ESC [ 2 J ESC [ H`).
/// 5. The visible viewport captured with ANSI SGR sequences inline.
/// 6. Final cursor position matching `screen.cursor_position()`.
///
/// Note: only the visible viewport is reproduced. Scrollback within the
/// screen grid is not re-emitted — passthrough relies on the host
/// terminal's own scrollback for live history.
#[must_use]
pub fn render_screen_snapshot(screen: &Screen) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b[?1049l");
    out.extend_from_slice(b"\x1b[!p");
    out.extend_from_slice(b"\x1b[m");
    out.extend_from_slice(b"\x1b[2J\x1b[H");

    let range = ScreenCaptureRange::default();
    let options = GridRenderOptions {
        join_wrapped: false,
        with_sequences: true,
        escape_sequences: false,
        include_empty_cells: true,
        trim_spaces: false,
    };
    let viewport = screen.capture_transcript(range, options);
    out.extend_from_slice(&viewport);

    let (cursor_x, cursor_y) = screen.cursor_position();
    out.extend_from_slice(b"\x1b[m");
    out.extend_from_slice(
        format!(
            "\x1b[{};{}H",
            cursor_y.saturating_add(1),
            cursor_x.saturating_add(1),
        )
        .as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_screen::TerminalScreen;
    use rmux_proto::TerminalSize;

    fn terminal_with(bytes: &[u8]) -> TerminalScreen {
        let mut terminal = TerminalScreen::new(TerminalSize { cols: 20, rows: 4 }, 1024);
        terminal.feed(bytes);
        terminal
    }

    #[test]
    fn empty_log_replays_only_snapshot() {
        let mut log = PassthroughReplayLog::new(64);
        log.refresh_snapshot(terminal_with(b"hello").screen());
        let replay = log.replay_bytes();
        assert!(!replay.is_empty());
        assert_eq!(replay, log.snapshot.clone());
    }

    #[test]
    fn append_grows_raw_log_only() {
        let mut log = PassthroughReplayLog::new(64);
        log.append(b"abc");
        log.append(b"def");
        assert_eq!(log.raw_log_len(), 6);
        assert_eq!(log.snapshot_len(), 0);
        assert_eq!(log.replay_bytes(), b"abcdef");
    }

    #[test]
    fn over_budget_triggers_when_raw_log_exceeds_budget() {
        let mut log = PassthroughReplayLog::new(4);
        log.append(b"abcd");
        assert!(!log.over_budget());
        log.append(b"e");
        assert!(log.over_budget());
    }

    #[test]
    fn refresh_clears_raw_log_and_repopulates_snapshot() {
        let mut log = PassthroughReplayLog::new(4);
        log.append(b"abcdefg");
        assert!(log.over_budget());
        log.refresh_snapshot(terminal_with(b"X").screen());
        assert_eq!(log.raw_log_len(), 0);
        assert!(log.snapshot_len() > 0);
        assert!(!log.over_budget());
    }

    #[test]
    fn replay_parts_borrows_without_copy() {
        let mut log = PassthroughReplayLog::new(64);
        log.refresh_snapshot(terminal_with(b"x").screen());
        log.append(b"y");
        let (snapshot, raw) = log.replay_parts();
        assert_eq!(snapshot, log.snapshot.as_slice());
        assert_eq!(raw, b"y");
    }

    #[test]
    fn snapshot_begins_with_safe_reset_sequence() {
        let bytes = render_screen_snapshot(terminal_with(b"hi").screen());
        assert!(bytes.starts_with(b"\x1b[?1049l\x1b[!p\x1b[m\x1b[2J\x1b[H"));
    }

    #[test]
    fn snapshot_ends_with_cursor_positioning() {
        let bytes = render_screen_snapshot(terminal_with(b"hi").screen());
        // The trailing CUP carries the live cursor coordinates as 1-based.
        // After "hi" cursor sits at col 2, row 0 → CSI 1;3 H.
        let tail = std::str::from_utf8(&bytes).expect("ascii cursor tail");
        assert!(tail.ends_with("\x1b[1;3H"), "tail was: {:?}", tail);
    }

    #[test]
    fn clear_zeroes_both_buffers() {
        let mut log = PassthroughReplayLog::new(64);
        log.refresh_snapshot(terminal_with(b"x").screen());
        log.append(b"y");
        log.clear();
        assert_eq!(log.raw_log_len(), 0);
        assert_eq!(log.snapshot_len(), 0);
        assert!(log.replay_bytes().is_empty());
    }

    #[test]
    fn is_passthrough_session_is_false_when_option_unset() {
        let store = OptionStore::new();
        let session = SessionName::new("alpha").expect("valid name");
        assert!(!is_passthrough_session(&store, &session));
    }

    #[test]
    fn is_passthrough_session_is_true_when_session_option_set_to_on() {
        use rmux_proto::SetOptionMode;
        let mut store = OptionStore::new();
        let session = SessionName::new("alpha").expect("valid name");
        store
            .set(
                rmux_proto::ScopeSelector::Session(session.clone()),
                OptionName::Passthrough,
                "on".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("set ok");
        assert!(is_passthrough_session(&store, &session));
    }

    #[test]
    fn reject_pane_op_passes_for_non_passthrough_sessions() {
        let store = OptionStore::new();
        let session = SessionName::new("alpha").expect("valid name");
        reject_pane_op_if_passthrough(&store, &session, "split-window")
            .expect("non-passthrough should allow split-window");
    }

    #[test]
    fn reject_pane_op_errors_with_op_name_for_passthrough_sessions() {
        use rmux_proto::SetOptionMode;
        let mut store = OptionStore::new();
        let session = SessionName::new("alpha").expect("valid name");
        store
            .set(
                rmux_proto::ScopeSelector::Session(session.clone()),
                OptionName::Passthrough,
                "on".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("set ok");
        let error = reject_pane_op_if_passthrough(&store, &session, "split-window")
            .expect_err("passthrough must reject");
        assert_eq!(
            error,
            RmuxError::Message(
                "split-window: not available in passthrough sessions".to_owned()
            )
        );
    }

    #[test]
    fn zero_budget_is_always_over_budget_after_first_byte() {
        let mut log = PassthroughReplayLog::new(0);
        assert!(!log.over_budget());
        log.append(b"x");
        assert!(log.over_budget());
    }
}
