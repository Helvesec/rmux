//! Screen state and `ScreenWriter` implementation backed by [`Grid`].

use crate::grid::{Grid, GridCell, GridCellFlags, GridLine};
use crate::hyperlinks::Hyperlinks;
use crate::input::{mode, CellState, SavedState, ScreenWriter, COLOUR_DEFAULT};
use crate::terminal_passthrough::{TerminalPassthrough, MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES};
use crate::utf8::{combine_char as utf8_combine_char, CombineResult, Utf8Config};
use rmux_proto::TerminalSize;

#[path = "screen/acs.rs"]
mod acs;
#[path = "screen/capture.rs"]
mod capture;
#[path = "screen/cell_nav.rs"]
mod cell_nav;
#[path = "screen/history_bytes.rs"]
mod history_bytes;
#[path = "screen/selection.rs"]
mod selection;
#[path = "screen/style_overlay.rs"]
mod style_overlay;
#[path = "screen/view.rs"]
mod view;
#[path = "screen/writer.rs"]
mod writer;

pub use view::{ScreenCellView, ScreenLineView};

pub(crate) const MAX_TERMINAL_PASSTHROUGH_EVENTS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SavedGrid {
    grid: Grid,
    history_enabled: bool,
}

/// One pane screen, including scrollback, alternate-screen state, cursor
/// position, modes, tab stops, and hyperlink storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    grid: Grid,
    cursor_x: u32,
    cursor_y: u32,
    pending_wrap: bool,
    saved_cursor_x: Option<u32>,
    saved_cursor_y: Option<u32>,
    saved_cursor_pending_wrap: bool,
    saved_state: SavedState,
    saved_grid: Option<SavedGrid>,
    rupper: u32,
    rlower: u32,
    mode: u32,
    cursor_style: u32,
    title: String,
    window_name: String,
    path: String,
    title_stack: Vec<String>,
    tabs: Vec<bool>,
    hyperlinks: Hyperlinks,
    active_hyperlink: u32,
    bell_count: u64,
    terminal_passthrough: Vec<TerminalPassthrough>,
    dropped_terminal_passthrough_count: u64,
    utf8_config: Utf8Config,
}

impl Screen {
    /// Creates a new screen with the given geometry and history limit.
    #[must_use]
    pub fn new(size: TerminalSize, history_limit: usize) -> Self {
        let grid = Grid::new(size, history_limit);
        let mut screen = Self {
            grid,
            cursor_x: 0,
            cursor_y: 0,
            pending_wrap: false,
            saved_cursor_x: None,
            saved_cursor_y: None,
            saved_cursor_pending_wrap: false,
            saved_state: SavedState::default(),
            saved_grid: None,
            rupper: 0,
            rlower: u32::from(size.rows.max(1)).saturating_sub(1),
            mode: mode::MODE_CURSOR | mode::MODE_WRAP,
            cursor_style: 0,
            title: String::new(),
            window_name: String::new(),
            path: String::new(),
            title_stack: Vec::new(),
            tabs: Vec::new(),
            hyperlinks: Hyperlinks::new(),
            active_hyperlink: 0,
            bell_count: 0,
            terminal_passthrough: Vec::new(),
            dropped_terminal_passthrough_count: 0,
            utf8_config: Utf8Config::default(),
        };
        screen.reset_tabs();
        screen
    }

    /// Returns the current terminal mode flags.
    #[must_use]
    pub const fn mode(&self) -> u32 {
        self.mode
    }

    /// Returns the most recent DECSCUSR cursor style parameter.
    #[must_use]
    pub const fn cursor_style(&self) -> u32 {
        self.cursor_style
    }

    /// Returns the DECSTBM scroll region as `(top, bottom)` rows, 0-based and
    /// inclusive. A full-screen region is `(0, rows - 1)`.
    #[must_use]
    pub const fn scroll_region(&self) -> (u32, u32) {
        (self.rupper, self.rlower)
    }

