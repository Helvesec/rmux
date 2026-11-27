use crate::grid::GridLineFlags;
use crate::input::mode;
use crate::input::{CellState, InputEndType, ScreenWriter};
#[cfg(feature = "kitty-keyboard")]
use crate::kitty_keyboard::{Control, Effect};
use crate::TerminalPassthrough;

use super::Screen;

mod alternate;
mod metadata;
mod reset;
mod tabs;

#[cfg(test)]
use tabs::cursor_backward_tab_steps;

impl ScreenWriter for Screen {
    #[cfg(feature = "kitty-keyboard")]
    fn keyboard_control(&mut self, control: Control) -> Effect {
        self.keyboard.apply(self.keyboard_screen(), control)
    }

    fn collect_add(&mut self, ch: char, cell: &CellState) {
        self.write_char(ch, cell, false);
    }

    fn collect_add_with_charset(&mut self, ch: char, cell: &CellState, acs: bool) {
        self.write_char(ch, cell, acs);
    }

    fn collect_add_ascii_run(&mut self, bytes: &[u8], cell: &CellState, acs: bool) {
        if !acs && !bytes.is_empty() {
            self.clear_combined_sequence_discard_at_cursor();
        }
        if !self.write_plain_ascii_run(bytes, cell, acs) {
            for &byte in bytes {
                self.write_char(char::from(byte), cell, acs);
            }
        }
    }

    fn cursor_up(&mut self, n: u32) {
        self.clear_pending_wrap();
        // Positions above the scroll region keep the screen edge; all others
        // stop at the region's top margin.
        let minimum_y = if self.cursor_y >= self.rupper {
            self.rupper
        } else {
            0
        };
        self.cursor_y = self.cursor_y.saturating_sub(n).max(minimum_y);
    }

    fn cursor_down(&mut self, n: u32) {
        self.clear_pending_wrap();
        // Positions below the scroll region keep the screen edge; all others
        // stop at the region's bottom margin.
        let maximum_y = if self.cursor_y <= self.rlower {
            self.rlower
        } else {
            self.grid.sy().saturating_sub(1)
        };
        self.cursor_y = self.cursor_y.saturating_add(n).min(maximum_y);
    }

    fn cursor_left(&mut self, n: u32) {
        self.clear_pending_wrap();
        self.cursor_x = self.cursor_column().saturating_sub(n);
    }

    fn cursor_right(&mut self, n: u32) {
        self.clear_pending_wrap();
        self.cursor_x = self
            .cursor_column()
            .saturating_add(n)
            .min(self.max_cursor_x());
    }

    fn cursor_move(&mut self, col: i32, row: i32, origin_mode: bool) {
        self.clear_pending_wrap();
        let max_x = self.grid.sx().saturating_sub(1);
        let (min_y, max_y) = if origin_mode && (self.mode & mode::MODE_ORIGIN) != 0 {
            (self.rupper, self.rlower)
        } else {
            (0, self.grid.sy().saturating_sub(1))
        };

        if col >= 0 {
            self.cursor_x = (col as u32).min(max_x);
        }
        if row >= 0 {
            self.cursor_y = min_y.saturating_add(row as u32).min(max_y);
        }
    }

    fn insert_line(&mut self, n: u32, bg: i32) {
        self.clear_pending_wrap();
        self.clear_selected_cells();
        if self.cursor_outside_scroll_region() {
            self.grid.insert_lines_to_bottom(self.cursor_y, n, bg);
            return;
        }

        let upper = self.cursor_y;
        let lower = self.rlower;
        let lines = n.max(1).min(lower.saturating_sub(upper).saturating_add(1));
        self.grid.insert_lines(upper, lower, lines, bg);
    }

    fn delete_line(&mut self, n: u32, bg: i32) {
        self.clear_pending_wrap();
        self.clear_selected_cells();
        if self.cursor_outside_scroll_region() {
            self.grid.delete_lines_to_bottom(self.cursor_y, n, bg);
            return;
        }

        let upper = self.cursor_y;
        let lower = self.rlower;
        let lines = n.max(1).min(lower.saturating_sub(upper).saturating_add(1));
        for _ in 0..lines {
            self.grid.scroll_region_up(upper, lower, bg, false);
        }
    }

    fn scroll_up(&mut self, n: u32, bg: i32) {
        self.clear_pending_wrap();
        self.clear_selected_cells();
        let lines = n
            .max(1)
            .min(self.rlower.saturating_sub(self.rupper).saturating_add(1));
        for _ in 0..lines {
            self.grid
                .scroll_region_up(self.rupper, self.rlower, bg, self.rupper == 0);
        }
    }

