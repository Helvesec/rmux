use rmux_proto::RmuxError;

use super::text::{owner_positions, scrollbar_slider_height, WordBoundary};
use super::types::CopyPosition;
use super::CopyModeState;

impl CopyModeState {
    pub(super) fn cmd_cursor_down(&mut self) -> Result<(), RmuxError> {
        if self.cursor.y + 1 < self.total_lines() {
            self.cursor.y += 1;
            self.cursor.x = self.owning_or_zero(self.cursor.y, self.cursor.x);
            self.ensure_cursor_visible();
        }
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_cursor_up(&mut self) -> Result<(), RmuxError> {
        if self.cursor.y > 0 {
            self.cursor.y -= 1;
            self.cursor.x = self.owning_or_zero(self.cursor.y, self.cursor.x);
            self.ensure_cursor_visible();
        }
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_cursor_left(&mut self) -> Result<(), RmuxError> {
        if let Some(previous) = self.previous_cell_position(self.cursor) {
            self.cursor = previous;
            self.ensure_cursor_visible();
        }
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_cursor_right(&mut self) -> Result<(), RmuxError> {
        if let Some(next) = self.next_cell_position(self.cursor) {
            self.cursor = next;
            self.ensure_cursor_visible();
        }
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_cursor_centre_vertical(&mut self) -> Result<(), RmuxError> {
        let rows = self.rows();
        self.top_line = self
            .cursor
            .y
            .saturating_sub(usize::from(rows / 2))
            .min(self.bottom_top_line());
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_cursor_centre_horizontal(&mut self) -> Result<(), RmuxError> {
        let middle = self.cols() / 2;
        self.cursor.x = self.owning_or_zero(self.cursor.y, middle);
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_start_of_line(&mut self) -> Result<(), RmuxError> {
        self.cursor.x = 0;
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_end_of_line(&mut self) -> Result<(), RmuxError> {
        self.cursor.y = self.logical_line_end_y(self.cursor.y);
        self.cursor.x = self.line_end_x(self.cursor.y);
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_top_line(&mut self) -> Result<(), RmuxError> {
        self.cursor.y = self.top_line;
        self.cursor.x = 0;
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_bottom_line(&mut self) -> Result<(), RmuxError> {
        let rows = self.rows().saturating_sub(1) as usize;
        self.cursor.y = (self.top_line + rows).min(self.total_lines().saturating_sub(1));
        self.cursor.x = 0;
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_middle_line(&mut self) -> Result<(), RmuxError> {
        self.cursor.y = (self.top_line + usize::from(self.rows().saturating_sub(1) / 2))
            .min(self.total_lines().saturating_sub(1));
        self.cursor.x = 0;
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_page_up(&mut self) -> Result<(), RmuxError> {
        let step = usize::from(self.rows().max(1));
        self.top_line = self.top_line.saturating_sub(step);
        self.cursor.y = self.cursor.y.saturating_sub(step);
        self.ensure_cursor_visible();
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_page_down(&mut self) -> Result<(), RmuxError> {
        let step = usize::from(self.rows().max(1));
        self.top_line = (self.top_line + step).min(self.bottom_top_line());
        self.cursor.y = (self.cursor.y + step).min(self.total_lines().saturating_sub(1));
        self.ensure_cursor_visible();
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_halfpage_up(&mut self) -> Result<(), RmuxError> {
        let step = usize::from(self.rows().max(1) / 2).max(1);
        self.top_line = self.top_line.saturating_sub(step);
        self.cursor.y = self.cursor.y.saturating_sub(step);
        self.ensure_cursor_visible();
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_halfpage_down(&mut self) -> Result<(), RmuxError> {
        let step = usize::from(self.rows().max(1) / 2).max(1);
        self.top_line = (self.top_line + step).min(self.bottom_top_line());
        self.cursor.y = (self.cursor.y + step).min(self.total_lines().saturating_sub(1));
        self.ensure_cursor_visible();
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_history_top(&mut self) -> Result<(), RmuxError> {
        self.top_line = 0;
        self.cursor = CopyPosition { x: 0, y: 0 };
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_history_bottom(&mut self) -> Result<(), RmuxError> {
        self.top_line = self.bottom_top_line();
        self.cursor = CopyPosition {
            x: self.backing.cursor_position().0,
            y: self.backing.cursor_absolute_y(),
        };
        self.ensure_cursor_visible();
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_scroll_top(&mut self) -> Result<(), RmuxError> {
        self.top_line = self.cursor.y.min(self.bottom_top_line());
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_scroll_bottom(&mut self) -> Result<(), RmuxError> {
        let rows = usize::from(self.rows().saturating_sub(1));
        self.top_line = self
            .cursor
            .y
            .saturating_sub(rows)
            .min(self.bottom_top_line());
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_scroll_middle(&mut self) -> Result<(), RmuxError> {
        let rows = usize::from(self.rows() / 2);
        self.top_line = self
            .cursor
            .y
            .saturating_sub(rows)
            .min(self.bottom_top_line());
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_scroll_up(&mut self) -> Result<(), RmuxError> {
        self.top_line = self.top_line.saturating_sub(1);
        if self.cursor.y > self.top_line + usize::from(self.rows()) - 1 {
            self.cursor.y = self.cursor.y.saturating_sub(1);
        }
        self.ensure_cursor_visible();
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_scroll_down(&mut self) -> Result<(), RmuxError> {
        self.top_line = (self.top_line + 1).min(self.bottom_top_line());
        if self.cursor.y < self.top_line {
            self.cursor.y = (self.cursor.y + 1).min(self.total_lines().saturating_sub(1));
        }
        self.ensure_cursor_visible();
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_back_to_indentation(&mut self) -> Result<(), RmuxError> {
        let line = self.line(self.cursor.y);
        let x = owner_positions(&line)
            .into_iter()
            .find(|x| {
                line.cell(*x)
                    .and_then(|cell| cell.text().chars().next())
                    .is_some_and(|ch| !ch.is_whitespace())
            })
            .unwrap_or(0);
        self.cursor.x = x;
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_goto_line(&mut self, line: &str) -> Result<(), RmuxError> {
        let Ok(number) = line.parse::<isize>() else {
            return Ok(());
        };
        if number < 0 {
            return Ok(());
        }
        let max_scroll = self.bottom_top_line();
        let scroll = usize::try_from(number)
            .unwrap_or(max_scroll)
            .min(max_scroll);
        self.top_line = self.bottom_top_line().saturating_sub(scroll);
        let visible_bottom = self.top_line + usize::from(self.rows().max(1));
        self.cursor.y = self
            .cursor
            .y
            .clamp(self.top_line, visible_bottom.saturating_sub(1))
            .min(self.total_lines().saturating_sub(1));
        self.cursor.x = self.owning_or_zero(self.cursor.y, self.cursor.x);
        self.ensure_cursor_visible();
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_next_prompt(&mut self, only_after: bool) -> Result<(), RmuxError> {
        let start = if only_after {
            self.cursor.y.saturating_add(1)
        } else {
            self.cursor.y
        };
        for y in start..self.total_lines() {
            if self
                .backing
                .absolute_line_view(y)
                .is_some_and(|line| line.start_prompt())
            {
                self.cursor.y = y;
                self.cursor.x = 0;
                self.ensure_cursor_visible();
                self.sync_selection_with_cursor();
                break;
            }
        }
        Ok(())
    }

    pub(super) fn cmd_previous_prompt(&mut self, only_before: bool) -> Result<(), RmuxError> {
        let start = if only_before {
            self.cursor.y.saturating_sub(1)
        } else {
            self.cursor.y
        };
        for y in (0..=start).rev() {
            if self
                .backing
                .absolute_line_view(y)
                .is_some_and(|line| line.start_prompt())
            {
                self.cursor.y = y;
                self.cursor.x = 0;
                self.ensure_cursor_visible();
                self.sync_selection_with_cursor();
                break;
            }
        }
        Ok(())
    }

    pub(super) fn cmd_next_matching_bracket(&mut self) -> Result<(), RmuxError> {
        if let Some(position) = self.find_matching_bracket(true) {
            self.cursor = position;
            self.ensure_cursor_visible();
            self.sync_selection_with_cursor();
        }
        Ok(())
    }

    pub(super) fn cmd_previous_matching_bracket(&mut self) -> Result<(), RmuxError> {
        if let Some(position) = self.find_matching_bracket(false) {
            self.cursor = position;
            self.ensure_cursor_visible();
            self.sync_selection_with_cursor();
        }
        Ok(())
    }

    pub(super) fn cmd_next_paragraph(&mut self) -> Result<(), RmuxError> {
        for y in self.cursor.y.saturating_add(1)..self.total_lines() {
            let current_blank = self.line_blank(y);
            let previous_blank = self.line_blank(y.saturating_sub(1));
            if current_blank != previous_blank && !current_blank {
                self.cursor.y = y;
                self.cursor.x = 0;
                self.ensure_cursor_visible();
                self.sync_selection_with_cursor();
                break;
            }
        }
        Ok(())
    }

    pub(super) fn cmd_previous_paragraph(&mut self) -> Result<(), RmuxError> {
        for y in (0..self.cursor.y).rev() {
            let current_blank = self.line_blank(y);
            let next_blank = self.line_blank(y.saturating_add(1));
            if current_blank != next_blank && !current_blank {
                self.cursor.y = y;
                self.cursor.x = 0;
                self.ensure_cursor_visible();
                self.sync_selection_with_cursor();
                break;
            }
        }
        Ok(())
    }

    pub(super) fn cmd_next_word(&mut self) -> Result<(), RmuxError> {
        if let Some(position) = self.find_word_boundary(self.cursor, WordBoundary::NextStart) {
            self.cursor = position;
            self.ensure_cursor_visible();
        }
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_next_word_end(&mut self) -> Result<(), RmuxError> {
        if let Some(position) = self.find_word_boundary(self.cursor, WordBoundary::NextEnd) {
            self.cursor = position;
            self.ensure_cursor_visible();
        }
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_previous_word(&mut self) -> Result<(), RmuxError> {
        if let Some(position) = self.find_word_boundary(self.cursor, WordBoundary::PreviousStart) {
            self.cursor = position;
            self.ensure_cursor_visible();
        }
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_next_space(&mut self) -> Result<(), RmuxError> {
        if let Some(position) = self.find_space_boundary(self.cursor, WordBoundary::NextStart) {
            self.cursor = position;
            self.ensure_cursor_visible();
        }
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_next_space_end(&mut self) -> Result<(), RmuxError> {
        if let Some(position) = self.find_space_boundary(self.cursor, WordBoundary::NextEnd) {
            self.cursor = position;
            self.ensure_cursor_visible();
        }
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_previous_space(&mut self) -> Result<(), RmuxError> {
        if let Some(position) = self.find_space_boundary(self.cursor, WordBoundary::PreviousStart) {
            self.cursor = position;
            self.ensure_cursor_visible();
        }
        self.sync_selection_with_cursor();
        Ok(())
    }

    pub(super) fn cmd_set_mark(&mut self) -> Result<(), RmuxError> {
        self.mark = Some(self.cursor);
        self.show_mark = true;
        Ok(())
    }

    pub(super) fn cmd_jump_to_mark(&mut self) -> Result<(), RmuxError> {
        if let Some(mark) = self.mark {
            self.cursor = mark;
            self.ensure_cursor_visible();
            self.sync_selection_with_cursor();
        }
        Ok(())
    }

    pub(super) fn move_cursor_to_mouse(&mut self, x: u32, y: u16) {
        if self.total_lines() == 0 {
            return;
        }
        self.cursor.y = (self.top_line + usize::from(y)).min(self.total_lines().saturating_sub(1));
        self.cursor.x = self.owning_or_zero(self.cursor.y, x.min(self.cols().saturating_sub(1)));
        self.ensure_cursor_visible();
        self.sync_selection_with_cursor();
    }

    pub(super) fn scroll_to_mouse(&mut self, slider_mpos: i32, mouse_y: u16) {
        if slider_mpos < 0 {
            return;
        }
        let sb_height = usize::from(self.rows().max(1));
        let size = self.bottom_top_line();
        if sb_height == 0 || size == 0 {
            return;
        }

        let slider_height = scrollbar_slider_height(self.rows().max(1), size);
        let new_slider_y = usize::from(mouse_y)
            .saturating_sub(slider_mpos as usize)
            .min(sb_height.saturating_sub(slider_height));
        let new_offset = ((new_slider_y as f64)
            * (((size + sb_height) as f64) / (sb_height as f64)))
            .floor() as usize;
        let offset = size.saturating_sub(self.top_line);
        let delta = offset as isize - new_offset as isize;

        if delta >= 0 {
            let shift = usize::try_from(delta).unwrap_or(usize::MAX);
            let actual = (self.top_line + shift).min(size) - self.top_line;
            self.top_line += actual;
            self.cursor.y = self.cursor.y.saturating_sub(actual);
        } else {
            let shift = delta.unsigned_abs();
            let actual = self.top_line.min(shift);
            self.top_line -= actual;
            self.cursor.y = (self.cursor.y + actual).min(self.total_lines().saturating_sub(1));
        }

        self.sync_selection_with_cursor();
    }
}
