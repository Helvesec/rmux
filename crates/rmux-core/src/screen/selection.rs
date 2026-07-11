use crate::grid::GridCellFlags;
use crate::input::{GridAttr, COLOUR_DEFAULT, COLOUR_TERMINAL};
use crate::style::{style_parse, Style, StyleCell};

use super::Screen;

impl Screen {
    /// Returns whether any visible cell is currently marked as selected.
    #[must_use]
    pub fn has_selected_cells(&self) -> bool {
        self.has_selected_cells
    }

    /// Marks one visible row range as selected.
    pub fn mark_selected_row_range(&mut self, row: u32, start_x: u32, end_x: u32) {
        let line_width = self.grid.sx();
        if row >= self.grid.sy() || line_width == 0 {
            return;
        }

        let Some(line) = self.grid.visible_line_mut(row) else {
            return;
        };
        let start_x = start_x.min(line_width.saturating_sub(1));
        let end_x = end_x.min(line_width.saturating_sub(1));
        if start_x > end_x {
            return;
        }

        let start_x = line.owning_cell_x(start_x).unwrap_or(start_x);
        let end_x = line.owning_cell_x(end_x).unwrap_or(end_x);
        let mut touched = false;
        let mut x = start_x;
        while x <= end_x {
            let owner_x = line.owning_cell_x(x).unwrap_or(x);
            let width = line
                .cell(owner_x)
                .map(|cell| u32::from(cell.width().max(1)))
                .unwrap_or(1);

            for offset in 0..width {
                let cell_x = owner_x.saturating_add(offset);
                if let Some(cell) = line.cell_mut(cell_x) {
                    let mut flags = cell.flags();
                    flags.insert(GridCellFlags::SELECTED);
                    cell.set_flags(flags);
                    self.has_selected_cells = true;
                    touched = true;
                }
            }

            x = owner_x.saturating_add(width.max(1));
        }
        if touched {
            line.touch();
        }
    }

    /// Clears all visible selected-cell markers.
    pub fn clear_selected_cells(&mut self) {
        if !self.has_selected_cells {
            return;
        }

        let width = self.grid.sx();
        for row in 0..self.grid.sy() {
            let Some(line) = self.grid.visible_line_mut(row) else {
                continue;
            };
            let mut touched = false;
            for x in 0..width {
                let Some(cell) = line.cell_mut(x) else {
                    continue;
                };
                if !cell.flags().contains(GridCellFlags::SELECTED) {
                    continue;
                }
                let mut flags = cell.flags();
                flags.remove(GridCellFlags::SELECTED);
                cell.set_flags(flags);
                touched = true;
            }
            if touched {
                line.touch();
            }
        }
        self.has_selected_cells = false;
    }

    /// Overlays `style_input` onto all selected visible cells.
    pub fn overlay_style_on_selected(&mut self, style_input: &str) {
        if style_input.is_empty() {
            self.clear_selected_cells();
            return;
        }

        let width = self.grid.sx();
        for row in 0..self.grid.sy() {
            let Some(line) = self.grid.visible_line_mut(row) else {
                continue;
            };
            let mut touched = false;
            for x in 0..width {
                let Some(cell) = line.cell_mut(x) else {
                    continue;
                };
                if !cell.flags().contains(GridCellFlags::SELECTED) || cell.is_padding() {
                    continue;
                }

                // Selection composition, probed against the pinned tmux
                // 3.7b oracle (2026-07-11): the style's fg/bg replace the
                // cell's unless left at the default colour (the cell's
                // colour shows through a partial style such as bg=red); a
                // "complete" style that sets both fg and bg paints the
                // selection uniformly, dropping the cell's attributes (the
                // default fg=black,bg=yellow mode-style renders bold text
                // unbolded), while a partial style keeps them, unioned with
                // any attributes the style itself sets. "noattr" always
                // drops them.
                let base = StyleCell::default();
                let mut style = Style::with_cell(base);
                if style_parse(&mut style, &base, style_input).is_err() {
                    return;
                }

                let fg_set = !matches!(style.cell.fg, COLOUR_DEFAULT | COLOUR_TERMINAL);
                let bg_set = !matches!(style.cell.bg, COLOUR_DEFAULT | COLOUR_TERMINAL);
                let style_attr = style.cell.attr & !GridAttr::NOATTR;
                let attr = if style.cell.attr & GridAttr::NOATTR != 0 || (fg_set && bg_set) {
                    // Oracle probe 2026-07-11: a complete fg+bg style drops
                    // the cell's attributes but still applies its own
                    // (bg=red,fg=white,bold paints bold over plain text).
                    style_attr | (cell.attr() & GridAttr::CHARSET)
                } else {
                    style_attr | cell.attr()
                };
                cell.set_attr(attr);
                if fg_set {
                    cell.set_fg(style.cell.fg);
                }
                if bg_set {
                    cell.set_bg(style.cell.bg);
                }
                cell.set_us(style.cell.us);
                touched = true;
            }
            if touched {
                line.touch();
            }
        }
        self.clear_selected_cells();
    }
}
