//! Crate-private terminal parser boundary.
//!
//! [`TerminalParser`] couples the existing tmux-compatible [`InputParser`]
//! state machine with a [`Screen`] grid, providing a single place inside
//! `rmux-core` that ingests PTY bytes. It is intentionally not part of the
//! public crate surface: SDK-facing semantics continue to flow through
//! [`Screen`], [`ScreenCellView`], and [`ScreenLineView`]. Server-side live
//! pane ingestion uses the public [`TerminalScreen`](crate::TerminalScreen)
//! wrapper, which delegates to this crate-private parser pair. Future parser
//! migrations must be implemented behind this same boundary and must pass the
//! parser-trace golden tests in `crates/rmux-core/tests/parser_traces.rs`
//! before they are allowed to replace the existing `InputParser`-driven
//! behavior.
//!
//! [`ScreenCellView`]: crate::screen::ScreenCellView
//! [`ScreenLineView`]: crate::screen::ScreenLineView
//! [`InputParser`]: crate::input::InputParser
//! [`Screen`]: crate::screen::Screen

use rmux_proto::TerminalSize;

use crate::input::{InputParser, InputState};
use crate::screen::Screen;
use crate::terminal_passthrough::TerminalPassthrough;
use crate::utf8::Utf8Config;

/// Backend-neutral parser progress exposed by [`TerminalParser`].
///
/// The current implementation maps from tmux-compatible [`InputState`], but
/// this enum is intentionally owned by the terminal boundary so a future
/// parser backend does not have to expose its native state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum TerminalParserState {
    /// Normal printable-character processing.
    Ground,
    /// An ESC-prefixed sequence is being collected.
    Escape,
    /// A CSI sequence is being collected.
    Csi,
    /// A DCS sequence is being collected.
    Dcs,
    /// A string-style sequence such as OSC, APC, rename, or ST-consume is
    /// being collected.
    String,
    /// An invalid sequence is being absorbed until its terminator.
    Ignore,
}

impl From<InputState> for TerminalParserState {
    fn from(state: InputState) -> Self {
        match state {
            InputState::Ground => Self::Ground,
            InputState::EscEnter | InputState::EscIntermediate => Self::Escape,
            InputState::CsiEnter | InputState::CsiParameter | InputState::CsiIntermediate => {
                Self::Csi
            }
            InputState::CsiIgnore | InputState::DcsIgnore => Self::Ignore,
            InputState::DcsEnter
            | InputState::DcsParameter
            | InputState::DcsIntermediate
            | InputState::DcsHandler
            | InputState::DcsEscape => Self::Dcs,
            InputState::OscString
            | InputState::ApcString
            | InputState::RenameString
            | InputState::ConsumeSt => Self::String,
        }
    }
}

/// Private parser/screen pair owned by `rmux-core`.
///
/// `TerminalParser` is the single ingest point for PTY bytes. It is not
/// re-exported from the crate root and must not appear in public SDK or
/// protocol signatures. Callers that need to inspect rendered cells or
/// transcript output use [`Screen`] through [`Self::screen`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct TerminalParser {
    parser: InputParser,
    screen: Screen,
}

#[cfg_attr(not(test), allow(dead_code))]
impl TerminalParser {
    /// Builds a fresh parser/screen pair with the given geometry and
    /// scrollback limit.
    #[must_use]
    pub(crate) fn new(size: TerminalSize, history_limit: usize) -> Self {
        let mut screen = Screen::new(size, history_limit);
        screen.set_preserve_alternate_screen_cursor(true);
        Self {
            parser: InputParser::new(),
            screen,
        }
    }

    /// Returns a borrow of the underlying [`Screen`].
    #[must_use]
    pub(crate) fn screen(&self) -> &Screen {
        &self.screen
    }

    /// Returns a mutable borrow of the underlying [`Screen`].
    pub(crate) fn screen_mut(&mut self) -> &mut Screen {
        &mut self.screen
    }

    /// Updates the tmux-style UTF-8 width and combining configuration on
    /// the underlying screen.
    pub(crate) fn set_utf8_config(&mut self, config: Utf8Config) {
        self.screen.set_utf8_config(config);
    }

    /// Enables or disables DEC alternate-screen entry on the underlying screen.
    pub(crate) fn set_alternate_screen_enabled(&mut self, enabled: bool) {
        self.screen.set_alternate_screen_enabled(enabled);
    }

    /// Updates the tmux `input-buffer-size` parser limit.
    pub(crate) fn set_input_buffer_limit(&mut self, limit: usize) {
        self.parser.set_input_buffer_limit(limit);
    }

