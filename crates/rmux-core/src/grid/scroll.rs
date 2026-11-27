//! Region movement and wrap-boundary maintenance.

use super::{Colour, Grid, GridLine};

impl Grid {
    pub(crate) fn scroll_region_up(
        &mut self,
        upper: u32,
        lower: u32,
        bg: Colour,
        to_history: bool,
    ) {
        if !self.valid_region(upper, lower) {
            return;
        }

        let moved_lines = lower.saturating_sub(upper);
        if !(to_history && self.history_enabled) {
            self.prepare_wrapped_line_move(upper, upper.saturating_add(1), moved_lines);
        }
        self.scroll_region_up_one(upper, lower, bg, to_history);
    }

    fn scroll_region_up_one(&mut self, upper: u32, lower: u32, bg: Colour, to_history: bool) {
        let upper = upper as usize;
        let lower = lower as usize;
        if upper == 0 && lower + 1 == self.visible.len() {
            let Some(mut removed) = self.visible.pop_front() else {
                return;
            };
            if to_history && self.history_enabled {
                self.push_history(removed);
                self.visible.push_back(GridLine::blank_with_bg(self.sx, bg));
            } else {
                removed.clear(bg);
                self.visible.push_back(removed);
            }
            return;
        }

        let removed_for_history = if to_history && self.history_enabled {
            let blank = GridLine::blank_with_bg(self.sx, bg);
            let visible = self.visible.make_contiguous();
            let removed = std::mem::replace(&mut visible[upper], blank);
            Some(removed)
        } else {
            None
        };
        if let Some(removed) = removed_for_history {
            self.push_history(removed);
        }
        let visible = self.visible.make_contiguous();
        visible[upper..=lower].rotate_left(1);
        let removed = &mut visible[lower];
        removed.clear(bg);
    }

    pub(crate) fn scroll_region_down(&mut self, upper: u32, lower: u32, bg: Colour) {
        if !self.valid_region(upper, lower) {
            return;
        }

        let moved_lines = lower.saturating_sub(upper);
        self.prepare_wrapped_line_move(upper.saturating_add(1), upper, moved_lines);
        self.scroll_region_down_one(upper, lower, bg);
    }

    fn scroll_region_down_one(&mut self, upper: u32, lower: u32, bg: Colour) {
        let upper = upper as usize;
        let lower = lower as usize;
        if upper == 0 && lower + 1 == self.visible.len() {
            let Some(mut removed) = self.visible.pop_back() else {
                return;
            };
            removed.clear(bg);
            self.visible.push_front(removed);
            return;
        }

        let visible = self.visible.make_contiguous();
        visible[upper..=lower].rotate_right(1);
        visible[upper].clear(bg);
    }

    pub(crate) fn insert_lines(&mut self, upper: u32, lower: u32, count: u32, bg: Colour) {
        if !self.valid_region(upper, lower) {
            return;
        }
        let region_lines = lower.saturating_sub(upper).saturating_add(1);
        let count = count.max(1).min(region_lines);
        let moved_lines = region_lines.saturating_sub(count);
        if moved_lines == 0 {
            self.break_wrap_before_visible_line(upper);
        } else {
            self.prepare_wrapped_line_move(upper.saturating_add(count), upper, moved_lines);
        }
        for _ in 0..count {
            self.scroll_region_down_one(upper, lower, bg);
        }
        // tmux follows the move with a full-line clear starting after the
        // moved range. That clear also breaks WRAPPED immediately before its
        // start, even when its unsigned row count describes an empty range.
        if count != moved_lines {
            self.break_wrap_before_visible_line(upper.saturating_add(moved_lines));
        }
    }

    /// Inserts `count` blank rows at `upper` and pushes the rows below it
    /// toward the bottom of the screen, where the overflow is discarded.
    ///
    /// This is tmux's `grid_view_insert_lines`, the path `IL` takes when the
    /// cursor is outside the scroll region. tmux 3.7b only moves the surviving
    /// rows, so when `count` exceeds them the rows they vacate are never
    /// cleared: stale text stays behind, and a `count` reaching the bottom
    /// changes nothing. RMUX deliberately clears every inserted row.
    pub(crate) fn insert_lines_to_bottom(&mut self, upper: u32, count: u32, bg: Colour) {
        if upper >= self.sy {
            return;
        }
        let rows = self.sy - upper;
        let count = count.max(1).min(rows);
        let moved_lines = rows - count;
        if moved_lines == 0 {
            self.break_wrap_before_visible_line(upper);
        } else {
            self.prepare_wrapped_line_move(upper + count, upper, moved_lines);
        }
        let visible = self.visible.make_contiguous();
        let edited = &mut visible[upper as usize..];
        edited.rotate_right(count as usize);
        for line in &mut edited[..count as usize] {
            line.clear(bg);
        }
    }

    /// Deletes `count` rows at `upper`, pulls the rows below it up and clears
    /// the rows freed at the bottom of the screen. Nothing enters history.
    ///
    /// This is tmux's `grid_view_delete_lines`, the path `DL` takes when the
    /// cursor is outside the scroll region, including its wrap maintenance:
    /// the move, then the full-row clear of the freed bottom rows.
    pub(crate) fn delete_lines_to_bottom(&mut self, upper: u32, count: u32, bg: Colour) {
        if upper >= self.sy {
            return;
        }
        let rows = self.sy - upper;
        let count = count.max(1).min(rows);
        self.prepare_wrapped_line_move(upper, upper + count, rows - count);
        let visible = self.visible.make_contiguous();
        let edited = &mut visible[upper as usize..];
        edited.rotate_left(count as usize);
        let freed_start = edited.len() - count as usize;
        for line in &mut edited[freed_start..] {
            line.clear(bg);
        }
        self.break_wrap_before_visible_line(self.sy - count);
    }

    /// Mirrors tmux's `grid_move_lines` wrap-boundary maintenance without
    /// coupling raw row storage to tmux's allocation strategy.
    fn prepare_wrapped_line_move(&mut self, destination: u32, source: u32, count: u32) {
        if count == 0 || source == destination {
            return;
        }
        self.break_wrap_before_visible_line(destination);
        let destination_end = destination.saturating_add(count);
        if source < destination || source >= destination_end {
            self.break_wrap_before_visible_line(source);
        }
    }

    fn valid_region(&self, upper: u32, lower: u32) -> bool {
        upper < self.sy && lower < self.sy && upper <= lower
    }
}
