//! Safe grid and scrollback storage for pane screen contents.

use rmux_proto::TerminalSize;
use std::collections::VecDeque;

use crate::hyperlinks::Hyperlinks;
use crate::input::{Colour, COLOUR_DEFAULT};
use crate::style::Style;

#[path = "grid/cell.rs"]
mod cell;
#[path = "grid/history_bytes.rs"]
mod history_bytes;
#[path = "grid/render.rs"]
mod render;

pub(crate) use cell::{GridCell, GridCellFlags, GridLine, GridLineFlags};
use render::{append_cell_text, append_grid_string_code, append_hyperlink};

const HISTORY_STAMP_REFRESH_LINES: u16 = 256;

/// Captured grid content rendered as logical lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct GridCapture {
    /// Captured lines ordered from oldest to newest.
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GridLogicalCursor {
    logical_start_y: usize,
    offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GridPhysicalCursor {
    pub absolute_y: usize,
    pub x: u32,
    pub pending_wrap: bool,
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
    /// Whether included empty cells should stop at tmux's allocation bucket.
    pub use_tmux_cell_capacity: bool,
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
            use_tmux_cell_capacity: false,
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

impl GridStringState {
    pub(crate) fn reset_to_default_line_style(
        &mut self,
        options: GridRenderOptions,
        hyperlinks: Option<&Hyperlinks>,
        output: &mut Vec<u8>,
    ) {
        if !options.with_sequences {
            return;
        }

        let default_cell = GridCell::blank_with_bg(COLOUR_DEFAULT);
        let mut rendered = String::new();
        let mut has_link = false;
        append_grid_string_code(
            &self.last_cell,
            &default_cell,
            &mut rendered,
            options.escape_sequences,
            hyperlinks,
            &mut has_link,
        );
        if has_link {
            append_hyperlink(&mut rendered, "", "", options.escape_sequences);
        }
        output.extend_from_slice(rendered.as_bytes());
        self.last_cell = default_cell;
    }
}

/// Absolute grid storage split into history and visible rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Grid {
    sx: u32,
    sy: u32,
    hlimit: usize,
    reflow_history_capacity: usize,
    hscrolled: usize,
    history_enabled: bool,
    history_stamp: i64,
    history_stamp_remaining: u16,
    history: VecDeque<GridLine>,
    visible: VecDeque<GridLine>,
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
            reflow_history_capacity: 0,
            hscrolled: 0,
            history_enabled: true,
            history_stamp: 0,
            history_stamp_remaining: 0,
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
        self.reflow_history_capacity = 0;
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
            self.reflow_history_capacity = self.reflow_history_capacity.saturating_sub(1);
            self.hscrolled = self.hscrolled.min(self.history.len());
            return true;
        }

        let visible_index = absolute_y.saturating_sub(self.history.len());
        if visible_index >= self.visible.len() {
            return false;
        }

        let _ = self.visible.remove(visible_index);
        self.visible.push_back(GridLine::new(self.sx));
        true
    }

    /// Drops all lines after the addressed absolute row and recomposes the viewport.
    pub(crate) fn truncate_after_absolute_line(&mut self, absolute_y: usize) -> bool {
        let total = self.history.len() + self.visible.len();
        if absolute_y >= total {
            return false;
        }

        let keep = absolute_y.saturating_add(1);
        let mut lines = self
            .history
            .iter()
            .chain(self.visible.iter())
            .take(keep)
            .cloned()
            .collect::<Vec<_>>();
        let visible_rows = self.sy as usize;
        while lines.len() < visible_rows {
            lines.push(GridLine::new(self.sx));
        }

        let visible_start = lines.len().saturating_sub(visible_rows);
        let mut visible = lines.split_off(visible_start);
        for line in &mut visible {
            line.resize_width_preserving_wrap(self.sx, COLOUR_DEFAULT);
        }
        self.history = compacted_history(lines);
        while self.history.len() > self.effective_history_capacity() {
            let _ = self.history.pop_front();
        }
        self.visible = visible.into();
        self.hscrolled = self.history.len();
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
        self.reflow_history_capacity = 0;
        self.hscrolled = 0;
    }

    /// Clears the visible grid.
    pub fn clear_visible(&mut self, bg: Colour) {
        for line in &mut self.visible {
            line.clear(bg);
        }
    }

    /// Moves used visible rows to scrollback before clearing the viewport.
    pub fn clear_visible_to_history(&mut self, bg: Colour) {
        if self.history_enabled {
            let last_used = self.visible.iter().rposition(|line| line.used_end() > 0);
            if let Some(last_used) = last_used {
                for index in 0..=last_used {
                    let line = self.visible[index].clone();
                    self.push_history(line);
                }
            }
        }
        self.clear_visible(bg);
    }

    /// Replaces the visible rows with a saved copy.
    pub fn replace_visible(&mut self, lines: Vec<GridLine>) {
        self.sy = lines.len() as u32;
        self.visible = lines.into();
        for line in &mut self.visible {
            line.resize_width_preserving_wrap(self.sx, COLOUR_DEFAULT);
        }
    }

    pub(crate) fn resize_visible_width_preserving_cursor(
        &mut self,
        sx: u32,
        bg: Colour,
        visible_y: u32,
        cursor_x: u32,
        pending_wrap: bool,
    ) -> GridPhysicalCursor {
        let source_width = self.sx.max(1);
        let target_width = sx.max(1);
        let visible = reflow_alternate_visible_lines(
            self.visible_lines(),
            target_width,
            bg,
            self.sy as usize,
        );
        let current_history = self.history.len();
        self.sx = target_width;
        self.visible = visible.into();

        // tmux keeps the alternate-screen cursor at its physical coordinate
        // across a width resize. Its grid may retain an x beyond the new edge;
        // RMUX models the same next-write behavior with a bounded edge cursor
        // and pending wrap.
        let physical_x = if pending_wrap { source_width } else { cursor_x };
        let (x, pending_wrap) = if physical_x >= target_width {
            (target_width.saturating_sub(1), true)
        } else {
            (physical_x, false)
        };
        GridPhysicalCursor {
            absolute_y: current_history.saturating_add(
                usize::try_from(visible_y.min(self.sy.saturating_sub(1))).unwrap_or(usize::MAX),
            ),
            x,
            pending_wrap,
        }
    }

    pub(crate) fn restore_visible_at_size(
        &mut self,
        source_size: TerminalSize,
        lines: Vec<GridLine>,
        bg: Colour,
    ) {
        self.sx = u32::from(source_size.cols.max(1));
        self.sy = u32::from(source_size.rows.max(1));
        self.visible = lines.into();
        while self.visible.len() > self.sy as usize {
            let _ = self.visible.pop_back();
        }
        while self.visible.len() < self.sy as usize {
            self.visible.push_back(GridLine::blank_with_bg(self.sx, bg));
        }
        for line in &mut self.visible {
            line.resize_width_preserving_wrap(self.sx, bg);
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
            .map(|line| line.render_with_options(self.sx as usize, options, state, hyperlinks))
    }

    pub fn append_rendered_absolute_line(
        &self,
        absolute_y: usize,
        options: GridRenderOptions,
        state: &mut GridStringState,
        hyperlinks: Option<&Hyperlinks>,
        output: &mut Vec<u8>,
    ) -> Option<()> {
        let line = self.absolute_line(absolute_y)?;
        if line.render_bytes_with_options(self.sx as usize, options, output) {
            return Some(());
        }
        let rendered = line.render_with_options(self.sx as usize, options, state, hyperlinks);
        output.extend_from_slice(rendered.as_bytes());
        Some(())
    }

    /// Renders one visible line after applying a pane default-style overlay to
    /// default cells only. This is used by live renderers to avoid cloning the
    /// full screen and scrollback when only the viewport is needed.
    #[must_use]
    pub fn render_visible_line_with_default_style(
        &self,
        row: usize,
        options: GridRenderOptions,
        state: &mut GridStringState,
        hyperlinks: Option<&Hyperlinks>,
        style: &Style,
    ) -> Option<String> {
        self.visible_line(u32::try_from(row).ok()?).map(|line| {
            line.render_with_default_style(self.sx as usize, options, state, hyperlinks, style)
        })
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
        self.visible.iter().cloned().collect()
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

        let upper = upper as usize;
        let lower = lower as usize;
        if upper == 0 && lower + 1 == self.visible.len() {
            let Some(mut removed) = self.visible.pop_front() else {
                return;
            };
            if to_history && self.history_enabled {
                self.push_history(removed);
                self.visible.push_back(GridLine::blank_with_bg(self.sx, bg));
            } else {
                removed.clear(bg);
                self.visible.push_back(removed);
            }
            return;
        }

        let removed_for_history = if to_history && self.history_enabled {
            let blank = GridLine::blank_with_bg(self.sx, bg);
            let visible = self.visible.make_contiguous();
            let removed = std::mem::replace(&mut visible[upper], blank);
            Some(removed)
        } else {
            None
        };
        if let Some(removed) = removed_for_history {
            self.push_history(removed);
        }
        let visible = self.visible.make_contiguous();
        visible[upper..=lower].rotate_left(1);
        let removed = &mut visible[lower];
        removed.clear(bg);
    }

    pub(crate) fn scroll_region_down(&mut self, upper: u32, lower: u32, bg: Colour) {
        if !self.valid_region(upper, lower) {
            return;
        }

        let upper = upper as usize;
        let lower = lower as usize;
        if upper == 0 && lower + 1 == self.visible.len() {
            let Some(mut removed) = self.visible.pop_back() else {
                return;
            };
            removed.clear(bg);
            self.visible.push_front(removed);
            return;
        }

        let visible = self.visible.make_contiguous();
        visible[upper..=lower].rotate_right(1);
        visible[upper].clear(bg);
    }

    pub(crate) fn logical_cursor(
        &self,
        visible_y: u32,
        cursor_x: u32,
        pending_wrap: bool,
    ) -> GridLogicalCursor {
        let total_lines = self.total_line_count();
        if total_lines == 0 {
            return GridLogicalCursor {
                logical_start_y: 0,
                offset: 0,
            };
        }

        let absolute_y = self
            .history
            .len()
            .saturating_add(visible_y as usize)
            .min(total_lines.saturating_sub(1));
        let logical_start_y = self.logical_start_y(absolute_y);
        let mut offset = 0_usize;
        for line_y in logical_start_y..absolute_y {
            if let Some(line) = self.absolute_line(line_y) {
                offset = offset.saturating_add(line.reflow_logical_width());
            }
        }
        let physical_cursor_column = if pending_wrap {
            self.sx as usize
        } else {
            cursor_x.min(self.sx.saturating_sub(1)) as usize
        };
        let cursor_column = self
            .absolute_line(absolute_y)
            .map_or(physical_cursor_column, |line| {
                line.reflow_logical_column(physical_cursor_column)
            });
        offset = offset.saturating_add(cursor_column);

        GridLogicalCursor {
            logical_start_y,
            offset,
        }
    }

    pub(crate) fn resize_width_remapping_cursor(
        &mut self,
        sx: u32,
        bg: Colour,
        cursor: GridLogicalCursor,
    ) -> GridPhysicalCursor {
        let sx = sx.max(1);
        if sx == self.sx {
            return self.locate_cursor_from_logical(cursor);
        }

        if self.can_resize_width_without_reflow(sx) {
            for line in &mut self.history {
                line.resize_width_preserving_wrap(sx, bg);
            }
            for line in &mut self.visible {
                line.resize_width_preserving_wrap(sx, bg);
            }
            self.sx = sx;
            return self.locate_cursor_from_logical(cursor);
        }

        let visible_rows = self.sy as usize;
        let lines = self
            .history
            .iter()
            .chain(self.visible.iter())
            .cloned()
            .collect::<Vec<_>>();
        let (mut reflowed, reflow_cursor) =
            reflow_wrapped_lines_remapping_cursor(lines, sx, bg, cursor);
        while reflowed.len() < visible_rows {
            reflowed.push(GridLine::blank_with_bg(sx, bg));
        }

        let default_history_rows = reflowed.len().saturating_sub(visible_rows);
        let mut mapped_cursor = reflow_cursor.unwrap_or(GridPhysicalCursor {
            absolute_y: reflowed.len().saturating_sub(1),
            x: sx.saturating_sub(1),
            pending_wrap: false,
        });
        let trailing_empty_rows = reflowed
            .iter()
            .rev()
            .take_while(|line| line.used_end() == 0 && !line.flags.contains(GridLineFlags::WRAPPED))
            .count();
        let cursor_shift = default_history_rows.saturating_sub(mapped_cursor.absolute_y);
        let history_rows =
            default_history_rows.saturating_sub(cursor_shift.min(trailing_empty_rows));
        if mapped_cursor.absolute_y < history_rows {
            mapped_cursor.absolute_y = history_rows;
            mapped_cursor.x = 0;
            mapped_cursor.pending_wrap = false;
        }
        let mut remaining = reflowed.split_off(history_rows);
        remaining.truncate(visible_rows);
        while remaining.len() < visible_rows {
            remaining.push(GridLine::blank_with_bg(sx, bg));
        }
        let mut visible = remaining;
        for line in &mut visible {
            line.resize_width_preserving_wrap(sx, bg);
        }
        self.history = compacted_history(reflowed);
        self.reflow_history_capacity = if self.history.len() > self.hlimit {
            self.history.len()
        } else {
            0
        };
        self.visible = visible.into();
        self.hscrolled = if self.hlimit == 0 {
            0
        } else {
            self.history.len()
        };
        self.sx = sx;
        mapped_cursor
    }

    fn can_resize_width_without_reflow(&self, sx: u32) -> bool {
        self.history.iter().chain(self.visible.iter()).all(|line| {
            !line.flags.contains(GridLineFlags::WRAPPED) && line.used_end() <= sx as usize
        })
    }

    fn effective_history_capacity(&self) -> usize {
        self.hlimit.max(self.reflow_history_capacity)
    }

    pub(crate) fn resize_height(&mut self, sy: u32, cursor_y: &mut u32, bg: Colour) {
        let sy = sy.max(1);
        let oldy = self.sy;

        if sy < oldy {
            let mut needed = oldy - sy;

            let available_bottom = oldy.saturating_sub(1).saturating_sub(*cursor_y);
            let remove_bottom = available_bottom.min(needed);
            for _ in 0..remove_bottom {
                let _ = self.visible.pop_back();
            }
            needed -= remove_bottom;

            if self.history_enabled {
                for _ in 0..needed {
                    let Some(line) = self.visible.pop_front() else {
                        break;
                    };
                    self.push_history_preserving_reflow_capacity(line);
                }
            } else {
                let remove_top = (*cursor_y).min(needed);
                for _ in 0..remove_top {
                    let _ = self.visible.pop_front();
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
                for mut line in restored.into_iter().rev() {
                    line.resize_width_preserving_wrap(self.sx, bg);
                    self.visible.push_front(line);
                }
                *cursor_y = cursor_y.saturating_add(pull).min(sy.saturating_sub(1));
                self.hscrolled -= pull as usize;
                needed -= pull;
            }

            for _ in 0..needed {
                self.visible.push_back(GridLine::blank_with_bg(self.sx, bg));
            }
        }

        self.sy = sy;
        while self.visible.len() > self.sy as usize {
            let _ = self.visible.pop_back();
        }
        while self.visible.len() < self.sy as usize {
            self.visible.push_back(GridLine::blank_with_bg(self.sx, bg));
        }
        for line in &mut self.visible {
            line.resize_width_preserving_wrap(self.sx, bg);
        }
        *cursor_y = (*cursor_y).min(self.sy.saturating_sub(1));
    }

    fn valid_region(&self, upper: u32, lower: u32) -> bool {
        upper < self.sy && lower < self.sy && upper <= lower
    }

    fn total_line_count(&self) -> usize {
        self.history.len().saturating_add(self.visible.len())
    }

    fn logical_start_y(&self, absolute_y: usize) -> usize {
        let mut start = absolute_y.min(self.total_line_count().saturating_sub(1));
        while start > 0
            && self
                .absolute_line(start - 1)
                .is_some_and(|line| line.flags.contains(GridLineFlags::WRAPPED))
        {
            start -= 1;
        }
        start
    }

    fn locate_cursor_from_logical(&self, cursor: GridLogicalCursor) -> GridPhysicalCursor {
        let total_lines = self.total_line_count();
        if total_lines == 0 {
            return GridPhysicalCursor {
                absolute_y: 0,
                x: 0,
                pending_wrap: false,
            };
        }

        let start = cursor.logical_start_y.min(total_lines.saturating_sub(1));
        let mut lines = Vec::new();
        for absolute_y in start..total_lines {
            let Some(line) = self.absolute_line(absolute_y) else {
                break;
            };
            lines.push(line.clone());
            if !line.flags.contains(GridLineFlags::WRAPPED) {
                break;
            }
        }

        if lines.len() == 1 {
            let used_end = lines[0].used_end();
            let at_new_edge =
                used_end > 0 && cursor.offset == used_end && used_end == self.sx as usize;
            return GridPhysicalCursor {
                absolute_y: start,
                x: if at_new_edge {
                    self.sx.saturating_sub(1)
                } else {
                    u32::try_from(cursor.offset)
                        .unwrap_or(u32::MAX)
                        .min(self.sx.saturating_sub(1))
                },
                pending_wrap: at_new_edge,
            };
        }

        let (_, relative) = reflow_wrapped_lines_remapping_cursor(
            lines,
            self.sx,
            COLOUR_DEFAULT,
            GridLogicalCursor {
                logical_start_y: 0,
                offset: cursor.offset,
            },
        );
        let relative = relative.unwrap_or(GridPhysicalCursor {
            absolute_y: 0,
            x: 0,
            pending_wrap: false,
        });
        GridPhysicalCursor {
            absolute_y: start.saturating_add(relative.absolute_y),
            x: relative.x,
            pending_wrap: relative.pending_wrap,
        }
    }

    fn push_history(&mut self, line: GridLine) {
        if self.hlimit == 0 {
            return;
        }
        // Normal terminal output consumes any unused resize-restoration
        // budget. Keep only the overflow rows that are still in history before
        // inserting the new row.
        if self.reflow_history_capacity > 0 {
            self.reflow_history_capacity = if self.history.len() > self.hlimit {
                self.history.len()
            } else {
                0
            };
        }
        self.push_history_with_effective_capacity(line);
    }

    fn push_history_preserving_reflow_capacity(&mut self, line: GridLine) {
        if self.hlimit == 0 {
            return;
        }
        // A height shrink may immediately return rows pulled by a preceding
        // growth, so it must retain the current reflow restoration budget.
        self.push_history_with_effective_capacity(line);
    }

    fn push_history_with_effective_capacity(&mut self, mut line: GridLine) {
        let history_capacity = self.effective_history_capacity();
        if history_capacity == 0 {
            return;
        }

        line.stamp_for_history_at(self.next_history_stamp());
        line.compact_for_history();
        if self.history.len() >= history_capacity {
            let _ = self.history.pop_front();
        }
        self.history.push_back(line);
        self.hscrolled = (self.hscrolled + 1).min(self.history.len());
    }

    fn next_history_stamp(&mut self) -> i64 {
        if self.history_stamp_remaining == 0 {
            self.history_stamp = cell::current_unix_timestamp();
            self.history_stamp_remaining = HISTORY_STAMP_REFRESH_LINES;
        }
        self.history_stamp_remaining = self.history_stamp_remaining.saturating_sub(1);
        self.history_stamp
    }
}

fn compacted_history(lines: Vec<GridLine>) -> VecDeque<GridLine> {
    lines
        .into_iter()
        .map(|mut line| {
            line.compact_for_history();
            line
        })
        .collect()
}

struct AlternateReflowGroup {
    source_rows: usize,
    rows: Vec<GridLine>,
}

fn reflow_alternate_visible_lines(
    lines: Vec<GridLine>,
    width: u32,
    bg: Colour,
    visible_rows: usize,
) -> Vec<GridLine> {
    let mut source_groups = Vec::new();
    let mut current_group = Vec::new();
    for line in lines {
        let wrapped = line.flags.contains(GridLineFlags::WRAPPED);
        current_group.push(line);
        if !wrapped {
            source_groups.push(std::mem::take(&mut current_group));
        }
    }
    if !current_group.is_empty() {
        source_groups.push(current_group);
    }

    let trailing_blank_groups = source_groups
        .iter()
        .rev()
        .take_while(|group| {
            group
                .iter()
                .all(|line| line.used_end() == 0 && line.flags == GridLineFlags::default())
        })
        .count();
    source_groups.truncate(source_groups.len().saturating_sub(trailing_blank_groups));

    let mut groups = source_groups
        .into_iter()
        .map(|group| {
            let source_rows = group.len();
            let (rows, _) = reflow_wrapped_lines_remapping_cursor(
                group,
                width,
                bg,
                GridLogicalCursor {
                    logical_start_y: usize::MAX,
                    offset: 0,
                },
            );
            AlternateReflowGroup { source_rows, rows }
        })
        .collect::<Vec<_>>();

    let mut allocations = groups
        .iter()
        .map(|group| group.source_rows.min(group.rows.len()))
        .collect::<Vec<_>>();
    let allocated = allocations.iter().copied().sum::<usize>();
    let mut remaining = visible_rows.saturating_sub(allocated);
    for (group, allocation) in groups.iter().zip(&mut allocations) {
        let extra = group.rows.len().saturating_sub(*allocation).min(remaining);
        *allocation = allocation.saturating_add(extra);
        remaining -= extra;
    }

    let mut visible = Vec::with_capacity(visible_rows);
    for (mut group, allocation) in groups.drain(..).zip(allocations) {
        let truncated = allocation < group.rows.len();
        group.rows.truncate(allocation);
        if truncated {
            if let Some(last) = group.rows.last_mut() {
                last.set_wrapped(false);
            }
        }
        visible.extend(group.rows);
    }
    visible.truncate(visible_rows);
    while visible.len() < visible_rows {
        visible.push(GridLine::blank_with_bg(width, bg));
    }
    visible
}

fn reflow_wrapped_lines_remapping_cursor(
    lines: Vec<GridLine>,
    width: u32,
    bg: Colour,
    cursor: GridLogicalCursor,
) -> (Vec<GridLine>, Option<GridPhysicalCursor>) {
    let mut output = Vec::new();
    let mut logical_cells = Vec::new();
    let mut logical_plain_text: Option<String> = None;
    let mut logical_flags = None;
    let mut logical_start_y = 0_usize;
    let mut mapped_cursor = None;

    for (absolute_y, line) in lines.into_iter().enumerate() {
        let wrapped = line.flags.contains(GridLineFlags::WRAPPED);
        if logical_flags.is_none() {
            logical_start_y = absolute_y;
            let mut flags = line.flags;
            flags.remove(GridLineFlags::WRAPPED);
            logical_flags = Some(flags);
            logical_plain_text = (bg == COLOUR_DEFAULT).then(String::new);
        }

        let end = if wrapped {
            self::line_width(&line)
        } else {
            line.used_end()
        };
        if let (Some(logical_text), Some(text)) = (logical_plain_text.as_mut(), line.plain_text()) {
            logical_text.extend(
                text.bytes()
                    .chain(std::iter::repeat(b' '))
                    .take(end)
                    .map(char::from),
            );
        } else {
            if let Some(text) = logical_plain_text.take() {
                extend_plain_ascii_cells(&mut logical_cells, text.bytes());
            }
            if let Some(text) = line.plain_text() {
                extend_plain_ascii_cells(
                    &mut logical_cells,
                    text.bytes().chain(std::iter::repeat(b' ')).take(end),
                );
            } else {
                logical_cells.extend(
                    line.cells
                        .iter()
                        .take(end)
                        .filter(|cell| !cell.is_padding() && !cell.is_reflow_gap())
                        .cloned(),
                );
            }
        }

        if !wrapped {
            let flags = logical_flags.take().unwrap_or_default();
            let cursor_offset =
                (logical_start_y == cursor.logical_start_y).then_some(cursor.offset);
            let (reflowed, relative_cursor) = if let Some(text) = logical_plain_text.take() {
                reflow_plain_ascii_line_remapping_cursor(&text, flags, width, bg, cursor_offset)
            } else {
                reflow_logical_line_remapping_cursor(
                    &logical_cells,
                    flags,
                    width,
                    bg,
                    cursor_offset,
                )
            };
            if let Some(mut physical) = relative_cursor {
                physical.absolute_y = physical.absolute_y.saturating_add(output.len());
                mapped_cursor = Some(physical);
            }
            output.extend(reflowed);
            logical_cells.clear();
        }
    }

    if logical_flags.is_some() || !logical_cells.is_empty() || logical_plain_text.is_some() {
        let flags = logical_flags.unwrap_or_default();
        let cursor_offset = (logical_start_y == cursor.logical_start_y).then_some(cursor.offset);
        let (reflowed, relative_cursor) = if let Some(text) = logical_plain_text {
            reflow_plain_ascii_line_remapping_cursor(&text, flags, width, bg, cursor_offset)
        } else {
            reflow_logical_line_remapping_cursor(&logical_cells, flags, width, bg, cursor_offset)
        };
        if let Some(mut physical) = relative_cursor {
            physical.absolute_y = physical.absolute_y.saturating_add(output.len());
            mapped_cursor = Some(physical);
        }
        output.extend(reflowed);
    }

    (output, mapped_cursor)
}

fn extend_plain_ascii_cells(cells: &mut Vec<GridCell>, bytes: impl IntoIterator<Item = u8>) {
    cells.extend(bytes.into_iter().map(GridCell::from_plain_ascii));
}

fn reflow_plain_ascii_line_remapping_cursor(
    text: &str,
    first_flags: GridLineFlags,
    width: u32,
    bg: Colour,
    cursor_offset: Option<usize>,
) -> (Vec<GridLine>, Option<GridPhysicalCursor>) {
    let width = width.max(1);
    if text.is_empty() || bg != COLOUR_DEFAULT {
        let mut line = GridLine::blank_with_bg(width, bg);
        line.flags = first_flags;
        let cursor = cursor_offset.map(|offset| GridPhysicalCursor {
            absolute_y: 0,
            x: u32::try_from(offset)
                .unwrap_or(u32::MAX)
                .min(width.saturating_sub(1)),
            pending_wrap: false,
        });
        return (vec![line], cursor);
    }

    let width_usize = width as usize;
    let mut output = Vec::with_capacity(text.len().div_ceil(width_usize));
    let mut start = 0;
    let mut flags = first_flags;
    while start < text.len() {
        let end = (start + width_usize).min(text.len());
        let mut line = GridLine::from_plain_ascii_text(width, flags, text[start..end].to_owned());
        if end < text.len() {
            line.set_wrapped(true);
        }
        output.push(line);
        flags = GridLineFlags::default();
        start = end;
    }

    let cursor = cursor_offset.map(|offset| {
        let content_len = text.len();
        if offset == content_len && content_len > 0 && content_len.is_multiple_of(width_usize) {
            return GridPhysicalCursor {
                absolute_y: content_len.div_ceil(width_usize).saturating_sub(1),
                x: width.saturating_sub(1),
                pending_wrap: true,
            };
        }
        if offset <= content_len {
            return GridPhysicalCursor {
                absolute_y: offset / width_usize,
                x: u32::try_from(offset % width_usize).unwrap_or(u32::MAX),
                pending_wrap: false,
            };
        }
        GridPhysicalCursor {
            absolute_y: output.len().saturating_sub(1),
            x: u32::try_from(offset)
                .unwrap_or(u32::MAX)
                .min(width.saturating_sub(1)),
            pending_wrap: false,
        }
    });
    (output, cursor)
}

fn reflow_logical_line_remapping_cursor(
    cells: &[GridCell],
    first_flags: GridLineFlags,
    width: u32,
    bg: Colour,
    cursor_offset: Option<usize>,
) -> (Vec<GridLine>, Option<GridPhysicalCursor>) {
    let width = width.max(1);
    if cells.is_empty() {
        let mut line = GridLine::blank_with_bg(width, bg);
        line.flags = first_flags;
        let cursor = cursor_offset.map(|offset| GridPhysicalCursor {
            absolute_y: 0,
            x: u32::try_from(offset)
                .unwrap_or(u32::MAX)
                .min(width.saturating_sub(1)),
            pending_wrap: false,
        });
        return (vec![line], cursor);
    }

    let mut output = Vec::new();
    let mut current = GridLine::blank_with_bg(width, bg);
    current.flags = first_flags;
    let mut x: u32 = 0;
    let mut logical_offset = 0_usize;
    let mut mapped_cursor = None;

    for cell in cells {
        let mut cell = cell.clone();
        let source_cell_width = u32::from(cell.width().max(1));
        let mut cell_width = source_cell_width;
        if cell_width > width {
            cell_width = 1;
            cell.set_width(1);
        }
        if x > 0 && x.saturating_add(cell_width) > width {
            current.mark_reflow_gap(x);
            current.set_wrapped(true);
            output.push(current);
            current = GridLine::blank_with_bg(width, bg);
            x = 0;
        }

        if mapped_cursor.is_none() {
            if let Some(cursor_offset) = cursor_offset {
                let cell_end = logical_offset.saturating_add(source_cell_width as usize);
                if cursor_offset >= logical_offset && cursor_offset < cell_end {
                    let relative = cursor_offset.saturating_sub(logical_offset);
                    let physical_offset =
                        u32::try_from(relative).unwrap_or(u32::MAX).min(cell_width);
                    if physical_offset == cell_width && x.saturating_add(cell_width) == width {
                        mapped_cursor = Some(GridPhysicalCursor {
                            absolute_y: output.len(),
                            x: width.saturating_sub(1),
                            pending_wrap: true,
                        });
                    } else {
                        mapped_cursor = Some(GridPhysicalCursor {
                            absolute_y: output.len(),
                            x: x.saturating_add(physical_offset)
                                .min(width.saturating_sub(1)),
                            pending_wrap: false,
                        });
                    }
                }
            }
        }

        if let Some(target) = current.cell_mut(x) {
            *target = cell.clone();
        }
        for offset in 1..cell_width {
            if let Some(padding_cell) = current.cell_mut(x + offset) {
                let mut padding = cell.clone();
                padding.set_text(" ".to_owned());
                padding.set_width(0);
                padding.set_flags(GridCellFlags::PADDING);
                *padding_cell = padding;
            }
        }
        current.touch();
        x += cell_width;
        logical_offset = logical_offset.saturating_add(source_cell_width as usize);
    }

    if mapped_cursor.is_none() {
        if let Some(cursor_offset) = cursor_offset {
            if cursor_offset == logical_offset {
                mapped_cursor = Some(if x == width {
                    GridPhysicalCursor {
                        absolute_y: output.len(),
                        x: width.saturating_sub(1),
                        pending_wrap: true,
                    }
                } else {
                    GridPhysicalCursor {
                        absolute_y: output.len(),
                        x,
                        pending_wrap: false,
                    }
                });
            } else if cursor_offset > logical_offset {
                mapped_cursor = Some(GridPhysicalCursor {
                    absolute_y: output.len(),
                    x: u32::try_from(cursor_offset)
                        .unwrap_or(u32::MAX)
                        .min(width.saturating_sub(1)),
                    pending_wrap: false,
                });
            }
        }
    }
    output.push(current);
    (output, mapped_cursor)
}

fn line_width(line: &GridLine) -> usize {
    line.width() as usize
}

#[cfg(test)]
#[path = "grid/tests.rs"]
mod tests;
