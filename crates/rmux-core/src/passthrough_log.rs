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
use crate::input::mode;
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
/// 1. Alt-screen sync: `ESC [ ? 1049 h` when the screen is in alt mode
///    (a TUI like vim/less/htop), otherwise `ESC [ ? 1049 l`. The host
///    must paint into the same buffer the captured grid belongs to —
///    forcing the host to main while the grid was captured from alt
///    paints alt-buffer content into main-buffer scrollback, corrupting
///    the user's history. (Previous versions of this function always
///    emitted `?1049l` and exhibited exactly that bug when a long-running
///    TUI triggered a snapshot refresh on raw-log overflow.)
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
    if screen.is_alternate() {
        out.extend_from_slice(b"\x1b[?1049h");
    } else {
        out.extend_from_slice(b"\x1b[?1049l");
    }
    out.extend_from_slice(b"\x1b[!p");
    out.extend_from_slice(b"\x1b[m");
    // Re-assert DEC private modes that the inner program had set. The
    // soft reset above wiped them; without re-asserting, an attach
    // landing on a mid-session TUI loses mouse reporting / bracketed
    // paste / cursor visibility / modifyOtherKeys, breaking interactive
    // input until the inner program happens to re-emit them on its own
    // (usually never, until SIGWINCH).
    render_dec_modes(screen.mode(), screen.cursor_style(), &mut out);
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

