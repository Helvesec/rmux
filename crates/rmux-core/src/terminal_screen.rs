//! Public live-terminal screen facade.
//!
//! This module exposes the server-facing screen wrapper while keeping the
//! parser implementation inside the crate-private `terminal` module.

use rmux_proto::TerminalSize;

use crate::input::OscColourSlot;
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

    /// Returns the last screen state eligible for presentation.
    ///
    /// While DEC synchronized output is active, parsing continues in
    /// [`Self::screen`] but presentation remains at the state captured when
    /// mode 2026 was entered.
    #[must_use]
    pub fn presentation_screen(&self) -> &Screen {
        self.parser.screen().presentation_screen()
    }

    /// Returns and clears whether the latest feed admitted synchronized output.
    pub fn take_synchronized_output_started(&mut self) -> bool {
        self.parser.screen_mut().take_synchronized_output_started()
    }

    /// Returns whether DEC synchronized output currently owns publication.
    #[must_use]
    pub fn synchronized_output_active(&self) -> bool {
        self.parser.screen().synchronized_output_active()
    }

    /// Releases synchronized output without discarding the converged screen.
    ///
    /// This is the fail-open path used when a child exits or leaves mode 2026
    /// unclosed.
    pub fn finish_synchronized_output(&mut self) -> bool {
        self.parser.screen_mut().finish_synchronized_output()
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

    /// Enables or disables window renames requested by pane output.
    pub fn set_allow_rename(&mut self, enabled: bool) {
        self.parser.set_allow_rename(enabled);
    }

    /// Drains the window-rename requests this pane's output accepted, in the
    /// order it emitted them.
    ///
    /// This is the pane owner's only view of `ESC k`: the stored screen
    /// metadata cannot stand in for it, because an empty request and a
    /// repeated identical one leave that metadata unchanged while still
    /// renaming the window. The owner drains after every feed, like the
    /// replies and the theme operations.
    pub fn take_window_rename_requests(&mut self) -> Vec<String> {
        self.parser.take_window_rename_requests()
    }

    /// Sets whether a whole-screen erase scrolls the visible rows into history.
    pub fn set_scroll_on_clear(&mut self, enabled: bool) {
        self.parser.set_scroll_on_clear(enabled);
    }

    /// Updates the tmux `input-buffer-size` parser limit.
    pub fn set_input_buffer_limit(&mut self, limit: usize) {
        self.parser.set_input_buffer_limit(limit);
    }

    /// Resizes the screen and resets the scroll region.
    pub fn resize(&mut self, size: TerminalSize) {
        self.parser.resize(size);
    }

    /// Feeds pane output with known foreground/background query overrides.
    /// These affect OSC 10/11 replies only, never application colours, cursor
    /// state or recovery serialisation. Unknown slots retain normal behavior.
    pub fn feed_with_query_colours(&mut self, bytes: &[u8], colours: [Option<[u8; 3]>; 2]) {
        self.parser.feed_with_query_colours(bytes, colours);
    }

    /// Feeds raw PTY output bytes through the private parser into the screen.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.feed(bytes);
    }

    /// Drain server-owned theme operations alongside the ordinary reply bytes.
    pub fn take_theme_events(&mut self) -> Vec<crate::input::ThemeEvent> {
        self.parser.take_theme_events()
    }

    /// Opt into server effects; the owner must drain them after every feed.
    pub fn enable_theme_events(&mut self) {
        self.parser.enable_theme_events();
    }

    /// Returns and clears whether the latest feeds used terminal-parser state
    /// that an ANSI recovery keyframe cannot reconstruct.
    ///
    /// Callers publishing raw continuation bytes must replace that
    /// continuation with an authoritative post-dispatch keyframe.
    #[must_use]
    pub fn take_recovery_rebase_required(&mut self) -> bool {
        self.parser.take_recovery_rebase_required()
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

    /// Borrows bytes buffered inside an incomplete parser state.
    #[must_use]
    pub fn pending_bytes_ref(&self) -> &[u8] {
        self.parser.pending_bytes_ref()
    }

    /// Clones renderer state, including bounded scrollback and saved buffers.
    #[must_use]
    pub fn clone_recovery_screen(&self) -> Screen {
        self.parser.clone_recovery_screen()
    }

    /// Returns ANSI restoring the parser's active rendition and character sets.
    #[must_use]
    pub fn active_cell_state_ansi(&self) -> Vec<u8> {
        self.parser.active_cell_state_ansi()
    }

    /// Returns bounded ANSI for the active rendition and whether metadata was
    /// represented completely.
    #[must_use]
    pub fn active_cell_state_ansi_bounded(&self, max_hyperlink_bytes: usize) -> (Vec<u8>, bool) {
        self.parser
            .active_cell_state_ansi_bounded(max_hyperlink_bytes)
    }

    /// Returns ANSI restoring the rendition saved by DECSC/SCP.
    #[must_use]
    pub fn saved_cell_state_ansi(&self) -> Vec<u8> {
        self.parser.saved_cell_state_ansi()
    }

    /// Returns bounded ANSI for the saved rendition and whether metadata was
    /// represented completely.
    #[must_use]
    pub fn saved_cell_state_ansi_bounded(&self, max_hyperlink_bytes: usize) -> (Vec<u8>, bool) {
        self.parser
            .saved_cell_state_ansi_bounded(max_hyperlink_bytes)
    }

    /// Returns the cursor and origin mode saved by DECSC/SCP.
    #[must_use]
    pub fn saved_cursor_state(&self) -> (u32, u32, bool) {
        self.parser.saved_cursor_state()
    }

    /// Returns ANSI restoring parser-owned state that has a faithful terminal
    /// representation, such as application-defined dynamic colours.
    #[must_use]
    pub fn recovery_parser_state_ansi(&self) -> Vec<u8> {
        self.parser.recovery_parser_state_ansi()
    }

    /// Returns an application-defined OSC 10/11/12 colour.
    #[must_use]
    pub fn dynamic_colour(&self, slot: OscColourSlot) -> Option<&str> {
        self.parser.dynamic_colour(slot)
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

#[cfg(test)]
#[path = "terminal_screen/tests.rs"]
mod tests;
