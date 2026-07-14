use crate::grid::{Grid, GridCell, GridCellFlags, GridLineFlags};
use crate::input::mode;
use crate::input::{CellState, InputEndType, ScreenWriter, COLOUR_DEFAULT};
use crate::{TerminalPaletteIndex, TerminalPassthrough};

use super::{SavedGrid, Screen, TITLE_STACK_MAX};

fn cursor_backward_tab_steps(n: u32, current: u32) -> u32 {
    n.max(1).min(current.saturating_add(1))
}

impl ScreenWriter for Screen {
    fn collect_add(&mut self, ch: char, cell: &CellState) {
        self.write_char(ch, cell, false);
    }

    fn collect_add_with_charset(&mut self, ch: char, cell: &CellState, acs: bool) {
        self.write_char(ch, cell, acs);
    }

    fn collect_add_ascii_run(&mut self, bytes: &[u8], cell: &CellState, acs: bool) {
        if !self.write_plain_ascii_run(bytes, cell, acs) {
            for &byte in bytes {
                self.write_char(char::from(byte), cell, acs);
            }
        }
    }

    fn cursor_up(&mut self, n: u32) {
        self.clear_pending_wrap();
        self.cursor_y = self.cursor_y.saturating_sub(n);
    }

    fn cursor_down(&mut self, n: u32) {
        self.clear_pending_wrap();
        self.cursor_y = self
            .cursor_y
            .saturating_add(n)
            .min(self.grid.sy().saturating_sub(1));
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
        if self.cursor_y < self.rupper || self.cursor_y > self.rlower {
            return;
        }
        self.clear_selected_cells();

        let upper = self.cursor_y;
        let lower = self.rlower;
        let lines = n.max(1).min(lower.saturating_sub(upper).saturating_add(1));
        for _ in 0..lines {
            self.grid.scroll_region_down(upper, lower, bg);
        }
    }

    fn delete_line(&mut self, n: u32, bg: i32) {
        self.clear_pending_wrap();
        if self.cursor_y < self.rupper || self.cursor_y > self.rlower {
            return;
        }
        self.clear_selected_cells();

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
        self.clear_pending_wrap();
        self.clear_selected_cells();
        let x = self.cursor_column();
        let sx = self.grid.sx();
        let count = n.max(1).min(sx.saturating_sub(x));
        let blank = self.blank_cell(bg);
        if let Some(line) = self.current_line_mut() {
            line.insert_cells(x, count, &blank);
            Self::repair_wide_cells_on_line(line, sx, bg);
            line.touch();
        }
    }

    fn delete_character(&mut self, n: u32, bg: i32) {
        self.clear_pending_wrap();
        self.clear_selected_cells();
        let x = self.cursor_column();
        let sx = self.grid.sx();
        let count = n.max(1).min(sx.saturating_sub(x));
        let blank = self.blank_cell(bg);
        if let Some(line) = self.current_line_mut() {
            line.delete_cells(x, count, &blank);
            Self::repair_wide_cells_on_line(line, sx, bg);
            line.touch();
        }
    }

    fn clear_character(&mut self, n: u32, bg: i32) {
        self.clear_pending_wrap();
        let x = self.cursor_column();
        let end = x
            .saturating_add(n.max(1))
            .saturating_sub(1)
            .min(self.grid.sx().saturating_sub(1));
        self.clear_line_range(self.cursor_y, x, end, bg);
    }

    fn clear_end_of_screen(&mut self, bg: i32) {
        if self.cursor_y == 0 && self.cursor_column() == 0 {
            self.clear_selected_cells();
            self.grid.clear_visible_to_history(COLOUR_DEFAULT);
            return;
        }
        let x = self.cursor_column();
        if self.cursor_y < self.grid.sy() {
            self.clear_line_range(self.cursor_y, x, self.grid.sx().saturating_sub(1), bg);
        }
        if self.cursor_y + 1 < self.grid.sy() {
            self.clear_screen_region(
                self.cursor_y + 1,
                self.grid.sy().saturating_sub(1),
                COLOUR_DEFAULT,
            );
        }
    }

    fn clear_start_of_screen(&mut self, bg: i32) {
        if self.cursor_y > 0 {
            self.clear_screen_region(0, self.cursor_y - 1, COLOUR_DEFAULT);
        }
        self.clear_line_range(self.cursor_y, 0, self.cursor_column(), bg);
    }

    fn clear_screen(&mut self, _bg: i32) {
        self.clear_selected_cells();
        self.grid.clear_visible_to_history(COLOUR_DEFAULT);
    }

    fn clear_history(&mut self) {
        self.clear_selected_cells();
        self.grid.clear_history();
    }