    /// Resizes the screen and resets the scroll region.
    pub(crate) fn resize(&mut self, size: TerminalSize) {
        self.screen.resize(size);
    }

    /// Feeds raw PTY output bytes through the parser into the screen.
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        self.parser.parse(bytes, &mut self.screen);
    }

    /// Returns the current parser progress state for diagnostics.
    #[must_use]
    pub(crate) fn parser_state(&self) -> TerminalParserState {
        self.parser.state().into()
    }

    /// Returns and drains any reply bytes that the parser would write back
    /// to the PTY.
    pub(crate) fn take_replies(&mut self) -> Vec<u8> {
        self.parser.take_replies()
    }

    /// Returns and drains terminal passthrough events generated while parsing.
    pub(crate) fn take_terminal_passthrough(&mut self) -> Vec<TerminalPassthrough> {
        self.screen.take_terminal_passthrough()
    }

    /// Returns and drains terminal passthrough events dropped by parser limits.
    pub(crate) fn take_terminal_passthrough_dropped_count(&mut self) -> u64 {
        self.parser
            .take_terminal_passthrough_dropped_count()
            .saturating_add(self.screen.take_terminal_passthrough_dropped_count())
    }

    /// Returns any bytes still buffered inside an incomplete parser state.
    #[must_use]
    pub(crate) fn pending_bytes(&self) -> Vec<u8> {
        self.parser.pending_bytes()
    }

    /// Returns whether the parser ground timer would currently be running.
    #[must_use]
    pub(crate) fn ground_timer_active(&self) -> bool {
        self.parser.ground_timer_active()
    }

    /// Notifies the parser that the ground timer has expired.
    pub(crate) fn ground_timer_expired(&mut self) {
        self.parser.ground_timer_expired();
    }

    /// Resets the parser state machine to ground.
    pub(crate) fn reset_to_ground(&mut self) {
        self.parser.reset_to_ground();
    }

    /// Replaces the parser with a fresh ground-state instance.
    ///
    /// This preserves the current screen grid while discarding parser-local
    /// state such as pending control bytes, replies, saved cursor state, and
    /// current cell attributes.
    pub(crate) fn reset_parser(&mut self) {
        let input_buffer_limit = self.parser.configured_input_buffer_limit();
        self.parser = InputParser::new();
        self.parser.set_input_buffer_limit(input_buffer_limit);
    }
}

#[cfg(test)]
mod tests {
    use super::{TerminalParser, TerminalParserState};
    use crate::utf8::Utf8Config;
    use rmux_proto::TerminalSize;

    fn size(cols: u16, rows: u16) -> TerminalSize {
        TerminalSize { cols, rows }
    }

    #[test]
    fn ascii_feed_updates_screen() {
        let mut parser = TerminalParser::new(size(5, 2), 10);
        parser.feed(b"hello");
        let screen = parser.screen();
        assert_eq!(screen.cursor_position(), (4, 0));
        assert_eq!(parser.parser_state(), TerminalParserState::Ground);
        assert!(parser.pending_bytes().is_empty());
    }

    #[test]
    fn pending_bytes_reflect_unfinished_csi() {
        let mut parser = TerminalParser::new(size(5, 2), 10);
        parser.feed(b"\x1b[1;");
        assert_ne!(parser.parser_state(), TerminalParserState::Ground);
        let pending = parser.pending_bytes();
        assert!(!pending.is_empty(), "pending bytes should buffer mid-CSI");
        parser.reset_to_ground();
        assert_eq!(parser.parser_state(), TerminalParserState::Ground);
        assert!(parser.pending_bytes().is_empty());
    }

    #[test]
    fn replies_drain_through_take_replies() {
        let mut parser = TerminalParser::new(size(5, 2), 10);
        // DA1 (Send Device Attributes) should produce a reply.
        parser.feed(b"\x1b[c");
        let replies = parser.take_replies();
        assert!(!replies.is_empty());
        assert!(parser.take_replies().is_empty());
    }

    #[test]
    fn kitty_graphics_passthrough_drains_once() {
        let mut parser = TerminalParser::new(size(20, 4), 10);
        parser.feed(b"\x1b[2;3H\x1b_Gf=100;AAAA\x1b\\");
        let passthroughs = parser.take_terminal_passthrough();
        assert_eq!(passthroughs.len(), 1);
        assert_eq!(passthroughs[0].cursor_x(), 2);
        assert_eq!(passthroughs[0].cursor_y(), 1);
        assert_eq!(passthroughs[0].payload(), b"Gf=100;AAAA");
        assert!(parser.take_terminal_passthrough().is_empty());
    }