    /// Returns the screen size.
    #[must_use]
    pub fn size(&self) -> TerminalSize {
        self.grid.size()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub(crate) fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Returns the current screen title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Sets the current screen title.
    pub fn set_title(&mut self, title: impl Into<String>) {
        self.title = title.into();
    }

    /// Returns the most recent OSC 7 path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns whether the alternate screen is active.
    #[must_use]
    pub fn is_alternate(&self) -> bool {
        self.saved_grid.is_some()
    }

    /// Returns the configured history limit.
    #[must_use]
    pub fn history_limit(&self) -> usize {
        self.grid.hlimit()
    }

    /// Returns the current history size in rows.
    #[must_use]
    pub fn history_size(&self) -> usize {
        self.grid.hsize()
    }

    /// Returns the current cursor position within the visible viewport.
    #[must_use]
    pub const fn cursor_position(&self) -> (u32, u32) {
        (self.cursor_x, self.cursor_y)
    }

    /// Returns the absolute cursor row including history.
    #[must_use]
    pub fn cursor_absolute_y(&self) -> usize {
        self.grid.hsize() + self.cursor_y as usize
    }

    /// Returns the total number of absolute lines retained by the screen.
    #[must_use]
    pub fn absolute_line_count(&self) -> usize {
        self.grid.hsize() + self.grid.sy() as usize
    }

    /// Deletes one visible line and scrolls the remaining viewport content up.
    ///
    /// This clears any pending wrap state because deleting a visible row
    /// invalidates the previous cursor edge condition.
    pub fn delete_visible_line(&mut self, y: u32) -> bool {
        if y >= self.grid.sy() {
            return false;
        }

        let cursor_x = self.cursor_x;
        let cursor_y = self.cursor_y;
        let rupper = self.rupper;
        let rlower = self.rlower;

        self.cursor_x = 0;
        self.cursor_y = y;
        self.pending_wrap = false;
        self.rupper = 0;
        self.rlower = self.grid.sy().saturating_sub(1);
        self.delete_line(1, COLOUR_DEFAULT);

        self.cursor_y = if cursor_y > y {
            cursor_y.saturating_sub(1)
        } else {
            cursor_y
        }
        .min(self.grid.sy().saturating_sub(1));
        self.cursor_x = cursor_x.min(self.grid.sx().saturating_sub(1));
        self.pending_wrap = false;
        self.rupper = rupper;
        self.rlower = rlower;
        true
    }

    /// Deletes one absolute line from history or the visible viewport.
    pub fn delete_absolute_line(&mut self, absolute_y: usize) -> bool {
        let history_size = self.grid.hsize();
        let visible_y = absolute_y.saturating_sub(history_size);
        let removed = self.grid.remove_absolute_line(absolute_y);
        if !removed {
            return false;
        }

        if absolute_y >= history_size {
            let visible_y = visible_y as u32;
            if visible_y < self.cursor_y {
                self.cursor_y = self.cursor_y.saturating_sub(1);
            }
        }
        self.pending_wrap = false;
        true
    }

    /// Returns the current retained history size in bytes.
    #[must_use]
    pub fn history_bytes(&self) -> usize {
        self.grid.history_byte_size()
    }

    /// Drains and returns the number of BEL notifications observed since the last drain.
    pub fn take_bell_count(&mut self) -> u64 {
        let bell_count = self.bell_count;
        self.bell_count = 0;
        bell_count
    }

    /// Drains terminal passthrough events observed since the last drain.
    pub fn take_terminal_passthrough(&mut self) -> Vec<TerminalPassthrough> {
        std::mem::take(&mut self.terminal_passthrough)
    }

    /// Drains the count of terminal passthrough events dropped by safety limits.
    pub fn take_terminal_passthrough_dropped_count(&mut self) -> u64 {
        let dropped = self.dropped_terminal_passthrough_count;
        self.dropped_terminal_passthrough_count = 0;
        dropped
    }

    fn push_terminal_passthrough(&mut self, passthrough: TerminalPassthrough) {
        if passthrough.payload().len() > MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES {
            self.dropped_terminal_passthrough_count =
                self.dropped_terminal_passthrough_count.saturating_add(1);
            return;
        }

        let overflow = self
            .terminal_passthrough
            .len()
            .saturating_add(1)
            .saturating_sub(MAX_TERMINAL_PASSTHROUGH_EVENTS);
        if overflow > 0 {
            self.terminal_passthrough.drain(..overflow);
            self.dropped_terminal_passthrough_count = self
                .dropped_terminal_passthrough_count
                .saturating_add(overflow as u64);
        }

        self.terminal_passthrough.push(passthrough);
    }

    /// Returns the stored OSC 8 URI for a hyperlink inner ID.
    #[must_use]
    pub fn hyperlink_uri(&self, inner_id: u32) -> Option<&str> {
        self.hyperlinks
            .get(inner_id)
            .map(|entry| entry.uri.as_str())
    }

    /// Updates the history limit.
    pub fn set_history_limit(&mut self, limit: usize) {
        self.grid.set_hlimit(limit);
    }

    /// Updates the tmux-style UTF-8 width and combining configuration.
    pub fn set_utf8_config(&mut self, utf8_config: Utf8Config) {
        self.utf8_config = utf8_config;
    }

    /// Resizes the screen and resets the scroll region.
    pub fn resize(&mut self, size: TerminalSize) {
        let cols = u32::from(size.cols.max(1));
        let rows = u32::from(size.rows.max(1));
        if cols != self.grid.sx() {
            self.grid.resize_width(cols, COLOUR_DEFAULT);
            self.reset_tabs();
        }
        if rows != self.grid.sy() {
            self.grid
                .resize_height(rows, &mut self.cursor_y, COLOUR_DEFAULT);
        }
        self.rupper = 0;
        self.rlower = rows.saturating_sub(1);
        self.cursor_x = self.cursor_x.min(self.max_cursor_x());
        self.pending_wrap &= self.cursor_x == self.max_cursor_x();
    }

    /// Clears history and optionally resets stored hyperlinks.
    pub fn clear_history_and_hyperlinks(&mut self, reset_hyperlinks: bool) {
        self.grid.clear_history();
        if reset_hyperlinks {
            self.hyperlinks.reset();
        }
    }

    fn reset_tabs(&mut self) {
        self.tabs = vec![false; self.grid.sx() as usize];
        for column in (8..self.grid.sx()).step_by(8) {
            self.tabs[column as usize] = true;
        }
    }

    fn max_cursor_x(&self) -> u32 {
        self.grid.sx().saturating_sub(1)
    }

    fn cursor_column(&self) -> u32 {
        self.cursor_x.min(self.max_cursor_x())
    }

    fn current_line_mut(&mut self) -> Option<&mut GridLine> {
        self.grid.visible_line_mut(self.cursor_y)
    }

    fn clear_pending_wrap(&mut self) {
        self.pending_wrap = false;
    }

    fn restore_cursor_position(&mut self, x: u32, y: u32, pending_wrap: bool) {
        self.cursor_x = x.min(self.max_cursor_x());
        self.cursor_y = y.min(self.grid.sy().saturating_sub(1));
        self.pending_wrap = pending_wrap
            && (self.mode & mode::MODE_WRAP) != 0
            && self.cursor_x == self.max_cursor_x();
    }

    fn apply_pending_wrap(&mut self) {
        if !self.pending_wrap || (self.mode & mode::MODE_WRAP) == 0 {
            self.pending_wrap = false;
            return;
        }

        if let Some(line) = self.current_line_mut() {
            line.set_wrapped(true);
        }
        self.pending_wrap = false;
        self.linefeed(false, COLOUR_DEFAULT);
        self.cursor_x = 0;
    }

    fn blank_cell(&self, bg: i32) -> GridCell {
        GridCell::blank_with_bg(bg)
    }

    fn overwrite_for_write(&mut self, x: u32, width: u32) {
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

    fn clear_line_range(&mut self, y: u32, start: u32, end_inclusive: u32, bg: i32) {
        let sx = self.grid.sx();
        let end = end_inclusive.min(sx.saturating_sub(1));
        let Some(line) = self.grid.visible_line_mut(y) else {
            return;
        };
        for x in start.min(sx)..=end {
            if let Some(cell) = line.cell_mut(x) {
                *cell = GridCell::blank_with_bg(bg);
            }
        }
        line.set_wrapped(false);
        line.touch();
    }

    fn clear_screen_region(&mut self, start_y: u32, end_y_inclusive: u32, bg: i32) {
        for y in start_y..=end_y_inclusive.min(self.grid.sy().saturating_sub(1)) {
            if let Some(line) = self.grid.visible_line_mut(y) {
                line.clear(bg);
            }
        }
    }

    fn write_char(&mut self, ch: char, cell: &CellState, acs: bool) {
        if self.grid.sx() == 0 || self.grid.sy() == 0 {
            return;
        }

        let ch = if acs { acs::translate_acs(ch) } else { ch };
        let width = u32::from(self.utf8_config.width(ch));
        if self.combine_char(ch) {
            return;
        }

        let automatic_wrap_continuation = self.pending_wrap && (self.mode & mode::MODE_WRAP) != 0;
        self.apply_pending_wrap();

        if (self.mode & mode::MODE_WRAP) != 0
            && self.cursor_x > self.grid.sx().saturating_sub(width)
        {
            if let Some(line) = self.current_line_mut() {
                line.set_wrapped(true);
            }
            self.linefeed(false, COLOUR_DEFAULT);
            self.cursor_x = 0;
        }

        if (self.mode & mode::MODE_WRAP) == 0
            && width > 1
            && (width > self.grid.sx() || self.cursor_x > self.grid.sx().saturating_sub(width))
        {
            return;
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

        if (self.mode & mode::MODE_WRAP) != 0 && x + width >= self.grid.sx() {
            self.cursor_x = self.max_cursor_x();
            self.pending_wrap = true;
        } else {
            self.cursor_x = x.saturating_add(width).min(self.max_cursor_x());
            self.pending_wrap = false;
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

    fn combine_char(&mut self, ch: char) -> bool {
        let mut x = self.cursor_column();
        if self.pending_wrap {
            x = self.max_cursor_x();
        } else if x == 0 {
            return matches!(
                utf8_combine_char(None, ch, &self.utf8_config),
                CombineResult::Discard
            );
        } else {
            x -= 1;
        }

        let Some(line) = self.grid.visible_line_mut(self.cursor_y) else {
            return matches!(
                utf8_combine_char(None, ch, &self.utf8_config),
                CombineResult::Discard
            );
        };
        let target_x = line.owning_cell_x(x).unwrap_or(x);
        let previous = line
            .cell(target_x)
            .map(|cell| (cell.text().to_owned(), cell.width()));
        let result = utf8_combine_char(
            previous
                .as_ref()
                .map(|(text, width)| (text.as_str(), *width)),
            ch,
            &self.utf8_config,
        );

        match result {
            CombineResult::Standalone { .. } => false,
            CombineResult::Discard => true,
            CombineResult::Combined { text, width } => {
                let previous_width = previous.as_ref().map_or(0, |(_, width)| *width);
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

    fn parse_hyperlink(data: &str) -> (Option<String>, String) {
        let (params, uri) = data.split_once(';').unwrap_or((data, ""));
        let mut internal_id = None;
        for part in params.split(':') {
            if let Some(value) = part.strip_prefix("id=") {
                internal_id = Some(value.to_owned());
            }
        }
        (internal_id, uri.to_owned())
    }
}

#[cfg(test)]
#[path = "screen/tests.rs"]
mod tests;