    fn scroll_down(&mut self, n: u32, bg: i32) {
        self.clear_pending_wrap();
        self.clear_selected_cells();
        let lines = n
            .max(1)
            .min(self.rlower.saturating_sub(self.rupper).saturating_add(1));
        for _ in 0..lines {
            self.grid.scroll_region_down(self.rupper, self.rlower, bg);
        }
    }

    fn linefeed(&mut self, wrapped: bool, bg: i32) {
        self.pending_wrap = false;
        if wrapped {
            if let Some(line) = self.current_line_mut() {
                line.set_wrapped(true);
            }
        }

        if self.cursor_y == self.rlower {
            self.clear_selected_cells();
            self.grid
                .scroll_region_up(self.rupper, self.rlower, bg, self.rupper == 0);
        } else if self.cursor_y < self.grid.sy().saturating_sub(1) {
            self.cursor_y += 1;
        }
    }

    fn reverse_index(&mut self, bg: i32) {
        self.clear_pending_wrap();
        if self.cursor_y == self.rupper {
            self.clear_selected_cells();
            self.grid.scroll_region_down(self.rupper, self.rlower, bg);
        } else if self.cursor_y > 0 {
            self.cursor_y -= 1;
        }
    }

    fn carriage_return(&mut self) {
        self.pending_wrap = false;
        self.cursor_x = 0;
    }

    fn backspace(&mut self) {
        self.clear_pending_wrap();
        let cx = self.cursor_column();
        if cx > 0 {
            self.cursor_x = cx - 1;
            return;
        }
        if self.cursor_y == 0 {
            return;
        }
        if self
            .grid
            .visible_line(self.cursor_y - 1)
            .is_some_and(|line| line.flags().contains(GridLineFlags::WRAPPED))
        {
            self.cursor_y -= 1;
            self.cursor_x = self.max_cursor_x();
        }
    }

    fn insert_character(&mut self, n: u32, bg: i32) {
        if self.logical_cursor_column() >= self.grid.sx() {
            return;
        }
        self.clear_selected_cells();
        let sx = self.grid.sx();
        let x = self.logical_insert_column();
        if x >= sx {
            self.pending_wrap = (self.mode & crate::input::mode::MODE_WRAP) != 0;
            return;
        }
        self.cursor_x = x;
        let count = n.max(1).min(sx.saturating_sub(x));
        let blank = self.blank_cell(bg);
        if let Some(line) = self.current_line_mut() {
            line.insert_cells(x, count, &blank);
            Self::repair_wide_cells_on_line(line, sx, bg);
            line.touch();
        }
    }

    fn delete_character(&mut self, n: u32, bg: i32) {
        let x = self.logical_cursor_column();
        let sx = self.grid.sx();
        if x >= sx {
            return;
        }
        self.clear_selected_cells();
        let count = n.max(1).min(sx.saturating_sub(x));
        let blank = self.blank_cell(bg);
        let clears_whole_line = x == 0 && count == sx;
        if clears_whole_line {
            self.grid.break_wrap_before_visible_line(self.cursor_y);
        }
        if let Some(line) = self.current_line_mut() {
            line.delete_cells(x, count, &blank);
            Self::repair_wide_cells_on_line(line, sx, bg);
            if clears_whole_line {
                line.set_wrapped(false);
                line.clear_shell_marks();
            }
            line.touch();
        }
    }

    fn clear_character(&mut self, n: u32, bg: i32) {
        let x = self.logical_cursor_column();
        if x >= self.grid.sx() {
            return;
        }
        let end = x
            .saturating_add(n.max(1))
            .saturating_sub(1)
            .min(self.grid.sx().saturating_sub(1));
        self.clear_line_range(self.cursor_y, x, end, bg);
    }

    fn clear_end_of_screen(&mut self, bg: i32) {
        let x = self.logical_cursor_column();
        if self.cursor_y == 0 && x == 0 {
            self.clear_whole_screen(bg);
            return;
        }
        if self.cursor_y < self.grid.sy() && x < self.grid.sx() {
            self.clear_line_range(self.cursor_y, x, self.grid.sx().saturating_sub(1), bg);
        }
        if self.cursor_y + 1 < self.grid.sy() {
            self.clear_screen_region(self.cursor_y + 1, self.grid.sy().saturating_sub(1), bg);
        }
    }