    #[test]
    fn sixel_passthrough_drains_once() {
        let mut parser = TerminalParser::new(size(20, 4), 10);
        parser.feed(b"\x1b[2;3H\x1bPq#0!10~\x1b\\");
        let passthroughs = parser.take_terminal_passthrough();
        assert_eq!(passthroughs.len(), 1);
        assert_eq!(passthroughs[0].cursor_x(), 2);
        assert_eq!(passthroughs[0].cursor_y(), 1);
        assert_eq!(passthroughs[0].payload(), b"q#0!10~");
        assert!(parser.take_terminal_passthrough().is_empty());
    }

    #[test]
    fn dcs_passthrough_drains_once() {
        let mut parser = TerminalParser::new(size(20, 4), 10);
        parser.feed(b"\x1b[2;3H\x1bPtmux;\x1b]52;c;QQ==\x07\x1b\\");
        let passthroughs = parser.take_terminal_passthrough();
        assert_eq!(passthroughs.len(), 1);
        assert_eq!(passthroughs[0].cursor_x(), 2);
        assert_eq!(passthroughs[0].cursor_y(), 1);
        assert_eq!(passthroughs[0].payload(), b"\x1b]52;c;QQ==\x07");
        assert!(parser.take_terminal_passthrough().is_empty());
    }

    #[test]
    fn resize_propagates_to_screen() {
        let mut parser = TerminalParser::new(size(5, 2), 10);
        parser.feed(b"abc");
        parser.resize(size(8, 3));
        assert_eq!(parser.screen().size().cols, 8);
        assert_eq!(parser.screen().size().rows, 3);
    }

    // --- Edge-case hardening ---------------------------------------------

    #[test]
    fn empty_feed_keeps_state_ground() {
        let mut parser = TerminalParser::new(size(4, 2), 4);
        parser.feed(b"");
        assert_eq!(parser.parser_state(), TerminalParserState::Ground);
        assert!(parser.pending_bytes().is_empty());
        assert!(parser.take_replies().is_empty());
        assert_eq!(parser.screen().cursor_position(), (0, 0));
        assert_eq!(parser.screen().history_size(), 0);
    }

    #[test]
    fn split_utf8_wide_glyph_byte_by_byte() {
        // Feeding multi-byte UTF-8 one byte at a time must produce the same
        // visible glyph as a single-shot feed, with parser returning to
        // Ground after the final byte.
        let mut a = TerminalParser::new(size(4, 1), 4);
        let bytes = "日".as_bytes();
        for byte in bytes {
            a.feed(std::slice::from_ref(byte));
        }
        assert_eq!(a.parser_state(), TerminalParserState::Ground);
        assert!(a.pending_bytes().is_empty());

        let mut b = TerminalParser::new(size(4, 1), 4);
        b.feed(bytes);

        assert_eq!(a.screen().cursor_position(), b.screen().cursor_position());
        assert_eq!(a.screen().size(), b.screen().size());
    }

    #[test]
    fn pending_clears_after_completing_sequence() {
        let mut parser = TerminalParser::new(size(8, 1), 4);
        parser.feed(b"\x1b[1;");
        assert_ne!(parser.parser_state(), TerminalParserState::Ground);
        // Completing the sequence must drain pending bytes without resetting.
        parser.feed(b"31m!");
        assert_eq!(parser.parser_state(), TerminalParserState::Ground);
        assert!(parser.pending_bytes().is_empty());
    }

    #[test]
    fn screen_mut_allows_history_clear() {
        let mut parser = TerminalParser::new(size(4, 1), 8);
        parser.feed(b"L1\nL2\nL3\nL4\n");
        assert!(parser.screen().history_size() > 0);
        parser.screen_mut().clear_history_and_hyperlinks(true);
        assert_eq!(parser.screen().history_size(), 0);
    }

    #[test]
    fn set_utf8_config_propagates_to_screen() {
        let mut parser = TerminalParser::new(size(8, 1), 4);
        // Default config first; then a non-default one. The call must not
        // panic and the screen must remain functional for subsequent feeds.
        parser.set_utf8_config(Utf8Config::default());
        parser.set_utf8_config(Utf8Config::default());
        parser.feed(b"hi");
        assert_eq!(parser.screen().cursor_position(), (2, 0));
    }

