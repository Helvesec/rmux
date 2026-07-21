//! Public live-terminal screen facade.
//!
//! This module exposes the server-facing screen wrapper while keeping the
//! parser implementation inside the crate-private `terminal` module.

use rmux_proto::TerminalSize;

use crate::screen::Screen;
use crate::terminal::TerminalParser;
use crate::terminal_passthrough::TerminalPassthrough;
use crate::utf8::Utf8Config;

/// Live terminal screen fed by rmux-core's private parser boundary.
///
/// `TerminalScreen` is the public core facade that server code uses to feed
/// raw PTY bytes and inspect structured screen cells. The parser itself stays
/// hidden behind the crate-private terminal module, so SDK/protocol code can
/// depend on screen-cell semantics without coupling to parser internals.
pub struct TerminalScreen {
    parser: TerminalParser,
}

impl TerminalScreen {
    /// Builds a fresh terminal screen with the given geometry and scrollback
    /// limit.
    #[must_use]
    pub fn new(size: TerminalSize, history_limit: usize) -> Self {
        Self {
            parser: TerminalParser::new(size, history_limit),
        }
    }

    /// Returns a borrow of the structured screen grid.
    #[must_use]
    pub fn screen(&self) -> &Screen {
        self.parser.screen()
    }

    /// Returns a mutable borrow of the structured screen grid.
    pub fn screen_mut(&mut self) -> &mut Screen {
        self.parser.screen_mut()
    }

    /// Returns whether plain printable output can bypass structured rendering
    /// without losing parser or screen semantics.
    #[must_use]
    pub fn plain_output_forwarding_safe(&self) -> bool {
        self.parser.plain_output_forwarding_safe()
    }

    /// Updates the tmux-style UTF-8 width and combining configuration.
    pub fn set_utf8_config(&mut self, config: Utf8Config) {
        self.parser.set_utf8_config(config);
    }

    /// Enables or disables DEC alternate-screen entry for subsequent output.
    pub fn set_alternate_screen_enabled(&mut self, enabled: bool) {
        self.parser.set_alternate_screen_enabled(enabled);
    }

    /// Enables or disables title changes requested by pane output.
    pub fn set_title_rename_enabled(&mut self, enabled: bool) {
        self.parser.set_title_rename_enabled(enabled);
    }

    /// Updates the tmux `input-buffer-size` parser limit.
    pub fn set_input_buffer_limit(&mut self, limit: usize) {
        self.parser.set_input_buffer_limit(limit);
    }

    /// Resizes the screen and resets the scroll region.
    pub fn resize(&mut self, size: TerminalSize) {
        self.parser.resize(size);
    }

    /// Feeds raw PTY output bytes through the private parser into the screen.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.feed(bytes);
    }

    /// Returns and drains terminal replies generated while parsing PTY output.
    pub fn take_replies(&mut self) -> Vec<u8> {
        self.parser.take_replies()
    }

    /// Returns any bytes still buffered inside an incomplete parser state.
    #[must_use]
    pub fn pending_bytes(&self) -> Vec<u8> {
        self.parser.pending_bytes()
    }

    /// Returns ANSI bytes that restore the parser's active rendition,
    /// character sets, and hyperlink after a renderer repaint.
    #[must_use]
    pub fn active_cell_state_ansi(&self) -> Vec<u8> {
        self.parser.active_cell_state_ansi()
    }

    /// Returns whether the parser ground timeout is currently armed.
    #[must_use]
    pub fn ground_timer_active(&self) -> bool {
        self.parser.ground_timer_active()
    }

    /// Notifies the parser that its ground timeout has expired.
    pub fn ground_timer_expired(&mut self) {
        self.parser.ground_timer_expired();
    }

    /// Returns and drains passthrough events generated while parsing PTY output.
    pub fn take_terminal_passthrough(&mut self) -> Vec<TerminalPassthrough> {
        self.parser.take_terminal_passthrough()
    }

    /// Returns and drains passthrough events dropped by parser safety limits.
    pub fn take_terminal_passthrough_dropped_count(&mut self) -> u64 {
        self.parser.take_terminal_passthrough_dropped_count()
    }

    /// Replaces the hidden parser with a fresh ground-state instance while
    /// preserving the current screen grid.
    pub fn reset_parser(&mut self) {
        self.parser.reset_parser();
    }
}