    fn clear_start_of_screen(&mut self, bg: i32) {
        if self.cursor_y > 0 {
            self.clear_screen_region(0, self.cursor_y - 1, bg);
        }
        self.clear_line_range(self.cursor_y, 0, self.cursor_column(), bg);
    }

    fn clear_screen(&mut self, bg: i32) {
        self.clear_whole_screen(bg);
    }

    fn clear_history(&mut self) {
        self.clear_selected_cells();
        self.grid.clear_history();
    }

    fn clear_end_of_line(&mut self, bg: i32) {
        let x = self.logical_cursor_column();
        if x >= self.grid.sx() {
            return;
        }
        self.clear_line_range(self.cursor_y, x, self.grid.sx().saturating_sub(1), bg);
    }

    fn clear_start_of_line(&mut self, bg: i32) {
        self.clear_line_range(self.cursor_y, 0, self.cursor_column(), bg);
    }

    fn clear_line(&mut self, bg: i32) {
        self.clear_line_range(self.cursor_y, 0, self.grid.sx().saturating_sub(1), bg);
    }

    fn extended_key_policy(&self) -> crate::input::ExtendedKeyPolicy {
        self.extended_key_policy
    }

    fn mode_set(&mut self, mode_bits: u32) {
        if mode_bits & mode::MODE_SYNC != 0 {
            self.begin_synchronized_output();
        }
        self.mode |= mode_bits;
    }

    fn mode_clear(&mut self, mode_bits: u32) {
        if mode_bits & mode::MODE_SYNC != 0 {
            self.end_synchronized_output();
        }
        self.mode &= !mode_bits;
        if (self.mode & mode::MODE_WRAP) == 0 {
            self.clear_pending_wrap();
        }
    }

    fn set_scroll_region(&mut self, top: u32, bottom: u32) {
        let max_y = self.grid.sy().saturating_sub(1);
        let top = top.min(max_y);
        let bottom = bottom.min(max_y);
        if top >= bottom {
            return;
        }
        self.pending_wrap = false;
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.rupper = top;
        self.rlower = bottom;
    }

    fn alternate_on(&mut self, bg: i32, save_cursor: bool) {
        let mut rendition = CellState::default();
        rendition.cell.bg = bg;
        self.enter_alternate_screen(&rendition, save_cursor);
    }

    fn alternate_off(&mut self, _bg: i32, restore_cursor: bool) {
        self.leave_alternate_screen(restore_cursor);
    }

    fn alternate_on_with_rendition(&mut self, cell: &CellState, save_cursor: bool) {
        self.enter_alternate_screen(cell, save_cursor);
    }

    fn alternate_off_with_rendition(&mut self, cell: &mut CellState, restore_cursor: bool) {
        if let Some(rendition) = self.leave_alternate_screen(restore_cursor) {
            // tmux restores the `grid_cell` only; the character sets stay.
            cell.cell.clone_from(&rendition.cell);
        }
    }

    fn tab(&mut self) {
        self.move_to_next_tab_stop();
    }

    fn cursor_backward_tab(&mut self, n: u32) {
        self.move_back_tab_stops(n);
    }

    fn set_tab_stop(&mut self) {
        self.set_tab_stop_at_cursor();
    }

    fn clear_tab_stop(&mut self) {
        self.clear_tab_stop_at_cursor();
    }

    fn clear_all_tab_stops(&mut self) {
        self.tabs.fill(false);
    }

    fn set_title(&mut self, title: &str) {
        if self.title_rename_enabled {
            Screen::set_title(self, title);
        }
    }

    fn set_window_name(&mut self, name: &str) {
        self.accept_window_name(name);
    }

    fn set_path(&mut self, path: &str) {
        self.update_path(path);
    }

    fn save_cursor(&mut self) {
        self.saved_cursor_x = Some(self.cursor_x);
        self.saved_cursor_y = Some(self.cursor_y);
        self.saved_cursor_pending_wrap = self.pending_wrap;
    }

    fn restore_cursor(&mut self) {
        if let (Some(x), Some(y)) = (self.saved_cursor_x, self.saved_cursor_y) {
            self.restore_cursor_position(x, y, self.saved_cursor_pending_wrap);
        }
    }

    fn alignment_test(&mut self) {
        self.fill_alignment_pattern();
    }

    fn full_reset(&mut self) {
        self.reset_to_initial_state();
    }

    fn start_sync(&mut self) {
        self.begin_synchronized_output();
    }

    fn stop_sync(&mut self) {
        self.end_synchronized_output();
    }

    fn set_cursor_style(&mut self, n: u32) {
        if crate::cursor_style_is_valid(n) {
            self.cursor_style = n;
        }
    }

