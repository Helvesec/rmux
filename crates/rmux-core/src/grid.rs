//! Safe grid and scrollback storage for pane screen contents.

use rmux_proto::TerminalSize;
use std::collections::VecDeque;

use crate::hyperlinks::Hyperlinks;
use crate::input::{Colour, COLOUR_DEFAULT};

#[path = "grid/cell.rs"]
mod cell;
#[path = "grid/history_bytes.rs"]
mod history_bytes;
#[path = "grid/render.rs"]
mod render;

pub(crate) use cell::{GridCell, GridCellFlags, GridLine, GridLineFlags};
use render::{append_cell_text, append_grid_string_code, append_hyperlink};

/// Captured grid content rendered as logical lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct GridCapture {
    /// Captured lines ordered from oldest to newest.
    pub lines: Vec<String>,
}

/// Rendering flags for tmux-style grid capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridRenderOptions {
    /// Whether wrapped rows should omit separating newlines.
    pub join_wrapped: bool,
    /// Whether to emit ANSI SGR and OSC sequences inline.
    pub with_sequences: bool,
    /// Whether control sequences should be octal-escaped.
    pub escape_sequences: bool,
    /// Whether trailing empty cells should be included.
    pub include_empty_cells: bool,
    /// Whether trailing spaces should be trimmed from the rendered line.
    pub trim_spaces: bool,
}

impl Default for GridRenderOptions {
    fn default() -> Self {
        Self {
            join_wrapped: false,
            with_sequences: false,
            escape_sequences: false,
            include_empty_cells: true,
            trim_spaces: true,
        }
    }
}

/// Per-capture ANSI state matching tmux's carried `lastgc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridStringState {
    last_cell: GridCell,
}

impl Default for GridStringState {
    fn default() -> Self {
        Self {
            last_cell: GridCell::blank_with_bg(COLOUR_DEFAULT),
        }
    }
}

/// Absolute grid storage split into history and visible rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Grid {
    sx: u32,
    sy: u32,
    hlimit: usize,
    hscrolled: usize,
    history_enabled: bool,
    history: VecDeque<GridLine>,
    visible: Vec<GridLine>,
}

impl Grid {
    /// Creates a new grid with the given geometry and history limit.
    #[must_use]
    pub fn new(size: TerminalSize, hlimit: usize) -> Self {
        let sx = u32::from(size.cols.max(1));
        let sy = u32::from(size.rows.max(1));
        Self {
            sx,
            sy,
            hlimit,
            hscrolled: 0,
            history_enabled: true,
            history: VecDeque::new(),
            visible: (0..sy).map(|_| GridLine::new(sx)).collect(),
        }
    }

    /// Returns the grid size.
    #[must_use]
    pub fn size(&self) -> TerminalSize {
        TerminalSize {
            cols: u16::try_from(self.sx).unwrap_or(u16::MAX),
            rows: u16::try_from(self.sy).unwrap_or(u16::MAX),
        }
    }

    /// Returns the visible width in columns.
    #[must_use]
    pub const fn sx(&self) -> u32 {
        self.sx
    }

    /// Returns the visible height in rows.
    #[must_use]
    pub const fn sy(&self) -> u32 {
        self.sy
    }

    /// Returns the history size in rows.
    #[must_use]
    pub fn hsize(&self) -> usize {
        self.history.len()
    }

    /// Returns the configured history limit.
    #[must_use]
    pub const fn hlimit(&self) -> usize {
        self.hlimit
    }

    /// Returns whether history collection is enabled.
    #[must_use]
    pub const fn history_enabled(&self) -> bool {
        self.history_enabled
    }

    /// Updates the history limit and evicts old rows if needed.
    pub fn set_hlimit(&mut self, hlimit: usize) {
        self.hlimit = hlimit;
        while self.history.len() > self.hlimit {
            let _ = self.history.pop_front();
        }
        self.hscrolled = self.hscrolled.min(self.history.len());
    }

    /// Enables or disables scrollback collection.
    pub fn set_history_enabled(&mut self, enabled: bool) {
        self.history_enabled = enabled;
    }

    /// Returns the number of history rows that can be pulled back by growth.
    #[allow(dead_code)]
    #[must_use]
    pub const fn hscrolled(&self) -> usize {
        self.hscrolled
    }

