//! Removal of visible, absolute and below-cursor lines.

use super::Screen;
use crate::input::{ScreenWriter, COLOUR_DEFAULT};

impl Screen {
    /// Deletes one visible line and scrolls the remaining viewport content up.
    ///
    /// This clears any pending wrap state because deleting a visible row
    /// invalidates the previous cursor edge condition.
    pub fn delete_visible_line(&mut self, y: u32) -> bool {
        if y >= self.grid.sy() {
            return false;
        }

        let cursor_x = self.cursor_x;
        let cursor_y = self.cursor_y;
        let rupper = self.rupper;
        let rlower = self.rlower;

        self.cursor_x = 0;
        self.cursor_y = y;
        self.pending_wrap = false;
        self.rupper = 0;
        self.rlower = self.grid.sy().saturating_sub(1);
        self.delete_line(1, COLOUR_DEFAULT);

        self.cursor_y = if cursor_y > y {
            cursor_y.saturating_sub(1)
        } else {
            cursor_y
        }
        .min(self.grid.sy().saturating_sub(1));
        self.cursor_x = cursor_x.min(self.grid.sx().saturating_sub(1));
        self.pending_wrap = false;
        self.rupper = rupper;
        self.rlower = rlower;
        true
    }

    /// Deletes one absolute line from history or the visible viewport.
    pub fn delete_absolute_line(&mut self, absolute_y: usize) -> bool {
        let history_size = self.grid.hsize();
        let visible_y = absolute_y.saturating_sub(history_size);
        let removed = self.grid.remove_absolute_line(absolute_y);
        if !removed {
            return false;
        }

        if absolute_y >= history_size {
            let visible_y = visible_y as u32;
            if visible_y < self.cursor_y {
                self.cursor_y = self.cursor_y.saturating_sub(1);
            }
        }
        self.pending_wrap = false;
        true
    }

    /// Trims all lines below the cursor and pulls history into the viewport.
    pub fn trim_below_cursor(&mut self) -> bool {
        let cursor_absolute_y = self.cursor_absolute_y();
        if !self.grid.truncate_after_absolute_line(cursor_absolute_y) {
            return false;
        }

        let history_size = self.grid.hsize();
        self.cursor_y = cursor_absolute_y
            .saturating_sub(history_size)
            .min(self.grid.sy().saturating_sub(1) as usize) as u32;
        self.cursor_x = self.cursor_x.min(self.max_cursor_x());
        self.pending_wrap = false;
        true
    }
}
