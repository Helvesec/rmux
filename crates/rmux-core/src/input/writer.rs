//! Screen-write abstraction used by the VT input parser.

use super::cell::CellState;
use super::InputEndType;

/// Screen-write abstraction trait.
///
/// The parser calls these methods to effect terminal changes. `rmux-server`
/// implements this trait against its pane/grid runtime.
#[allow(unused_variables)]
pub trait ScreenWriter {
    // ─── Character output ──────────────────────────────────────
    /// Add a printable character with current cell attributes.
    fn collect_add(&mut self, ch: char, cell: &CellState);

    /// Add a printable character, applying ACS charset if `acs` is true.
    fn collect_add_with_charset(&mut self, ch: char, cell: &CellState, acs: bool) {
        self.collect_add(ch, cell);
    }

    /// End character collection (flush). Called before non-print transitions.
    fn collect_end(&mut self) {}

    // ─── Cursor movement ───────────────────────────────────────
    /// Move cursor up by `n` rows.
    fn cursor_up(&mut self, n: u32) {}
    /// Move cursor down by `n` rows.
    fn cursor_down(&mut self, n: u32) {}
    /// Move cursor left by `n` columns.
    fn cursor_left(&mut self, n: u32) {}
    /// Move cursor right by `n` columns.
    fn cursor_right(&mut self, n: u32) {}
    /// Move cursor to absolute position. -1 means "don't change".
    fn cursor_move(&mut self, col: i32, row: i32, origin_mode: bool) {}

    // ─── Line operations ───────────────────────────────────────
    /// Insert `n` blank lines at cursor, scrolling down.
    fn insert_line(&mut self, n: u32, bg: i32) {}
    /// Delete `n` lines at cursor, scrolling up.
    fn delete_line(&mut self, n: u32, bg: i32) {}
    /// Scroll up by `n` lines.
    fn scroll_up(&mut self, n: u32, bg: i32) {}
    /// Scroll down by `n` lines.
    fn scroll_down(&mut self, n: u32, bg: i32) {}
    /// Line feed.
    fn linefeed(&mut self, wrapped: bool, bg: i32) {}
    /// Reverse index (scroll down or move up).
    fn reverse_index(&mut self, bg: i32) {}
    /// Carriage return.
    fn carriage_return(&mut self) {}
    /// Backspace.
    fn backspace(&mut self) {}

    // ─── Character operations ──────────────────────────────────
    /// Insert `n` blank characters at cursor.
    fn insert_character(&mut self, n: u32, bg: i32) {}
    /// Delete `n` characters at cursor.
    fn delete_character(&mut self, n: u32, bg: i32) {}
    /// Erase (clear) `n` characters at cursor.
    fn clear_character(&mut self, n: u32, bg: i32) {}

    // ─── Erase operations ──────────────────────────────────────
    /// Clear to end of screen.
    fn clear_end_of_screen(&mut self, bg: i32) {}
    /// Clear from start of screen.
    fn clear_start_of_screen(&mut self, bg: i32) {}
    /// Clear entire screen.
    fn clear_screen(&mut self, bg: i32) {}
    /// Clear scrollback history.
    fn clear_history(&mut self) {}
    /// Clear to end of line.
    fn clear_end_of_line(&mut self, bg: i32) {}
    /// Clear from start of line.
    fn clear_start_of_line(&mut self, bg: i32) {}
    /// Clear entire line.
    fn clear_line(&mut self, bg: i32) {}

    // ─── Mode operations ───────────────────────────────────────
    /// Set mode bits.
    fn mode_set(&mut self, mode: u32) {}
    /// Clear mode bits.
    fn mode_clear(&mut self, mode: u32) {}

    // ─── Scroll region ─────────────────────────────────────────
    /// Set scroll region (top and bottom margins, 0-based).
    fn set_scroll_region(&mut self, top: u32, bottom: u32) {}

    // ─── Alternate screen ──────────────────────────────────────
    /// Switch to alternate screen. `save_cursor` = true for 1049.
    fn alternate_on(&mut self, bg: i32, save_cursor: bool) {}
    /// Switch back from alternate screen.
    fn alternate_off(&mut self, bg: i32, restore_cursor: bool) {}

    // ─── Attributes ────────────────────────────────────────────
    /// Reset cell attributes to default.
    fn reset_attributes(&mut self) {}

    // ─── Tab handling ──────────────────────────────────────────
    /// Handle a horizontal tab.
    fn tab(&mut self) {}
    /// Move cursor backward `n` tab stops (CBT).
    fn cursor_backward_tab(&mut self, n: u32) {}
    /// Set a tab stop at the current cursor column.
    fn set_tab_stop(&mut self) {}
    /// Clear a tab stop at the current cursor column.
    fn clear_tab_stop(&mut self) {}
    /// Clear all tab stops.
    fn clear_all_tab_stops(&mut self) {}