    /// Returns one visible line by row.
    #[must_use]
    pub fn visible_line(&self, y: u32) -> Option<&GridLine> {
        self.visible.get(y as usize)
    }

    pub(crate) fn visible_line_mut(&mut self, y: u32) -> Option<&mut GridLine> {
        self.visible.get_mut(y as usize)
    }

    /// Returns one absolute line where rows `0..hsize` are history and
    /// `hsize..hsize+sy` are the visible screen.
    #[allow(dead_code)]
    #[must_use]
    pub fn absolute_line(&self, absolute_y: usize) -> Option<&GridLine> {
        if absolute_y < self.history.len() {
            self.history.get(absolute_y)
        } else {
            self.visible.get(absolute_y - self.history.len())
        }
    }

    /// Removes one absolute line from history or the visible viewport.
    ///
    /// Visible removals keep the viewport height stable by pushing a blank row
    /// at the bottom.
    pub fn remove_absolute_line(&mut self, absolute_y: usize) -> bool {
        if absolute_y < self.history.len() {
            let _ = self.history.remove(absolute_y);
            self.hscrolled = self.hscrolled.min(self.history.len());
            return true;
        }

        let visible_index = absolute_y.saturating_sub(self.history.len());
        if visible_index >= self.visible.len() {
            return false;
        }

        let _ = self.visible.remove(visible_index);
        self.visible.push(GridLine::new(self.sx));
        true
    }

    /// Returns whether the absolute line is marked as wrapped.
    #[must_use]
    pub fn absolute_line_wrapped(&self, absolute_y: usize) -> Option<bool> {
        self.absolute_line(absolute_y)
            .map(|line| line.flags.contains(GridLineFlags::WRAPPED))
    }

