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
use ratatui_core::style::{Modifier, Style};
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
            // Buffer styles merge modifiers. Only this render's base style
            // and captured cells may contribute text attributes, not the host
            // or a previous snapshot. Keep color inheritance unchanged.
            cell.modifier = Modifier::empty();
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

/// Pure whole-scene application-grid widget, keyed by stable pane identities.
/// Uses server-resolved raw spans and floating frames. Status lines, tiled border
/// decoration and scrollbars belong to the host; no layout is recomputed here.
#[derive(Debug, Clone, Copy)]
pub struct SceneWidget<'a> {
    scene: &'a rmux_sdk::SceneSnapshot,
    panes: &'a [(rmux_sdk::PaneId, PaneState)],
}
impl<'a> SceneWidget<'a> {
    /// Binds one resolved scene to separately captured pane grids.
    pub const fn new(
        scene: &'a rmux_sdk::SceneSnapshot,
        panes: &'a [(rmux_sdk::PaneId, PaneState)],
    ) -> Self {
        Self { scene, panes }
    }
    /// Projects the focused visible cursor; hidden/occluded cursors return None.
    pub fn cursor(&self, area: Rect) -> Option<(u16, u16)> {
        let id = self.scene.active_pane_id?;
        let pane = self
            .scene
            .panes
            .iter()
            .find(|p| p.id == id && p.focused && p.visible)?;
        let snapshot = &self.panes.iter().find(|(p, _)| *p == id)?.1.snapshot;
        let cursor = snapshot.cursor;
        if !cursor.visible {
            return None;
        }
        let span = pane.spans.iter().find(|s| {
            s.source_row == usize::from(cursor.row)
                && usize::from(cursor.col) >= s.source_col
                && usize::from(cursor.col) < s.source_col.saturating_add(usize::from(s.cols))
        })?;
        let x = span
            .x
            .checked_add(u16::try_from(usize::from(cursor.col) - span.source_col).ok()?)?;
        if x >= area.width.min(self.scene.cols) || span.y >= area.height.min(self.scene.rows) {
            return None;
        }
        Some((area.x.checked_add(x)?, area.y.checked_add(span.y)?))
    }
}
impl Widget for SceneWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        fill_area(buf, area, Style::new());
        let width = area.width.min(self.scene.cols);
        let height = area.height.min(self.scene.rows);
        for pane in self.scene.panes.iter().filter(|p| p.visible) {
            let Some((_, state)) = self.panes.iter().find(|(id, _)| *id == pane.id) else {
                continue;
            };
            if !state.snapshot.is_row_major_shape() {
                continue;
            }
            for span in &pane.spans {
                if span.x >= width || span.y >= height {
                    continue;
                }
                let Some(row) = u16::try_from(span.source_row)
                    .ok()
                    .and_then(|r| state.snapshot.row_cells(r))
                else {
                    continue;
                };
                let cols = span.cols.min(width - span.x);
                for offset in 0..cols {
                    let Some(source_col) = span.source_col.checked_add(usize::from(offset)) else {
                        continue;
                    };
                    let Some(cell) = row.get(source_col) else {
                        continue;
                    };
                    let Some(x) = area
                        .x
                        .checked_add(span.x)
                        .and_then(|x| x.checked_add(offset))
                    else {
                        continue;
                    };
                    let Some(y) = area.y.checked_add(span.y) else {
                        continue;
                    };
                    let available = (cols - offset).min(buf.area.right().saturating_sub(x));
                    let Some(target) = buf.cell_mut((x, y)) else {
                        continue;
                    };
                    let symbol = glyph_symbol(&cell.glyph);
                    target.set_symbol(
                        if symbol.is_empty() || u16::from(cell.glyph.width) > available {
                            " "
                        } else {
                            symbol
                        },
                    );
                    target.set_style(cell_style(cell));
                }
            }
        }
        for border in &self.scene.borders {
            if border.x < width && border.y < height {
                if let Some((x, y)) = area
                    .x
                    .checked_add(border.x)
                    .zip(area.y.checked_add(border.y))
                {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_symbol(&border.glyph.to_string());
                    }
                }
            }
        }
    }
}
