//! Printing characters into the grid: wide cells, insert mode, autowrap and
//! the plain-ASCII fast path.

use super::{acs, Screen};
use crate::grid::{GridCell, GridCellFlags};
use crate::input::{mode, CellState, ScreenWriter, COLOUR_DEFAULT};
use crate::utf8::CombinedSequenceAction;

impl Screen {
    pub(super) fn logical_insert_column(&self) -> u32 {
        let x = self.cursor_column();
        let Some(line) = self.grid.visible_line(self.cursor_y) else {
            return x;
        };
        let Some(owner_x) = line.owning_cell_x(x).filter(|owner_x| *owner_x != x) else {
            return x;
        };
        let width = line
            .cell(owner_x)
            .map_or(1, |cell| u32::from(cell.width()).max(1));
        owner_x.saturating_add(width)
    }

    pub(super) fn overwrite_for_write(&mut self, x: u32, width: u32) {
        let sx = self.grid.sx();
        let blank = GridCell::blank_with_bg(COLOUR_DEFAULT);
        let Some(line) = self.current_line_mut() else {
            return;
        };

        let current_is_padding = line.is_padding_cell(x);
        if current_is_padding {
            if let Some(owner_x) = line.owning_cell_x(x).filter(|owner_x| *owner_x != x) {
                if let Some(owner) = line.cell_mut(owner_x) {
                    *owner = blank.clone();
                }
            }
        }

        let clear_following_padding = width != 1
            || line
                .cell(x)
                .is_some_and(|cell| cell.width() != 1 || cell.is_padding());
        if clear_following_padding {
            let mut clear_x = x.saturating_add(width);
            while clear_x < sx && line.is_padding_cell(clear_x) {
                if let Some(cell) = line.cell_mut(clear_x) {
                    *cell = blank.clone();
                }
                clear_x += 1;
            }
        }

        line.touch();
    }

    pub(super) fn write_char(&mut self, ch: char, cell: &CellState, acs: bool) {
        if self.grid.sx() == 0 || self.grid.sy() == 0 {
            return;
        }
        self.clear_selected_cells();

        let ch = if acs { acs::translate_acs(ch) } else { ch };
        let discard_action = self.combined_sequence_action(ch);
        if matches!(discard_action, CombinedSequenceAction::Discard) {
            return;
        }
        let requested_width = u32::from(self.utf8_config.width(ch));
        if self.combine_char(
            ch,
            matches!(discard_action, CombinedSequenceAction::Standalone),
        ) {
            return;
        }
        // A cell wider than the entire viewport cannot be represented with
        // its normal padding. Preserve the glyph as a one-column cell rather
        // than creating an out-of-bounds wide owner.
        let width = requested_width.clamp(1, self.grid.sx());

        let wrap_enabled = (self.mode & mode::MODE_WRAP) != 0;
        let mut automatic_wrap_continuation = self.pending_wrap && wrap_enabled;
        self.apply_pending_wrap();

        if (self.mode & mode::MODE_INSERT) != 0 {
            let insert_x = self.logical_insert_column();
            if insert_x >= self.grid.sx() {
                if !wrap_enabled {
                    return;
                }
                if let Some(line) = self.current_line_mut() {
                    line.set_wrapped(true);
                }
                self.linefeed(false, COLOUR_DEFAULT);
                self.cursor_x = 0;
                automatic_wrap_continuation = true;
            } else {
                self.cursor_x = insert_x;
            }
        }

        let write_would_cross_right_edge =
            width > self.grid.sx() || self.cursor_x > self.grid.sx().saturating_sub(width);
        if !wrap_enabled && width > 1 && write_would_cross_right_edge {
            return;
        }

        // tmux shifts the current row before an auto-wrapped wide glyph moves
        // to the next row; the no-wrap rejection above must leave it untouched.
        if (self.mode & mode::MODE_INSERT) != 0 {
            <Self as ScreenWriter>::insert_character(self, width, cell.bg());
        }

        if wrap_enabled && write_would_cross_right_edge {
            let gap_start = self.cursor_column();
            if let Some(line) = self.current_line_mut() {
                line.mark_unused_suffix_as_reflow_gap(gap_start);
                line.set_wrapped(true);
            }
            self.linefeed(false, COLOUR_DEFAULT);
            self.cursor_x = 0;
            automatic_wrap_continuation = true;
        }

        if self.cursor_y >= self.grid.sy()
            || self.cursor_column() > self.grid.sx().saturating_sub(width)
        {
            return;
        }

        let x = self.cursor_column();
        if x == 0 && !automatic_wrap_continuation {
            self.break_previous_wrapped_line();
        }
        self.overwrite_for_write(x, width);
        if let Some(line) = self.current_line_mut() {
            if let Some(target) = line.cell_mut(x) {
                *target = GridCell::from_state(
                    ch,
                    u8::try_from(width).unwrap_or(1),
                    cell,
                    GridCellFlags::default(),
                );
            }
            for offset in 1..width {
                if let Some(padding) = line.cell_mut(x + offset) {
                    *padding = GridCell::from_state(' ', 0, cell, GridCellFlags::PADDING);
                }
            }
            line.touch();
        }

        if wrap_enabled && x + width >= self.grid.sx() {
            self.cursor_x = self.max_cursor_x();
            self.pending_wrap = true;
        } else {
            self.cursor_x = x.saturating_add(width).min(self.max_cursor_x());
            self.pending_wrap = false;
        }
    }