    /// Clears every history row.
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.hscrolled = 0;
    }

    /// Clears the visible grid.
    pub fn clear_visible(&mut self, bg: Colour) {
        for line in &mut self.visible {
            line.clear(bg);
        }
    }

    /// Replaces the visible rows with a saved copy.
    pub fn replace_visible(&mut self, lines: Vec<GridLine>) {
        self.sy = lines.len() as u32;
        self.visible = lines;
        for line in &mut self.visible {
            line.resize_width_preserving_wrap(self.sx, COLOUR_DEFAULT);
        }
    }

    /// Captures the grid as rendered lines. Wrapped rows are optionally joined.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn capture(&self, join_wrapped: bool) -> GridCapture {
        let mut lines = Vec::new();
        let mut pending = String::new();

        for line in self.history.iter().chain(self.visible.iter()) {
            let rendered = line.render_text();
            if join_wrapped {
                pending.push_str(&rendered);
                if !line.flags.contains(GridLineFlags::WRAPPED) {
                    lines.push(std::mem::take(&mut pending));
                }
                continue;
            }

            lines.push(rendered);
        }

        if join_wrapped && !pending.is_empty() {
            lines.push(pending);
        }

        GridCapture { lines }
    }

    /// Renders one absolute line using tmux-style capture options.
    #[must_use]
    pub fn render_absolute_line(
        &self,
        absolute_y: usize,
        options: GridRenderOptions,
        state: &mut GridStringState,
        hyperlinks: Option<&Hyperlinks>,
    ) -> Option<String> {
        self.absolute_line(absolute_y)
            .map(|line| line.render_with_options(options, state, hyperlinks))
    }

    /// Returns the retained history size in bytes including newlines.
    #[must_use]
    pub fn history_byte_size(&self) -> usize {
        self.history
            .iter()
            .map(|line| line.render_text().len() + 1)
            .sum()
    }

    /// Captures only the visible rows.
    #[must_use]
    pub fn visible_lines(&self) -> Vec<GridLine> {
        self.visible.clone()
    }

    pub(crate) fn scroll_region_up(
        &mut self,
        upper: u32,
        lower: u32,
        bg: Colour,
        to_history: bool,
    ) {
        if !self.valid_region(upper, lower) {
            return;
        }

        let removed = self.visible.remove(upper as usize);
        if to_history && self.history_enabled {
            self.push_history(removed);
        }
        self.visible
            .insert(lower as usize, GridLine::blank_with_bg(self.sx, bg));
    }

    pub(crate) fn scroll_region_down(&mut self, upper: u32, lower: u32, bg: Colour) {
        if !self.valid_region(upper, lower) {
            return;
        }

        let _ = self.visible.remove(lower as usize);
        self.visible
            .insert(upper as usize, GridLine::blank_with_bg(self.sx, bg));
    }

    pub(crate) fn resize_width(&mut self, sx: u32, bg: Colour) {
        let sx = sx.max(1);
        if sx == self.sx {
            return;
        }

        let visible_rows = self.sy as usize;
        let lines = self
            .history
            .iter()
            .chain(self.visible.iter())
            .cloned()
            .collect::<Vec<_>>();
        let mut reflowed = reflow_wrapped_lines(lines, sx, bg);
        while reflowed.len() < visible_rows {
            reflowed.push(GridLine::blank_with_bg(sx, bg));
        }

        let history_rows = reflowed.len().saturating_sub(visible_rows);
        let visible = reflowed.split_off(history_rows);
        self.history = reflowed.into();
        while self.history.len() > self.hlimit {
            let _ = self.history.pop_front();
        }
        self.visible = visible;
        self.hscrolled = self.history.len();
        self.sx = sx;
    }

    pub(crate) fn resize_height(&mut self, sy: u32, cursor_y: &mut u32, bg: Colour) {
        let sy = sy.max(1);
        let oldy = self.sy;

        if sy < oldy {
            let mut needed = oldy - sy;

            let available_bottom = oldy.saturating_sub(1).saturating_sub(*cursor_y);
            let remove_bottom = available_bottom.min(needed);
            for _ in 0..remove_bottom {
                let _ = self.visible.pop();
            }
            needed -= remove_bottom;

            if self.history_enabled {
                for _ in 0..needed {
                    let Some(line) = self.visible.first().cloned() else {
                        break;
                    };
                    let _ = self.visible.remove(0);
                    self.push_history(line);
                }
            } else {
                let remove_top = (*cursor_y).min(needed);
                for _ in 0..remove_top {
                    if !self.visible.is_empty() {
                        let _ = self.visible.remove(0);
                    }
                }
                *cursor_y = cursor_y.saturating_sub(remove_top);
            }
        } else if sy > oldy {
            let mut needed = sy - oldy;
            let pull = self.hscrolled.min(needed as usize).min(self.history.len()) as u32;
            if self.history_enabled && pull > 0 {
                let mut restored = Vec::with_capacity(pull as usize);
                for _ in 0..pull {
                    if let Some(line) = self.history.pop_back() {
                        restored.push(line);
                    }
                }
                restored.reverse();
                for line in restored.into_iter().rev() {
                    self.visible.insert(0, line);
                }
                *cursor_y = cursor_y.saturating_add(pull).min(sy.saturating_sub(1));
                self.hscrolled -= pull as usize;
                needed -= pull;
            }

            for _ in 0..needed {
                self.visible.push(GridLine::blank_with_bg(self.sx, bg));
            }
        }

        self.sy = sy;
        self.visible
            .resize_with(self.sy as usize, || GridLine::blank_with_bg(self.sx, bg));
        *cursor_y = (*cursor_y).min(self.sy.saturating_sub(1));
    }

    fn valid_region(&self, upper: u32, lower: u32) -> bool {
        upper < self.sy && lower < self.sy && upper <= lower
    }

    fn push_history(&mut self, mut line: GridLine) {
        if self.hlimit == 0 {
            return;
        }

        line.touch();
        if self.history.len() == self.hlimit {
            let _ = self.history.pop_front();
        }
        self.history.push_back(line);
        self.hscrolled = (self.hscrolled + 1).min(self.history.len());
    }
}

fn reflow_wrapped_lines(lines: Vec<GridLine>, width: u32, bg: Colour) -> Vec<GridLine> {
    let mut output = Vec::new();
    let mut logical_cells = Vec::new();
    let mut logical_flags = None;

    for line in lines {
        let wrapped = line.flags.contains(GridLineFlags::WRAPPED);
        if logical_flags.is_none() {
            let mut flags = line.flags;
            flags.remove(GridLineFlags::WRAPPED);
            logical_flags = Some(flags);
        }

        let end = if wrapped {
            line.cells.len()
        } else {
            line.used_end()
        };
        logical_cells.extend(
            line.cells
                .iter()
                .take(end)
                .filter(|cell| !cell.is_padding())
                .cloned(),
        );

        if !wrapped {
            output.extend(reflow_logical_line(
                &logical_cells,
                logical_flags.take().unwrap_or_default(),
                width,
                bg,
            ));
            logical_cells.clear();
        }
    }

    if logical_flags.is_some() || !logical_cells.is_empty() {
        output.extend(reflow_logical_line(
            &logical_cells,
            logical_flags.unwrap_or_default(),
            width,
            bg,
        ));
    }

    output
}