/// Emits the byte sequence required to re-assert the DEC private modes
/// implied by `mode_bits` (and the cursor style implied by
/// `cursor_style`) into `out`.
///
/// The bits map 1:1 to the constants in [`crate::input::mode`]. We
/// emit only what differs from the post-DECSTR defaults, so a fresh
/// shell — which leaves everything at defaults except cursor-visible
/// and autowrap (both on) — produces zero bytes here. A vim/htop /
/// less / fzf session with mouse + bracketed paste + cursor hidden
/// emits the half-dozen toggles needed to make it functional again.
///
/// Order is deliberate: ON-by-default modes first (so an explicit
/// reset can't be re-overridden by a later toggle), then OFF-by-
/// default mode setters, then mouse + modifyOtherKeys families where
/// the higher-numbered variant should win when the inner program had
/// (incorrectly) set multiple.
fn render_dec_modes(mode_bits: u32, cursor_style: u32, out: &mut Vec<u8>) {
    let on = |bit: u32| mode_bits & bit != 0;

    // On-by-default modes: emit reset only when currently off.
    if !on(mode::MODE_CURSOR) {
        out.extend_from_slice(b"\x1b[?25l");
    }
    if !on(mode::MODE_WRAP) {
        out.extend_from_slice(b"\x1b[?7l");
    }

    // Off-by-default mode setters.
    if on(mode::MODE_INSERT) {
        out.extend_from_slice(b"\x1b[4h");
    }
    if on(mode::MODE_KCURSOR) {
        // DECCKM — application cursor keys (arrows emit `\x1bOA` etc.).
        out.extend_from_slice(b"\x1b[?1h");
    }
    if on(mode::MODE_KKEYPAD) {
        // DECPAM — application keypad. Note the `\x1b=` form (no CSI).
        out.extend_from_slice(b"\x1b=");
    }
    if on(mode::MODE_ORIGIN) {
        out.extend_from_slice(b"\x1b[?6h");
    }
    if on(mode::MODE_CRLF) {
        out.extend_from_slice(b"\x1b[?20h");
    }
    if on(mode::MODE_FOCUSON) {
        out.extend_from_slice(b"\x1b[?1004h");
    }
    if on(mode::MODE_BRACKETPASTE) {
        out.extend_from_slice(b"\x1b[?2004h");
    }
    if on(mode::MODE_THEME_UPDATES) {
        out.extend_from_slice(b"\x1b[?2031h");
    }
    if on(mode::MODE_SYNC) {
        out.extend_from_slice(b"\x1b[?2026h");
    }

    // Mouse tracking family. The three "what to track" modes
    // (?1000/1002/1003) are mutually exclusive at the terminal — pick
    // the most-permissive one set. The two "encoding" modes
    // (?1005 UTF-8 / ?1006 SGR) are also mutually exclusive in
    // practice; prefer SGR (modern default) when both are set.
    if on(mode::MODE_MOUSE_ALL) {
        out.extend_from_slice(b"\x1b[?1003h");
    } else if on(mode::MODE_MOUSE_BUTTON) {
        out.extend_from_slice(b"\x1b[?1002h");
    } else if on(mode::MODE_MOUSE_STANDARD) {
        out.extend_from_slice(b"\x1b[?1000h");
    }
    if on(mode::MODE_MOUSE_SGR) {
        out.extend_from_slice(b"\x1b[?1006h");
    } else if on(mode::MODE_MOUSE_UTF8) {
        out.extend_from_slice(b"\x1b[?1005h");
    }

    // modifyOtherKeys (xterm CSI > 4 ; n m). EXTENDED_2 (level 2,
    // sends all keys as CSI u / extended sequences) supersedes
    // EXTENDED (level 1).
    if on(mode::MODE_KEYS_EXTENDED_2) {
        out.extend_from_slice(b"\x1b[>4;2m");
    } else if on(mode::MODE_KEYS_EXTENDED) {
        out.extend_from_slice(b"\x1b[>4;1m");
    }

    // Cursor style (DECSCUSR). 0 = "terminal default" — skip; a
    // non-zero value names a specific shape the inner program asked
    // for (blinking block, steady underline, bar, etc.).
    if cursor_style != 0 {
        out.extend_from_slice(format!("\x1b[{cursor_style} q").as_bytes());
    }
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
    fn snapshot_for_main_screen_begins_with_alt_exit_then_safe_reset() {
        let bytes = render_screen_snapshot(terminal_with(b"hi").screen());
        assert!(bytes.starts_with(b"\x1b[?1049l\x1b[!p\x1b[m\x1b[2J\x1b[H"));
    }

    #[test]
    fn snapshot_for_alt_screen_begins_with_alt_enter_then_safe_reset() {
        // Regression: a long-running TUI (vim, less, htop) in alt-screen
        // mode would, on raw-log overflow, get a snapshot that
        // unconditionally emitted `?1049l`. That dragged the host out of
        // alt and painted the captured alt-buffer grid into main-buffer
        // scrollback, corrupting the user's history. The snapshot must
        // paint into the buffer the grid was captured from.
        let bytes = render_screen_snapshot(terminal_with(b"\x1b[?1049hin alt").screen());
        assert!(
            bytes.starts_with(b"\x1b[?1049h\x1b[!p\x1b[m\x1b[2J\x1b[H"),
            "alt-screen snapshot must begin with `?1049h`; got: {:?}",
            String::from_utf8_lossy(&bytes[..16.min(bytes.len())])
        );
    }

    fn snapshot_after(setup: &[u8]) -> Vec<u8> {
        render_screen_snapshot(terminal_with(setup).screen())
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn snapshot_re_asserts_mouse_sgr_reporting_when_inner_program_had_set_it() {
        // The mouse-broken-vim-after-reattach failure mode: vim sets
        // ?1006h (SGR mouse encoding) + ?1002h (button events) at
        // startup. The snapshot's soft reset wipes these; without
        // re-asserting them, an attach mid-session leaves vim unable
        // to receive mouse events until something else re-emits the
        // toggles. Pin per-protocol so a future regression fingers the
        // right family.
        let bytes = snapshot_after(b"\x1b[?1006h\x1b[?1002h vim ready");
        assert!(contains(&bytes, b"\x1b[?1006h"), "must re-emit SGR mouse encoding");
        assert!(contains(&bytes, b"\x1b[?1002h"), "must re-emit button-event tracking");
    }

    #[test]
    fn snapshot_picks_most_permissive_mouse_tracking_mode() {
        // When more than one tracking-level bit is on (rare but
        // possible if the inner program toggled them in sequence and
        // the parser kept all bits set), ?1003 (any-event) wins.
        // The encoding-level bits (?1006 SGR vs ?1005 UTF-8) are
        // independent of tracking-level and may coexist; SGR wins.
        let bytes = snapshot_after(b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h");
        assert!(
            contains(&bytes, b"\x1b[?1003h"),
            "?1003 must be picked when all three tracking modes are set"
        );
        assert!(
            !contains(&bytes, b"\x1b[?1002h") && !contains(&bytes, b"\x1b[?1000h"),
            "lower-precedence tracking modes must NOT be emitted: {:?}",
            String::from_utf8_lossy(&bytes)
        );
        assert!(contains(&bytes, b"\x1b[?1006h"));
    }

    #[test]
    fn snapshot_re_asserts_bracketed_paste() {
        // Shells (bash, zsh) and most TUIs enable bracketed paste on
        // startup so multi-line pastes don't auto-execute. Losing it
        // across a snapshot turns a pasted block into a runaway
        // command. Easy to miss; hence a dedicated pin.
        let bytes = snapshot_after(b"\x1b[?2004h prompt ");
        assert!(contains(&bytes, b"\x1b[?2004h"));
    }

    #[test]
    fn snapshot_re_asserts_modify_other_keys_level_two() {
        // modifyOtherKeys=2 is the modern wezterm/kitty default for
        // shells that opt in. Level 2 supersedes level 1; only the
        // level-2 toggle should appear in the snapshot.
        let bytes = snapshot_after(b"\x1b[>4;2m");
        assert!(contains(&bytes, b"\x1b[>4;2m"));
        assert!(
            !contains(&bytes, b"\x1b[>4;1m"),
            "level-2 supersedes level-1; only one toggle should be emitted"
        );
    }

    #[test]
    fn snapshot_re_asserts_cursor_hidden_when_inner_program_hid_it() {
        // Cursor visible is the post-DECSTR default, so the snapshot
        // emits NO ?25 toggle for a shell. When the inner program
        // hid the cursor (a TUI mid-animation), the snapshot must
        // emit `?25l` so the cursor stays hidden through replay.
        let visible = snapshot_after(b"shell prompt$ ");
        assert!(
            !contains(&visible, b"\x1b[?25l"),
            "cursor-visible is the default; snapshot must NOT emit ?25l"
        );
        let hidden = snapshot_after(b"\x1b[?25l hidden");
        assert!(contains(&hidden, b"\x1b[?25l"));
    }

    #[test]
    fn snapshot_for_plain_shell_emits_no_extra_mode_bytes() {
        // Sanity floor: a shell running with nothing fancy set should
        // produce a snapshot whose prefix matches the historical fixed
        // prefix exactly, plus the grid and cursor positioning. This
        // pin guards against accidentally emitting toggles for modes
        // that ARE on by default.
        let bytes = snapshot_after(b"$ ");
        let expected_prefix = b"\x1b[?1049l\x1b[!p\x1b[m\x1b[2J\x1b[H";
        assert!(
            bytes.starts_with(expected_prefix),
            "plain shell must produce no extra mode bytes between SGR reset \
             and clear; got prefix: {:?}",
            String::from_utf8_lossy(&bytes[..expected_prefix.len().min(bytes.len())])
        );
    }

    #[test]
    fn snapshot_re_asserts_application_cursor_keys() {
        // DECCKM (vim, less, fzf all set this so arrows emit `\x1bOA`).
        // Losing it across snapshot would silently re-encode arrow
        // keys as `\x1b[A` which the inner program doesn't recognise.
        let bytes = snapshot_after(b"\x1b[?1h app-cursor");
        assert!(contains(&bytes, b"\x1b[?1h"));
    }

    #[test]
    fn snapshot_re_asserts_focus_event_reporting() {
        let bytes = snapshot_after(b"\x1b[?1004h");
        assert!(contains(&bytes, b"\x1b[?1004h"));
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
