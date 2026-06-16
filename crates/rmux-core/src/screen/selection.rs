use crate::grid::GridCellFlags;
use crate::input::GridAttr;
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

                let base = StyleCell {
                    fg: cell.fg(),
                    bg: cell.bg(),
                    us: cell.us(),
                    attr: cell.attr(),
                };
                let mut style = Style::with_cell(base);
                if style_parse(&mut style, &base, style_input).is_err() {
                    return;
                }

                let attr = if style.cell.attr & GridAttr::NOATTR != 0 {
                    style.cell.attr & !GridAttr::NOATTR
                } else {
                    style.cell.attr
                };
                cell.set_attr(attr);
                cell.set_fg(style.cell.fg);
                cell.set_bg(style.cell.bg);
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