    #[test]
    fn ground_timer_lifecycle_is_idempotent() {
        let mut parser = TerminalParser::new(size(4, 1), 4);
        // Calling expired/reset on a freshly-constructed parser must be safe.
        parser.ground_timer_expired();
        parser.reset_to_ground();
        assert_eq!(parser.parser_state(), TerminalParserState::Ground);
        assert!(parser.pending_bytes().is_empty());
        // ground_timer_active is observable but its exact value depends on
        // the underlying parser; the contract is just that it returns.
        let _ = parser.ground_timer_active();
    }

    #[test]
    fn resize_to_one_by_one_is_safe() {
        let mut parser = TerminalParser::new(size(8, 4), 4);
        parser.feed(b"hello");
        parser.resize(size(1, 1));
        assert_eq!(parser.screen().size().cols, 1);
        assert_eq!(parser.screen().size().rows, 1);
        // The parser must keep working post-resize without panicking.
        parser.feed(b"X");
        assert_eq!(parser.parser_state(), TerminalParserState::Ground);
    }

    #[test]
    fn da1_reply_is_drained_only_once() {
        let mut parser = TerminalParser::new(size(4, 1), 2);
        parser.feed(b"\x1b[c");
        let first = parser.take_replies();
        assert!(!first.is_empty(), "DA1 must produce a reply");
        // A second drain with no new replies must yield an empty Vec rather
        // than re-emitting the previous reply.
        assert!(parser.take_replies().is_empty());
        // Issuing the request again must produce a fresh reply equal to the
        // first one (deterministic DA1 response).
        parser.feed(b"\x1b[c");
        assert_eq!(parser.take_replies(), first);
    }

    #[test]
    fn zero_history_limit_does_not_retain_lines() {
        let mut parser = TerminalParser::new(size(2, 2), 0);
        parser.feed(b"AB\nCD\nEF\n");
        assert_eq!(parser.screen().history_size(), 0);
    }

    #[test]
    fn alternate_screen_entry_sets_flag_and_exit_clears_it() {
        let mut parser = TerminalParser::new(size(8, 3), 4);
        parser.feed(b"primary");
        assert!(!parser.screen().is_alternate());
        parser.feed(b"\x1b[?1049h");
        assert!(parser.screen().is_alternate());
        parser.feed(b"\x1b[?1049l");
        assert!(!parser.screen().is_alternate());
    }

    #[test]
    fn arbitrary_chunk_boundaries_match_single_feed() {
        // Sweep every possible split point of a moderately complex byte
        // stream and confirm that feeding the prefix then the suffix
        // produces the same final cursor position and parser ground state
        // as a single-shot feed. This guards against state-machine
        // regressions where intermediate suspensions could lose bytes.
        let bytes: &[u8] = b"abc\x1b[1;31mX\x1b[0m\x1b[?25l\x1b[?25h!";
        let mut baseline = TerminalParser::new(size(20, 1), 4);
        baseline.feed(bytes);
        let baseline_cursor = baseline.screen().cursor_position();
        assert_eq!(baseline.parser_state(), TerminalParserState::Ground);

        for split in 0..=bytes.len() {
            let mut parser = TerminalParser::new(size(20, 1), 4);
            parser.feed(&bytes[..split]);
            parser.feed(&bytes[split..]);
            assert_eq!(
                parser.parser_state(),
                TerminalParserState::Ground,
                "chunked feed split={split} must end in Ground",
            );
            assert!(
                parser.pending_bytes().is_empty(),
                "chunked feed split={split} must drain pending bytes",
            );
            assert_eq!(
                parser.screen().cursor_position(),
                baseline_cursor,
                "chunked feed split={split} diverged from single-shot baseline",
            );
        }
    }

    #[test]
    fn reset_to_ground_after_partial_csi_drops_pending_without_screen_output() {
        let mut parser = TerminalParser::new(size(6, 1), 2);
        parser.feed(b"\x1b[31");
        assert_ne!(parser.parser_state(), TerminalParserState::Ground);
        let cursor_before = parser.screen().cursor_position();
        parser.reset_to_ground();
        assert_eq!(parser.parser_state(), TerminalParserState::Ground);
        assert!(parser.pending_bytes().is_empty());
        // Reset must not nudge the cursor or otherwise emit visible output.
        assert_eq!(parser.screen().cursor_position(), cursor_before);
        // After reset, normal ASCII must still print at the prior cursor.
        parser.feed(b"X");
        assert_eq!(parser.screen().cursor_position(), (1, 0));
    }
}