    pub(super) fn write_plain_ascii_run(
        &mut self,
        mut bytes: &[u8],
        cell: &CellState,
        acs: bool,
    ) -> bool {
        if bytes.is_empty() {
            return true;
        }
        if acs
            || (self.mode & mode::MODE_INSERT) != 0
            || cell.attr() != 0
            || cell.fg() != COLOUR_DEFAULT
            || cell.bg() != COLOUR_DEFAULT
            || cell.us() != COLOUR_DEFAULT
            || cell.link() != 0
            || self.grid.sx() == 0
            || self.grid.sy() == 0
        {
            return false;
        }
        self.clear_selected_cells();

        while !bytes.is_empty() {
            let automatic_wrap_continuation =
                self.pending_wrap && (self.mode & mode::MODE_WRAP) != 0;
            self.apply_pending_wrap();
            if self.cursor_y >= self.grid.sy() {
                self.write_ascii_run_slow(bytes, cell, acs);
                return true;
            }

            let sx = self.grid.sx();
            let x = self.cursor_column();
            if x == 0 && !automatic_wrap_continuation {
                self.break_previous_wrapped_line();
            }

            if (self.mode & mode::MODE_WRAP) == 0 {
                let available = sx.saturating_sub(x) as usize;
                if bytes.len() > available {
                    self.write_ascii_run_slow(bytes, cell, acs);
                    return true;
                }
            }

            let writable = sx.saturating_sub(x) as usize;
            if writable == 0 {
                self.write_ascii_run_slow(bytes, cell, acs);
                return true;
            }
            let chunk_len = bytes.len().min(writable);
            let (chunk, rest) = bytes.split_at(chunk_len);
            let wrote_chunk = self
                .current_line_mut()
                .is_some_and(|line| line.write_plain_ascii_run(x, chunk));
            if !wrote_chunk {
                // Earlier chunks in this run may already have changed prior
                // rows. Consume only the untouched suffix through the general
                // writer so the caller never replays bytes already written.
                self.write_ascii_run_slow(bytes, cell, acs);
                return true;
            }

            if (self.mode & mode::MODE_WRAP) != 0 && x + chunk_len as u32 >= sx {
                self.cursor_x = self.max_cursor_x();
                self.pending_wrap = true;
            } else {
                self.cursor_x = x.saturating_add(chunk_len as u32).min(self.max_cursor_x());
                self.pending_wrap = false;
            }
            bytes = rest;
        }
        true
    }

    fn write_ascii_run_slow(&mut self, bytes: &[u8], cell: &CellState, acs: bool) {
        for &byte in bytes {
            self.write_char(char::from(byte), cell, acs);
        }
    }

    fn break_previous_wrapped_line(&mut self) {
        if self.cursor_y == 0 {
            return;
        }
        if let Some(previous) = self.grid.visible_line_mut(self.cursor_y - 1) {
            previous.set_wrapped(false);
        }
    }
}
