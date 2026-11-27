//! Erasing cells, line ranges and the viewport, and repairing wide cells an
//! erase splits.

use super::Screen;
use crate::grid::{GridCell, GridLine};

impl Screen {
    pub(super) fn blank_cell(&self, bg: i32) -> GridCell {
        GridCell::blank_with_bg(bg)
    }

    pub(super) fn repair_wide_cells_on_line(line: &mut GridLine, sx: u32, bg: i32) {
        let blank = GridCell::blank_with_bg(bg);
        let mut changed = false;
        let mut x = 0;

        while x < sx {
            let Some(cell) = line.cell(x) else {
                x += 1;
                continue;
            };

            if cell.is_padding() {
                if line.owning_cell_x(x).is_none() {
                    if let Some(target) = line.cell_mut(x) {
                        *target = blank.clone();
                        changed = true;
                    }
                }
                x += 1;
                continue;
            }

            let width = u32::from(cell.width());
            if width <= 1 {
                x += 1;
                continue;
            }

            let mut valid = x.saturating_add(width) <= sx;
            if valid {
                for offset in 1..width {
                    let column = x + offset;
                    let valid_padding = line
                        .cell(column)
                        .is_some_and(|candidate| candidate.is_padding())
                        && line.owning_cell_x(column) == Some(x);
                    if !valid_padding {
                        valid = false;
                        break;
                    }
                }
            }

            if valid {
                x += width;
                continue;
            }

            if let Some(target) = line.cell_mut(x) {
                *target = blank.clone();
                changed = true;
            }
            x += 1;
        }

        if changed {
            line.touch();
        }
    }

    pub(super) fn clear_line_range(&mut self, y: u32, start: u32, end_inclusive: u32, bg: i32) {
        self.clear_selected_cells();
        let sx = self.grid.sx();
        let end = end_inclusive.min(sx.saturating_sub(1));
        let clears_whole_line = start == 0 && end == sx.saturating_sub(1);
        if clears_whole_line {
            self.grid.break_wrap_before_visible_line(y);
        }
        let Some(line) = self.grid.visible_line_mut(y) else {
            return;
        };
        for x in start.min(sx)..=end {
            if let Some(cell) = line.cell_mut(x) {
                *cell = GridCell::blank_with_bg(bg);
            }
        }
        Self::repair_wide_cells_on_line(line, sx, bg);
        if clears_whole_line {
            line.set_wrapped(false);
            line.clear_shell_marks();
            line.forget_hidden_columns();
        }
        line.touch();
    }

    /// Erases the whole viewport, honouring the `scroll-on-clear` policy.
    ///
    /// `ED 2` and `ED 0` issued from the home cell funnel here, so DECCOLM,
    /// which reuses `clear_screen`, follows the same policy as measured on
    /// tmux 3.7b. [`full_reset`] does not funnel here, but it consults the same
    /// policy: it transfers the used rows through
    /// `Grid::clear_visible_to_history` when the policy is on, which moves
    /// nothing on the alternate screen. The two off branches are NOT
    /// identical — this one breaks the wrap into the first visible row and
    /// `full_reset` deliberately does not, a difference left standing rather
    /// than folded into the reset change.
    ///
    /// This helper's own two branches — the transferring one and the in-place
    /// one — both paint the erased cells with `bg`, the background the parser
    /// holds when the erase arrives, matching tmux cell for cell on these
    /// entries. [`full_reset`] is not one of them: it keeps its default
    /// background, as RIS1 established. Rows already transferred to scrollback
    /// are cloned before the paint, so they keep the styles they were written
    /// with; only the viewport being erased takes `bg`.
    ///
    /// The paint is not content: a cell this erase painted is materialised but
    /// logically empty, so a further erase over untouched rows moves nothing to
    /// scrollback, exactly as tmux's `cellused` stays at zero when
    /// `grid_empty_line` re-materialises a line with a background. That
    /// distinction lives in the grid cell layer, not in this helper.
    ///
    /// [`full_reset`]: crate::input::ScreenWriter::full_reset
    pub(super) fn clear_whole_screen(&mut self, bg: i32) {
        self.clear_selected_cells();
        if self.scroll_on_clear {
            self.grid.clear_visible_to_history(bg);
        } else {
            self.grid.clear_visible_without_history(bg);
        }
    }

    pub(super) fn clear_screen_region(&mut self, start_y: u32, end_y_inclusive: u32, bg: i32) {
        self.clear_selected_cells();
        for y in start_y..=end_y_inclusive.min(self.grid.sy().saturating_sub(1)) {
            self.grid.break_wrap_before_visible_line(y);
            if let Some(line) = self.grid.visible_line_mut(y) {
                line.clear(bg);
            }
        }
    }
}