    // ─── Title/path ────────────────────────────────────────────
    /// Set the pane/window title.
    fn set_title(&mut self, title: &str) {}
    /// Set the window name (from rename string).
    fn set_window_name(&mut self, name: &str) {}
    /// Set the current directory path (OSC 7).
    fn set_path(&mut self, path: &str) {}

    // ─── Cursor save/restore ───────────────────────────────────
    /// Save cursor state (DECSC).
    fn save_cursor(&mut self) {}
    /// Restore cursor state (DECRC).
    fn restore_cursor(&mut self) {}

    // ─── Alignment test ────────────────────────────────────────
    /// Fill screen with 'E' characters (DECALN).
    fn alignment_test(&mut self) {}

    // ─── Full reset ────────────────────────────────────────────
    /// Full terminal reset (RIS).
    fn full_reset(&mut self) {}

    // ─── Synchronized output ───────────────────────────────────
    /// Start synchronized output.
    fn start_sync(&mut self) {}
    /// Stop synchronized output and redraw.
    fn stop_sync(&mut self) {}

    // ─── Cursor style ──────────────────────────────────────────
    /// Set cursor style (DECSCUSR). `n` is the Ps value 0-6.
    fn set_cursor_style(&mut self, n: u32) {}

    // ─── OSC handlers ──────────────────────────────────────────
    /// Handle OSC 4 palette colour.
    fn osc_palette(&mut self, data: &str, end: InputEndType) {}
    /// Handle OSC 8 hyperlink.
    fn osc_hyperlink(&mut self, data: &str) {}
    /// Returns the active OSC 8 hyperlink inner ID.
    fn current_hyperlink_id(&self) -> u32 {
        0
    }
    /// Handle OSC 9 notification.
    fn osc_notification(&mut self, data: &str) {}
    /// Handle OSC 10 fg colour query/set.
    fn osc_fg_colour(&mut self, data: &str, end: InputEndType) {}
    /// Handle OSC 11 bg colour query/set.
    fn osc_bg_colour(&mut self, data: &str, end: InputEndType) {}
    /// Handle OSC 12 cursor colour query/set.
    fn osc_cursor_colour(&mut self, data: &str, end: InputEndType) {}
    /// Handle OSC 52 clipboard.
    fn osc_clipboard(&mut self, data: &str, end: InputEndType) {}
    /// Handle OSC 104 reset palette.
    fn osc_reset_palette(&mut self, data: &str) {}
    /// Handle OSC 110 reset fg.
    fn osc_reset_fg(&mut self) {}
    /// Handle OSC 111 reset bg.
    fn osc_reset_bg(&mut self) {}
    /// Handle OSC 112 reset cursor colour.
    fn osc_reset_cursor(&mut self) {}
    /// Handle OSC 133 shell integration.
    fn osc_shell_integration(&mut self, data: &str) {}

    // ─── DCS handlers ──────────────────────────────────────────
    /// Handle DCS passthrough string.
    fn dcs_passthrough(&mut self, data: &[u8]) {}

    /// Handle an opaque SIXEL DCS passthrough string.
    fn sixel_passthrough(&mut self, data: &[u8]) {}

    // ─── APC handlers ──────────────────────────────────────────
    /// Handle an opaque APC passthrough string.
    fn apc_passthrough(&mut self, data: &[u8]) {}

    // ─── Paste/focus/notify ────────────────────────────────────
    /// Ring the bell (BEL).
    fn bell(&mut self) {}

    // ─── Query responses ───────────────────────────────────────
    /// Get screen width in columns.
    fn screen_size_x(&self) -> u32 {
        80
    }
    /// Get screen height in rows.
    fn screen_size_y(&self) -> u32 {
        24
    }
    /// Get current cursor column (0-based).
    fn cursor_x(&self) -> u32 {
        0
    }
    /// Get current cursor row (0-based).
    fn cursor_y(&self) -> u32 {
        0
    }
    /// Get current screen mode flags.
    fn current_mode(&self) -> u32 {
        0
    }

    // ─── Title stack ───────────────────────────────────────────
    /// Push current title onto the stack.
    fn push_title(&mut self) {}
    /// Pop title from the stack.
    fn pop_title(&mut self) {}

    // ─── Pane notification ─────────────────────────────────────
    /// Signal that the pane title changed.
    fn notify_pane_title_changed(&mut self) {}
}
