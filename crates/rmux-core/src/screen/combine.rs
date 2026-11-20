//! Combining characters: the cell a combining mark joins, and the
//! per-cell state that discards an overlong combined sequence.

use super::Screen;
use crate::grid::GridCellFlags;
use crate::input::mode;
use crate::utf8::{
    combine_char as utf8_combine_char, CombineResult, CombinedSequenceAction,
    CombinedSequenceDiscard,
};

impl Screen {
    fn combining_target(&self) -> Option<(u32, u32)> {
        let mut x = self.cursor_column();
        if self.pending_wrap {
            x = self.max_cursor_x();
        } else if x == 0 {
            return None;
        } else {
            x -= 1;
        }
        let line = self.grid.visible_line(self.cursor_y)?;
        Some((line.owning_cell_x(x).unwrap_or(x), self.cursor_y))
    }

    pub(super) fn combined_sequence_discard_at_cursor(&self) -> CombinedSequenceDiscard {
        let Some((x, y)) = self.combining_target() else {
            return CombinedSequenceDiscard::default();
        };
        self.grid
            .visible_line(y)
            .map_or_else(CombinedSequenceDiscard::default, |line| {
                line.combined_sequence_discard(x)
            })
    }

    pub(super) fn combined_sequence_action(&mut self, ch: char) -> CombinedSequenceAction {
        let Some((x, y)) = self.combining_target() else {
            return CombinedSequenceAction::Combine;
        };
        let active = self
            .grid
            .visible_line(y)
            .is_some_and(|line| line.combined_sequence_discard(x).is_active());
        if !active {
            return CombinedSequenceAction::Combine;
        }
        let Some(cell) = self
            .grid
            .visible_line_mut(y)
            .and_then(|line| line.cell_mut(x))
        else {
            return CombinedSequenceAction::Combine;
        };
        cell.combined_sequence_discard_mut()
            .action_for(ch, &self.utf8_config)
    }

    pub(super) fn clear_combined_sequence_discard_at_cursor(&mut self) {
        let Some((x, y)) = self.combining_target() else {
            return;
        };
        let active = self
            .grid
            .visible_line(y)
            .is_some_and(|line| line.combined_sequence_discard(x).is_active());
        if !active {
            return;
        }
        if let Some(cell) = self
            .grid
            .visible_line_mut(y)
            .and_then(|line| line.cell_mut(x))
        {
            cell.clear_combined_sequence_discard();
        }
    }

    pub(super) fn clear_combined_sequence_discards(&mut self) {
        self.grid.clear_combined_sequence_discards();
        if let Some(saved) = &mut self.saved_grid {
            saved.grid.clear_combined_sequence_discards();
        }
    }

    pub(super) fn combine_char(&mut self, ch: char, force_standalone: bool) -> bool {
        if force_standalone {
            return matches!(
                utf8_combine_char(None, ch, &self.utf8_config),
                CombineResult::Discard
            );
        }
        let Some((target_x, target_y)) = self.combining_target() else {
            return matches!(
                utf8_combine_char(None, ch, &self.utf8_config),
                CombineResult::Discard
            );
        };

        let Some(previous) = self.grid.visible_line(target_y).and_then(|line| {
            line.cell(target_x)
                .map(|cell| (cell.text().to_owned(), cell.width()))
        }) else {
            return matches!(
                utf8_combine_char(None, ch, &self.utf8_config),
                CombineResult::Discard
            );
        };
        let result = utf8_combine_char(
            Some((previous.0.as_str(), previous.1)),
            ch,
            &self.utf8_config,
        );

        match result {
            CombineResult::Standalone { .. } => false,
            CombineResult::Overflow(discard) => {
                if let Some(cell) = self
                    .grid
                    .visible_line_mut(target_y)
                    .and_then(|line| line.cell_mut(target_x))
                {
                    *cell.combined_sequence_discard_mut() = discard;
                }
                true
            }
            CombineResult::Discard => true,
            CombineResult::Combined { text, width } => {
                let previous_width = previous.1;
                let available_width = self.grid.sx().saturating_sub(target_x).max(1);
                let width = width.min(u8::try_from(available_width).unwrap_or(u8::MAX));
                if width != previous_width {
                    // Width promotion may replace the owner of an adjacent
                    // wide cell. Clear that owner's displaced padding before
                    // installing the new owner/padding pair.
                    self.overwrite_for_write(target_x, u32::from(width));
                }
                let Some(line) = self.grid.visible_line_mut(target_y) else {
                    return true;
                };
                if let Some(cell) = line.cell_mut(target_x) {
                    cell.set_text(text);
                    cell.set_width(width);
                    if width == 2 {
                        let mut padding = cell.clone();
                        padding.set_text(" ".to_owned());
                        padding.set_width(0);
                        padding.set_flags(GridCellFlags::PADDING);
                        if let Some(padding_cell) = line.cell_mut(target_x + 1) {
                            *padding_cell = padding;
                        }
                    }
                    line.touch();
                }
                if previous_width == 1 && width == 2 && !self.pending_wrap {
                    let next_cursor = target_x.saturating_add(2);
                    if next_cursor >= self.grid.sx() {
                        self.cursor_x = self.max_cursor_x();
                        self.pending_wrap = (self.mode & mode::MODE_WRAP) != 0;
                    } else {
                        self.cursor_x = next_cursor;
                    }
                }
                true
            }
        }
    }
}
