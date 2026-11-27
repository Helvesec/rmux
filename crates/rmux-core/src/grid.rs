//! Safe grid and scrollback storage for pane screen contents.

use rmux_proto::TerminalSize;
use std::collections::VecDeque;

use crate::hyperlinks::Hyperlinks;
use crate::input::{Colour, COLOUR_DEFAULT};
use crate::style::Style;

mod cell;
mod history;
mod history_bytes;
mod reflow;
mod render;
mod resize;
mod scroll;
mod shell_marks;

pub use cell::RenderedLineSpan;
pub(crate) use cell::{GridCell, GridCellFlags, GridLine, GridLineFlags};
use render::{append_cell_text, append_grid_string_code, append_hyperlink};

pub(crate) fn render_cell_state_ansi(
    state: &crate::input::CellState,
    hyperlinks: &Hyperlinks,
) -> Vec<u8> {
    let previous = GridCell::blank_with_bg(COLOUR_DEFAULT);
    let current = GridCell::from_state(' ', 1, state, GridCellFlags::default());
    let mut rendered = String::new();
    let mut has_link = false;
    append_grid_string_code(
        &previous,
        &current,
        &mut rendered,
        false,
        Some(hyperlinks),
        &mut has_link,
    );
    rendered.into_bytes()
}

use history::{compacted_history, history_line_content_bytes};

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
    history_content_bytes: usize,
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
            // A grid built at zero starts with collection off and a later
            // limit change never turns it back on, as tmux decides it once at
            // construction (grid.c:306-321) and `session.c:778-779` changes no
            // flag.
            history_enabled: hlimit != 0,
            history_stamp: 0,
            history_stamp_remaining: 0,
            history: VecDeque::new(),
            history_content_bytes: 0,
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
        // tmux grid_collect_history(all=1) removes at least one row when
        // already at the limit, even if this assignment repeats that limit.
        let remove = if !self.history.is_empty() && self.history.len() >= hlimit {
            (self.history.len() - hlimit).max(1)
        } else {
            0
        };
        for _ in 0..remove {
            let _ = self.pop_history_front();
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

    /// Breaks the soft-wrap boundary immediately before one visible row.
    ///
    /// The predecessor may be the final history row when the first viewport
    /// row is a continuation. Alternate-screen rows never continue main-screen
    /// history, which is retained in the same grid with history disabled.
    pub(crate) fn break_wrap_before_visible_line(&mut self, y: u32) {
        let previous = if y == 0 {
            if self.history_enabled {
                self.history.back_mut()
            } else {
                None
            }
        } else {
            self.visible.get_mut(y.saturating_sub(1) as usize)
        };
        if let Some(previous) = previous {
            previous.set_wrapped(false);
        }
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
            if let Some(line) = self.history.remove(absolute_y) {
                self.history_content_bytes = self
                    .history_content_bytes
                    .saturating_sub(history_line_content_bytes(&line));
            }
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
        self.recompute_history_content_bytes();
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

    /// Returns the columns a pre-wrapped wide glyph left unused at the end of
    /// the absolute line.
    #[must_use]
    pub fn absolute_line_trailing_reflow_gap(&self, absolute_y: usize) -> Option<u32> {
        self.absolute_line(absolute_y)
            .map(GridLine::trailing_reflow_gap)
    }

    /// Clears every history row.
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.history_content_bytes = 0;
        self.reflow_history_capacity = 0;
        self.hscrolled = 0;
    }

    /// Clears the visible grid.
    pub fn clear_visible(&mut self, bg: Colour) {
        for line in &mut self.visible {
            line.clear(bg);
        }
    }

    /// Clears the viewport in place, discarding the visible rows instead of
    /// moving them to scrollback.
    ///
    /// The wrap flag of the row preceding the viewport is cleared so the last
    /// scrollback row cannot stay joined to the blanked first visible row.
    pub fn clear_visible_without_history(&mut self, bg: Colour) {
        self.break_wrap_before_visible_line(0);
        self.clear_visible(bg);
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

    /// Captures the grid as rendered lines. Wrapped rows are optionally joined.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn capture(&self, join_wrapped: bool) -> GridCapture {
        let mut lines = Vec::new();
        let mut pending = String::new();

        for line in self.history.iter().chain(self.visible.iter()) {
            let wrapped = line.flags.contains(GridLineFlags::WRAPPED);
            let rendered = if join_wrapped {
                line.render_with_options(
                    self.sx as usize,
                    GridRenderOptions {
                        join_wrapped: true,
                        include_empty_cells: false,
                        trim_spaces: false,
                        ..GridRenderOptions::default()
                    },
                    &mut GridStringState::default(),
                    None,
                )
            } else {
                line.render_text()
            };
            if join_wrapped {
                pending.push_str(&rendered);
                if !wrapped {
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

    /// Renders one absolute line and reports the columns it paints.
    #[must_use]
    pub fn render_absolute_line_measured(
        &self,
        absolute_y: usize,
        options: GridRenderOptions,
        state: &mut GridStringState,
        hyperlinks: Option<&Hyperlinks>,
    ) -> Option<(String, RenderedLineSpan)> {
        self.absolute_line(absolute_y).map(|line| {
            line.render_with_options_measured(self.sx as usize, options, state, hyperlinks)
        })
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
    pub const fn history_byte_size(&self) -> usize {
        self.history_content_bytes
    }

    /// Captures only the visible rows.
    #[must_use]
    pub fn visible_lines(&self) -> Vec<GridLine> {
        self.visible.iter().cloned().collect()
    }

    pub(crate) fn clear_combined_sequence_discards(&mut self) {
        for line in self.history.iter_mut().chain(&mut self.visible) {
            line.clear_combined_sequence_discards();
        }
    }

    pub(crate) fn clone_recovery_projection(&self, max_history_bytes: usize) -> Self {
        let mut remaining = max_history_bytes;
        let mut start = self.history.len();
        while start > 0 {
            let group_start = self.logical_start_y(start - 1);
            let group_bytes = self
                .history
                .range(group_start..start)
                .map(GridLine::recovery_clone_bytes)
                .fold(0_usize, usize::saturating_add);
            if group_bytes > remaining {
                break;
            }
            remaining -= group_bytes;
            start = group_start;
        }

        let retained = self
            .history
            .range(start..)
            .cloned()
            .collect::<VecDeque<_>>();
        let history_content_bytes = retained.iter().map(history_line_content_bytes).sum();
        Self {
            sx: self.sx,
            sy: self.sy,
            hlimit: retained.len(),
            reflow_history_capacity: 0,
            hscrolled: retained.len(),
            history_enabled: self.history_enabled,
            history_stamp: self.history_stamp,
            history_stamp_remaining: self.history_stamp_remaining,
            history: retained,
            history_content_bytes,
            visible: self.visible.clone(),
        }
    }
}

#[cfg(test)]
#[path = "grid/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "grid/history_limit_tests.rs"]
mod history_limit_tests;
