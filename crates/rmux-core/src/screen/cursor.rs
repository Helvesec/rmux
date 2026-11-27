//! Cursor clamping, restoration and the deferred autowrap.

use super::Screen;
use crate::grid::{GridLine, GridPhysicalCursor};
use crate::input::{mode, ScreenWriter, COLOUR_DEFAULT};

impl Screen {
    pub(super) fn max_cursor_x(&self) -> u32 {
        self.grid.sx().saturating_sub(1)
    }

    pub(super) fn cursor_column(&self) -> u32 {
        self.cursor_x.min(self.max_cursor_x())
    }

    /// Column that erase and character edits start from. tmux keeps its
    /// logical cursor at `sx` while an autowrap is pending, so those
    /// operations see no cell under the cursor instead of the last column.
    pub(super) fn logical_cursor_column(&self) -> u32 {
        if self.pending_wrap {
            self.grid.sx()
        } else {
            self.cursor_column()
        }
    }

    /// Whether the cursor row lies above or below the scroll region. `IL` and
    /// `DL` then edit from the cursor row to the bottom of the screen.
    pub(super) fn cursor_outside_scroll_region(&self) -> bool {
        self.cursor_y < self.rupper || self.cursor_y > self.rlower
    }

    pub(super) fn current_line_mut(&mut self) -> Option<&mut GridLine> {
        self.grid.visible_line_mut(self.cursor_y)
    }

    pub(super) fn clear_pending_wrap(&mut self) {
        self.pending_wrap = false;
    }

    pub(super) fn restore_cursor_position(&mut self, x: u32, y: u32, pending_wrap: bool) {
        self.cursor_x = x.min(self.max_cursor_x());
        self.cursor_y = y.min(self.grid.sy().saturating_sub(1));
        self.pending_wrap = pending_wrap
            && (self.mode & mode::MODE_WRAP) != 0
            && self.cursor_x == self.max_cursor_x();
    }

    pub(super) fn apply_grid_cursor(&mut self, cursor: GridPhysicalCursor) {
        let history_size = self.grid.hsize();
        self.cursor_x = cursor.x.min(self.max_cursor_x());
        self.cursor_y = cursor
            .absolute_y
            .saturating_sub(history_size)
            .min(self.grid.sy().saturating_sub(1) as usize) as u32;
        self.pending_wrap = cursor.pending_wrap
            && (self.mode & mode::MODE_WRAP) != 0
            && self.cursor_x == self.max_cursor_x();
    }

    pub(super) fn apply_pending_wrap(&mut self) {
        if !self.pending_wrap || (self.mode & mode::MODE_WRAP) == 0 {
            self.pending_wrap = false;
            return;
        }

        if let Some(line) = self.current_line_mut() {
            line.set_wrapped(true);
        }
        self.pending_wrap = false;
        self.linefeed(false, COLOUR_DEFAULT);
        self.cursor_x = 0;
    }
}