    fn clear_end_of_line(&mut self, bg: i32) {
        self.clear_line_range(
            self.cursor_y,
            self.cursor_column(),
            self.grid.sx().saturating_sub(1),
            bg,
        );
    }

    fn clear_start_of_line(&mut self, bg: i32) {
        self.clear_line_range(self.cursor_y, 0, self.cursor_column(), bg);
    }

    fn clear_line(&mut self, bg: i32) {
        self.clear_line_range(self.cursor_y, 0, self.grid.sx().saturating_sub(1), bg);
    }

    fn mode_set(&mut self, mode_bits: u32) {
        self.mode |= mode_bits;
    }

    fn mode_clear(&mut self, mode_bits: u32) {
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
        if !self.alternate_screen_enabled {
            return;
        }
        if self.is_alternate() {
            return;
        }
        self.clear_selected_cells();

        let mut saved_grid = Grid::new(self.grid.size(), 0);
        saved_grid.replace_visible(self.grid.visible_lines());
        self.saved_grid = Some(SavedGrid {
            grid: saved_grid,
            history_enabled: self.grid.history_enabled(),
        });
        if save_cursor {
            self.saved_cursor_x = Some(self.cursor_x);
            self.saved_cursor_y = Some(self.cursor_y);
            self.saved_cursor_pending_wrap = self.pending_wrap;
            self.saved_state.cx = self.cursor_column();
            self.saved_state.cy = self.cursor_y;
        }

        self.grid.clear_visible(bg);
        self.grid.set_history_enabled(false);
        self.pending_wrap = false;
        if !save_cursor || !self.preserve_alternate_screen_cursor {
            self.cursor_x = 0;
            self.cursor_y = 0;
        }
    }

    fn alternate_off(&mut self, _bg: i32, restore_cursor: bool) {
        self.clear_selected_cells();
        let saved_cursor = if restore_cursor {
            self.saved_cursor_x
                .zip(self.saved_cursor_y)
                .map(|(x, y)| (x, y, self.saved_cursor_pending_wrap))
        } else {
            None
        };

        let Some(saved) = self.saved_grid.take() else {
            if let Some((x, y, pending_wrap)) = saved_cursor {
                self.restore_cursor_position(x, y, pending_wrap);
            }
            return;
        };

        let current_size = self.grid.size();
        let saved_size = saved.grid.size();
        if current_size.rows == saved_size.rows {
            self.grid.replace_visible_resized_width_only(
                saved_size,
                saved.grid.visible_lines(),
                COLOUR_DEFAULT,
            );
        } else {
            self.grid
                .resize_width(u32::from(saved_size.cols), COLOUR_DEFAULT);
            self.grid.resize_height(
                u32::from(saved_size.rows),
                &mut self.cursor_y,
                COLOUR_DEFAULT,
            );
            self.grid.replace_visible(saved.grid.visible_lines());
        }
        self.grid.set_history_enabled(saved.history_enabled);
        self.resize(current_size);
        if let Some((x, y, pending_wrap)) = saved_cursor {
            self.restore_cursor_position(x, y, pending_wrap);
        } else {
            self.pending_wrap = false;
        }
    }

    fn tab(&mut self) {
        self.clear_pending_wrap();
        let start = self.cursor_column();
        let next = ((start + 1) as usize..self.tabs.len())
            .find(|index| self.tabs[*index])
            .map(|index| index as u32)
            .unwrap_or_else(|| self.grid.sx().saturating_sub(1));
        self.cursor_x = next;
    }

    fn cursor_backward_tab(&mut self, n: u32) {
        self.clear_pending_wrap();
        let mut current = self.cursor_column();
        for _ in 0..cursor_backward_tab_steps(n, current) {
            if current == 0 {
                break;
            }
            let previous = (0..current as usize)
                .rev()
                .find(|index| self.tabs[*index])
                .map(|index| index as u32)
                .unwrap_or(0);
            current = previous;
        }
        self.cursor_x = current;
    }

    fn set_tab_stop(&mut self) {
        let column = self.cursor_column() as usize;
        if let Some(tab) = self.tabs.get_mut(column) {
            *tab = true;
        }
    }

    fn clear_tab_stop(&mut self) {
        let column = self.cursor_column() as usize;
        if let Some(tab) = self.tabs.get_mut(column) {
            *tab = false;
        }
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
        if self.title_rename_enabled {
            self.window_name = name.to_owned();
        }
    }