fn reflow_logical_line(
    cells: &[GridCell],
    first_flags: GridLineFlags,
    width: u32,
    bg: Colour,
) -> Vec<GridLine> {
    if cells.is_empty() {
        let mut line = GridLine::blank_with_bg(width, bg);
        line.flags = first_flags;
        return vec![line];
    }

    let mut output = Vec::new();
    let mut current = GridLine::blank_with_bg(width, bg);
    current.flags = first_flags;
    let mut x: u32 = 0;

    for cell in cells {
        let mut cell = cell.clone();
        let mut cell_width = u32::from(cell.width().max(1));
        if cell_width > width {
            cell_width = 1;
            cell.set_width(1);
        }
        if x > 0 && x.saturating_add(cell_width) > width {
            current.set_wrapped(true);
            output.push(current);
            current = GridLine::blank_with_bg(width, bg);
            x = 0;
        }

        if let Some(target) = current.cells.get_mut(x as usize) {
            *target = cell.clone();
        }
        for offset in 1..cell_width {
            if let Some(padding_cell) = current.cells.get_mut((x + offset) as usize) {
                let mut padding = cell.clone();
                padding.set_text(" ".to_owned());
                padding.set_width(0);
                padding.set_flags(GridCellFlags::PADDING);
                *padding_cell = padding;
            }
        }
        current.touch();
        x += cell_width;
    }

    output.push(current);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::CellState;

    #[test]
    fn render_without_trimming_preserves_explicit_trailing_spaces_but_not_cleared_cells() {
        let mut line = GridLine::new(6);
        let state = CellState::default();
        for (x, ch) in "A  ".chars().enumerate() {
            *line.cell_mut(x as u32).expect("cell exists") =
                GridCell::from_state(ch, 1, &state, GridCellFlags::default());
        }

        let mut render_state = GridStringState::default();
        let rendered = line.render_with_options(
            GridRenderOptions {
                trim_spaces: false,
                include_empty_cells: false,
                ..GridRenderOptions::default()
            },
            &mut render_state,
            None,
        );
        assert_eq!(rendered, "A  ");

        let mut render_state = GridStringState::default();
        let trimmed = line.render_with_options(
            GridRenderOptions {
                trim_spaces: true,
                include_empty_cells: false,
                ..GridRenderOptions::default()
            },
            &mut render_state,
            None,
        );
        assert_eq!(trimmed, "A");
    }

    #[test]
    fn capture_join_wrapped_keeps_spaces_at_wrapped_boundaries() {
        let mut grid = Grid::new(TerminalSize { cols: 6, rows: 2 }, 0);
        let state = CellState::default();
        let first = grid.visible_line_mut(0).expect("line exists");
        for (x, ch) in "user ".chars().enumerate() {
            *first.cell_mut(x as u32).expect("cell exists") =
                GridCell::from_state(ch, 1, &state, GridCellFlags::default());
        }
        first.set_wrapped(true);
        let second = grid.visible_line_mut(1).expect("line exists");
        for (x, ch) in "root".chars().enumerate() {
            *second.cell_mut(x as u32).expect("cell exists") =
                GridCell::from_state(ch, 1, &state, GridCellFlags::default());
        }

        let mut render_state = GridStringState::default();
        let mut output = Vec::new();
        for absolute_y in 0..2 {
            let line = grid
                .render_absolute_line(
                    absolute_y,
                    GridRenderOptions {
                        join_wrapped: true,
                        trim_spaces: false,
                        include_empty_cells: false,
                        ..GridRenderOptions::default()
                    },
                    &mut render_state,
                    None,
                )
                .expect("line renders");
            output.extend_from_slice(line.as_bytes());
            if !grid.absolute_line_wrapped(absolute_y).unwrap_or(false) {
                output.push(b'\n');
            }
        }

        assert_eq!(String::from_utf8(output).expect("utf8"), "user root\n");
    }
}