    fn osc_hyperlink(&mut self, data: &str) {
        let (internal_id, uri) = Self::parse_hyperlink(data);
        if uri.is_empty() {
            self.active_hyperlink = 0;
            self.bump_metadata_revision();
            return;
        }
        self.active_hyperlink = self.hyperlinks.put(&uri, internal_id.as_deref());
        self.bump_metadata_revision();
    }

    fn current_hyperlink_id(&self) -> u32 {
        self.active_hyperlink
    }

    fn bell(&mut self) {
        self.bell_count = self.bell_count.saturating_add(1);
    }

    fn apc_passthrough(&mut self, data: &[u8]) {
        self.push_terminal_passthrough(TerminalPassthrough::kitty_graphics(
            self.cursor_x,
            self.cursor_y,
            data.to_vec(),
        ));
    }

    fn dcs_passthrough(&mut self, data: &[u8]) {
        self.push_terminal_passthrough(TerminalPassthrough::raw(
            self.cursor_x,
            self.cursor_y,
            data.to_vec(),
        ));
    }

    fn restore_recovery_written_columns(&mut self, columns: u32) {
        if let Some(line) = self.grid.visible_line_mut(self.cursor_y) {
            line.restore_recovery_written_columns(columns);
        }
    }

    fn sixel_passthrough(&mut self, data: &[u8]) {
        self.push_terminal_passthrough(TerminalPassthrough::sixel(
            self.cursor_x,
            self.cursor_y,
            data.to_vec(),
        ));
    }

    fn sixel_passthrough_available(&self) -> bool {
        self.sixel_passthrough_available
    }

    fn cell_pixels(&self) -> Option<rmux_types::TerminalPixels> {
        self.cell_pixels
    }

    fn screen_size_x(&self) -> u32 {
        self.grid.sx()
    }

    fn screen_size_y(&self) -> u32 {
        self.grid.sy()
    }

    fn resolved_cursor_state(&self) -> crate::ResolvedCursorStyle {
        crate::resolve_cursor_style_state(
            self.cursor_style(),
            self.mode(),
            self.cursor_default_style(),
        )
    }

    fn cursor_x(&self) -> u32 {
        self.cursor_x
    }

    fn cursor_y(&self) -> u32 {
        self.cursor_y
    }

    fn alternate_active(&self) -> bool {
        self.is_alternate()
    }

    fn current_mode(&self) -> u32 {
        self.mode
    }

    fn push_title(&mut self) {
        self.push_title_stack();
    }

    fn pop_title(&mut self) {
        self.pop_title_stack();
    }

    fn osc_palette(&mut self, data: &str, _end: InputEndType) {
        self.relay_palette_queries(data);
    }
    fn osc_notification(&mut self, _data: &str) {}
    fn osc_fg_colour(&mut self, _data: &str, _end: InputEndType) {}
    fn osc_bg_colour(&mut self, _data: &str, _end: InputEndType) {}
    fn osc_cursor_colour(&mut self, _data: &str, _end: InputEndType) {}
    fn osc_clipboard(&mut self, data: &str, end: InputEndType) {
        self.relay_clipboard(data, end);
    }
    fn osc_reset_palette(&mut self, _data: &str) {}
    fn osc_reset_fg(&mut self) {}
    fn osc_reset_bg(&mut self) {}
    fn osc_reset_cursor(&mut self) {}
    fn osc_shell_integration(&mut self, data: &str) {
        // tmux 3.7b consumes A/C as line marks. B/D carry no public state.
        // Accept extension parameters, but require a complete command token.
        let output = match crate::input::ShellMark::parse(data) {
            Some(crate::input::ShellMark::Prompt) => {
                // OSC 7501: a new prompt expires active and idle reports.
                self.expire_program_status();
                false
            }
            Some(crate::input::ShellMark::Output) => true,
            _ => return,
        };
        if let Some(line) = self.grid.visible_line_mut(self.cursor_y) {
            line.mark_shell_line(output);
        }
    }
    fn supports_program_status(&self) -> bool {
        true
    }
    fn osc_program_status(&mut self, data: &str, end: InputEndType) {
        self.program_status.report(data, end == InputEndType::Bel);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cursor_backward_tab_steps_are_bounded_by_cursor_column() {
        assert_eq!(super::cursor_backward_tab_steps(0, 0), 1);
        assert_eq!(super::cursor_backward_tab_steps(1, 7), 1);
        assert_eq!(super::cursor_backward_tab_steps(u32::MAX, 7), 8);
    }
}
