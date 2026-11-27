use rmux_proto::RmuxError;

use super::text::{owner_positions, scrollbar_slider_height, WordBoundary};
use super::types::{CopyPosition, ModeKeys};
use super::{CopyModeState, RecentreTopBottomPosition, RecentreTopBottomState};

/// The column that page motions keep, with the length of the line where it
/// was taken (tmux's `lastcx` and `lastsx`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct PageColumn {
    x: u32,
    line_length: u32,
}

#[derive(Debug, Clone, Copy)]
enum PageSize {
    Full,
    Half,
}

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
        self.cursor.x = self.cursor_limit(self.cursor.y);
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
        self.page_up(PageSize::Full);
        Ok(())
    }

    pub(super) fn cmd_page_down(&mut self) -> Result<(), RmuxError> {
        self.page_down(PageSize::Full);
        Ok(())
    }

    pub(super) fn cmd_halfpage_up(&mut self) -> Result<(), RmuxError> {
        self.page_up(PageSize::Half);
        Ok(())
    }

    pub(super) fn cmd_halfpage_down(&mut self) -> Result<(), RmuxError> {
        self.page_down(PageSize::Half);
        Ok(())
    }

    /// tmux's `window_copy_pageup1`: the view moves up by the page step and
    /// the cursor keeps its screen row; at the top of the history the view
    /// stops and the cursor moves up that step on the screen instead.
    fn page_up(&mut self, size: PageSize) {
        let column = self.take_page_column();
        let step = self.page_step(size);
        let mut screen_y = self.cursor_screen_row();
        if self.top_line < step {
            self.top_line = 0;
            screen_y = screen_y.saturating_sub(step);
        } else {
            self.top_line -= step;
        }
        self.cursor.y = self.top_line + screen_y;
        self.finish_page_motion(column);
    }

    /// tmux's `window_copy_pagedown1`: the mirror of [`Self::page_up`],
    /// stopping on the last screen row at the bottom of the history.
    fn page_down(&mut self, size: PageSize) {
        let column = self.take_page_column();
        let step = self.page_step(size);
        let mut screen_y = self.cursor_screen_row();
        let bottom = self.bottom_top_line();
        if bottom.saturating_sub(self.top_line) < step {
            self.top_line = bottom;
            screen_y = (screen_y + step).min(usize::from(self.rows()) - 1);
        } else {
            self.top_line += step;
        }
        self.cursor.y = (self.top_line + screen_y).min(self.total_lines() - 1);
        self.finish_page_motion(column);
    }

    /// A page is the pane height less two rows, half a page is half of it,
    /// and a pane of one or two rows pages by one row.
    fn page_step(&self, size: PageSize) -> usize {
        let rows = usize::from(self.rows());
        match size {
            _ if rows <= 2 => 1,
            PageSize::Full => rows - 2,
            PageSize::Half => rows / 2,
        }
    }

    fn cursor_screen_row(&self) -> usize {
        self.cursor
            .y
            .saturating_sub(self.top_line)
            .min(usize::from(self.rows()) - 1)
    }

    /// tmux's `lastcx` and `lastsx`: a cursor that is not at the end of its
    /// line records its column and that line's length, and a cursor at the end
    /// of its line keeps the column recorded before.
    fn take_page_column(&mut self) -> u32 {
        let length = self.line_length(self.cursor.y);
        if self.cursor.x != length {
            self.page_column = PageColumn {
                x: self.cursor.x,
                line_length: length,
            };
        }
        self.page_column.x
    }

    /// Outside a rectangle selection, the cursor goes to the end of its new
    /// line when it was at the end of the recorded line or would fall past
    /// the end of the new one.
    fn finish_page_motion(&mut self, column: u32) {
        let line = self.line(self.cursor.y);
        self.cursor.x = line.owning_cell_x(column).unwrap_or(column);
        if self.selection.is_none() || !self.rectangle {
            let length = self.line_length(self.cursor.y);
            if (column >= self.page_column.line_length && column != length) || column > length {
                self.cursor.y = self.logical_line_end_y(self.cursor.y);
                self.cursor.x = self.cursor_limit(self.cursor.y);
            }
        }
        self.ensure_cursor_visible();
        self.sync_selection_with_cursor();
    }

    /// tmux's `grid_line_length`: the columns up to the last non-blank cell.
    pub(super) fn line_length(&self, y: usize) -> u32 {
        let line = self.line(y);
        owner_positions(&line)
            .into_iter()
            .rev()
            .find_map(|x| {
                line.cell(x)
                    .filter(|cell| !cell.text().chars().all(char::is_whitespace))
                    .map(|cell| x + u32::from(cell.width().max(1)))
            })
            .map_or(0, |length| length.min(line.width()))
    }

    /// tmux's `window_copy_cursor_limit`: the end of a line is past its last
    /// cell in emacs keys and on its last cell in vi keys.
    fn cursor_limit(&self, y: usize) -> u32 {
        let length = self.line_length(y);
        match self.mode_keys {
            ModeKeys::Vi => length.saturating_sub(1),
            ModeKeys::Emacs => length,
        }
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

    pub(super) fn cmd_recentre_top_bottom(&mut self) -> Result<(), RmuxError> {
        let line = self.cursor.y;
        let position = match self.recentre_top_bottom {
            Some(state) if state.line == line => state.next,
            _ => RecentreTopBottomPosition::Middle,
        };

        match position {
            RecentreTopBottomPosition::Middle => self.cmd_scroll_middle()?,
            RecentreTopBottomPosition::Top => self.cmd_scroll_top()?,
            RecentreTopBottomPosition::Bottom => self.cmd_scroll_bottom()?,
        }
        self.recentre_top_bottom = Some(RecentreTopBottomState {
            line,
            next: position.next(),
        });
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

    pub(super) fn cmd_goto_line(
        &mut self,
        line: &str,
        absolute_line_numbers: bool,
    ) -> Result<(), RmuxError> {
        let Ok(number) = line.parse::<isize>() else {
            return Ok(());
        };
        // tmux accepts -1 as the sentinel for the oldest history line, but
        // values below -1 fail parsing and leave the viewport unchanged.
        if number < -1 {
            return Ok(());
        }
        let max_scroll = self.bottom_top_line();
        let scroll = if absolute_line_numbers {
            let line = if number <= 0 {
                1
            } else {
                usize::try_from(number)
                    .unwrap_or(max_scroll.saturating_add(1))
                    .min(max_scroll.saturating_add(1))
            };
            max_scroll.saturating_sub(line.saturating_sub(1))
        } else {
            usize::try_from(number)
                .unwrap_or(max_scroll)
                .min(max_scroll)
        };
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

    pub(super) fn cmd_next_prompt(&mut self, output: bool) -> Result<(), RmuxError> {
        self.move_to_shell_mark(self.cursor.y.saturating_add(1)..self.total_lines(), output);
        Ok(())
    }

    pub(super) fn cmd_previous_prompt(&mut self, output: bool) -> Result<(), RmuxError> {
        self.move_to_shell_mark((0..self.cursor.y).rev(), output);
        Ok(())
    }

    fn move_to_shell_mark(&mut self, rows: impl Iterator<Item = usize>, output: bool) {
        for y in rows {
            if self.backing.absolute_line_view(y).is_some_and(|line| {
                if output {
                    line.start_output()
                } else {
                    line.start_prompt()
                }
            }) {
                self.cursor.y = y;
                self.cursor.x = 0;
                // tmux anchors a history match at the top, and restores the
                // live viewport when the match belongs to the visible screen.
                self.top_line = y.min(self.bottom_top_line());
                self.sync_selection_with_cursor();
                break;
            }
        }
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
            // Every backing change clamps the mark already; clamping again
            // here keeps a later backing path from handing `line()` a row past
            // the screen.
            self.cursor = self.clamp_position(mark);
            self.ensure_cursor_visible();
            self.sync_selection_with_cursor();
        }
        Ok(())
    }

    pub(super) fn reposition_cursor_to_mouse(&mut self, x: u32, y: u16) {
        if self.total_lines() == 0 {
            return;
        }
        self.cursor.y = (self.top_line + usize::from(y)).min(self.total_lines().saturating_sub(1));
        self.cursor.x = self.owning_or_zero(self.cursor.y, x.min(self.cols().saturating_sub(1)));
        self.ensure_cursor_visible();
    }

    pub(super) fn move_cursor_to_mouse(&mut self, x: u32, y: u16) {
        self.reposition_cursor_to_mouse(x, y);
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
