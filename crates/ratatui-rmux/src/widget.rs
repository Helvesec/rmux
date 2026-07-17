//! Deterministic, sync ratatui widget for a captured RMUX pane state.
//!
//! `Widget::render` is a referentially transparent projection of the
//! supplied [`PaneState`] into a ratatui [`Buffer`]. The renderer never
//! awaits, never opens a socket, never spawns a task, and never reads
//! `Instant::now()` or any other ambient clock. Two consecutive renders
//! against the same state into the same buffer produce byte-identical
//! cells; this is what makes the widget safe to call from inside any
//! ratatui draw loop, including non-tokio hosts.
//!
//! The widget does *no* SDK calls. Drivers fold daemon state into the
//! [`PaneState`] before render time; the widget reads only that
//! captured value.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::Style;
use ratatui_core::widgets::Widget;

use crate::state::PaneState;
use crate::theme::{cell_style, glyph_symbol};

/// Sync ratatui widget that paints one [`PaneState`].
///
/// Uses a `&PaneState`, so the widget can be created inline in
/// a ratatui draw loop without cloning the captured grid.
#[derive(Debug, Clone, Copy)]
pub struct PaneWidget<'a> {
    state: &'a PaneState,
    base_style: Style,
}

impl<'a> PaneWidget<'a> {
    /// Creates a widget that renders `state` with the default style.
    #[must_use]
    pub const fn new(state: &'a PaneState) -> Self {
        Self {
            state,
            base_style: Style::new(),
        }
    }

    /// Returns a copy of this widget that pre-fills the render area
    /// with `style` before painting captured cells. Cells with their
    /// own colors override the base style cell-by-cell.
    #[must_use]
    pub const fn base_style(mut self, style: Style) -> Self {
        self.base_style = style;
        self
    }

    /// Returns the borrowed state. Provided for tests and host code
    /// that needs to read the same data the widget will project.
    #[must_use]
    pub const fn state(&self) -> &'a PaneState {
        self.state
    }

    fn paint(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let snapshot = &self.state.snapshot;
        if !snapshot.is_row_major_shape() {
            // Malformed snapshots paint a blank base area only — the
            // widget never panics on a bad input.
            fill_area(buf, area, self.base_style);
            return;
        }

        fill_area(buf, area, self.base_style);

        let max_rows = u16::min(snapshot.rows, area.height);
        let max_cols = u16::min(snapshot.cols, area.width);

        for row in 0..max_rows {
            let Some(row_cells) = snapshot.row_cells(row) else {
                continue;
            };
            let target_y = area.y.saturating_add(row);
            for col in 0..max_cols {
                let Some(cell) = row_cells.get(usize::from(col)) else {
                    continue;
                };
                let target_x = area.x.saturating_add(col);
                let available_columns =
                    (max_cols - col).min(buf.area.right().saturating_sub(target_x));
                let buffer_cell = buf.cell_mut((target_x, target_y));
                let Some(buffer_cell) = buffer_cell else {
                    continue;
                };
                let symbol = glyph_symbol(&cell.glyph);
                if symbol.is_empty() {
                    // Wide-glyph padding occupies a cell in the
                    // snapshot. Clear stale host content without
                    // introducing an extra visible glyph.
                    buffer_cell.set_symbol(" ");
                    buffer_cell.set_style(cell_style(cell));
                    continue;
                }
                if u16::from(cell.glyph.width) > available_columns {
                    // Ratatui treats a wide symbol as occupying cells beyond
                    // the one being written. Never let a clipped pane claim a
                    // neighboring widget's first cell in Buffer::diff.
                    buffer_cell.set_symbol(" ");
                    buffer_cell.set_style(cell_style(cell));
                    continue;
                }
                buffer_cell.set_symbol(symbol);
                buffer_cell.set_style(cell_style(cell));
            }
        }
    }
}

fn fill_area(buf: &mut Buffer, area: Rect, style: Style) {
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            let Some(cell) = buf.cell_mut((x, y)) else {
                continue;
            };
            cell.set_symbol(" ");
            cell.set_style(style);
        }
    }
}

impl<'a> Widget for PaneWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.paint(area, buf);
    }
}

impl<'a, 'b> Widget for &'b PaneWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.paint(area, buf);
    }
}