    fn set_path(&mut self, path: &str) {
        self.path = path.to_owned();
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
        self.clear_selected_cells();
        self.rupper = 0;
        self.rlower = self.grid.sy().saturating_sub(1);
        let sx = self.grid.sx();
        for y in 0..self.grid.sy() {
            if let Some(line) = self.grid.visible_line_mut(y) {
                for x in 0..sx {
                    if let Some(cell) = line.cell_mut(x) {
                        *cell = GridCell::from_state(
                            'E',
                            1,
                            &CellState::default(),
                            GridCellFlags::default(),
                        );
                    }
                }
                line.set_wrapped(false);
                line.touch();
            }
        }
    }

    fn full_reset(&mut self) {
        self.clear_selected_cells();
        if self.is_alternate() {
            self.alternate_off(COLOUR_DEFAULT, false);
        }
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.pending_wrap = false;
        self.rupper = 0;
        self.rlower = self.grid.sy().saturating_sub(1);
        self.mode = mode::MODE_CURSOR | mode::MODE_WRAP | (self.mode & mode::MODE_CRLF);
        self.grid.clear_visible(COLOUR_DEFAULT);
        self.reset_tabs();
        self.title_stack.clear();
        self.active_hyperlink = 0;
        self.hyperlinks.reset();
    }

    fn start_sync(&mut self) {
        self.mode |= mode::MODE_SYNC;
    }

    fn stop_sync(&mut self) {
        self.mode &= !mode::MODE_SYNC;
    }

    fn set_cursor_style(&mut self, n: u32) {
        self.cursor_style = n;
    }

    fn osc_hyperlink(&mut self, data: &str) {
        let (internal_id, uri) = Self::parse_hyperlink(data);
        if uri.is_empty() {
            self.active_hyperlink = 0;
            return;
        }
        self.active_hyperlink = self.hyperlinks.put(&uri, internal_id.as_deref());
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

    fn sixel_passthrough(&mut self, data: &[u8]) {
        self.push_terminal_passthrough(TerminalPassthrough::sixel(
            self.cursor_x,
            self.cursor_y,
            data.to_vec(),
        ));
    }

    fn screen_size_x(&self) -> u32 {
        self.grid.sx()
    }

    fn screen_size_y(&self) -> u32 {
        self.grid.sy()
    }

    fn cursor_x(&self) -> u32 {
        self.cursor_x
    }

    fn cursor_y(&self) -> u32 {
        self.cursor_y
    }

    fn current_mode(&self) -> u32 {
        self.mode
    }

    fn push_title(&mut self) {
        if !self.title_rename_enabled {
            return;
        }
        if self.title_stack.len() >= TITLE_STACK_MAX {
            let excess = self.title_stack.len() + 1 - TITLE_STACK_MAX;
            self.title_stack.drain(0..excess);
        }
        self.title_stack.push(self.title.clone());
    }

    fn pop_title(&mut self) {
        if !self.title_rename_enabled {
            return;
        }
        if let Some(title) = self.title_stack.pop() {
            self.title = title;
        }
    }

    fn osc_palette(&mut self, data: &str, _end: InputEndType) {
        // OSC 4 is a list of index/value pairs. tmux 3.7b forwards each valid
        // query as its own canonical ST-terminated OSC while handling palette
        // sets internally. RMUX does not maintain an outer-terminal palette,
        // so keep set behaviour unchanged and relay only strict, bounded
        // queries. The dedicated event kind bypasses generic raw-passthrough
        // policy without reflecting arbitrary OSC payloads.
        let mut fields = data.split(';');
        while let Some(index) = fields.next() {
            let Some(value) = fields.next() else {
                break;
            };
            if value != "?" {
                continue;
            }
            let Some(index) = TerminalPaletteIndex::parse(index) else {
                continue;
            };
            self.push_terminal_passthrough(TerminalPassthrough::palette_query(index));
        }
    }
    fn osc_notification(&mut self, _data: &str) {}
    fn osc_fg_colour(&mut self, _data: &str, _end: InputEndType) {}
    fn osc_bg_colour(&mut self, _data: &str, _end: InputEndType) {}
    fn osc_cursor_colour(&mut self, _data: &str, _end: InputEndType) {}
    fn osc_clipboard(&mut self, data: &str, end: InputEndType) {
        let mut sequence = Vec::with_capacity(data.len() + 7);
        sequence.extend_from_slice(b"\x1b]52;");
        sequence.extend_from_slice(data.as_bytes());
        match end {
            InputEndType::Bel => sequence.push(b'\x07'),
            InputEndType::St => sequence.extend_from_slice(b"\x1b\\"),
        }
        self.push_terminal_passthrough(TerminalPassthrough::clipboard(sequence));
    }
    fn osc_reset_palette(&mut self, _data: &str) {}
    fn osc_reset_fg(&mut self) {}
    fn osc_reset_bg(&mut self) {}
    fn osc_reset_cursor(&mut self) {}
    fn osc_shell_integration(&mut self, _data: &str) {}
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
