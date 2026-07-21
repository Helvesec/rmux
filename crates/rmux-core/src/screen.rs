//! Screen state and `ScreenWriter` implementation backed by [`Grid`].

use crate::grid::{Grid, GridCell, GridCellFlags, GridLine, GridPhysicalCursor};
use crate::hyperlinks::Hyperlinks;
use crate::input::{mode, CellState, SavedState, ScreenWriter, COLOUR_DEFAULT};
use crate::terminal_passthrough::{TerminalPassthrough, MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES};
use crate::utf8::{combine_char as utf8_combine_char, CombineResult, Utf8Config};
use rmux_proto::TerminalSize;

#[path = "screen/acs.rs"]
mod acs;
#[path = "screen/capture.rs"]
mod capture;
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

pub use view::{ScreenCellRef, ScreenCellView, ScreenLineView};

pub(crate) const MAX_TERMINAL_PASSTHROUGH_EVENTS: usize = 256;
const TITLE_STACK_MAX: usize = 100;

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
    title_rename_enabled: bool,
    tabs: Vec<bool>,
    hyperlinks: Hyperlinks,
    active_hyperlink: u32,
    bell_count: u64,
    terminal_passthrough: Vec<TerminalPassthrough>,
    dropped_terminal_passthrough_count: u64,
    has_selected_cells: bool,
    utf8_config: Utf8Config,
    alternate_screen_enabled: bool,
    preserve_alternate_screen_cursor: bool,
}

impl Screen {
    pub(crate) fn render_cell_state_ansi(&self, state: &CellState) -> Vec<u8> {
        crate::grid::render_cell_state_ansi(state, &self.hyperlinks)
    }
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
            title_rename_enabled: true,
            tabs: Vec::new(),
            hyperlinks: Hyperlinks::new(),
            active_hyperlink: 0,
            bell_count: 0,
            terminal_passthrough: Vec::new(),
            dropped_terminal_passthrough_count: 0,
            has_selected_cells: false,
            utf8_config: Utf8Config::default(),
            alternate_screen_enabled: true,
            preserve_alternate_screen_cursor: false,
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

    pub(crate) const fn plain_output_forwarding_safe(&self) -> bool {
        let unsafe_modes = mode::MODE_INSERT | mode::MODE_CRLF | mode::MODE_SYNC;
        !self.pending_wrap
            && self.mode & mode::MODE_WRAP != 0
            && self.mode & unsafe_modes == 0
            && self.rupper == 0
            && self.rlower == self.grid.sy().saturating_sub(1)
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

    /// Enables or disables title changes requested by pane output.
    pub fn set_title_rename_enabled(&mut self, enabled: bool) {
        self.title_rename_enabled = enabled;
    }

    /// Enables or disables DEC alternate-screen entry for this screen.
    ///
    /// Exit sequences are still honored by the writer so disabling the option
    /// while a pane is already in alternate screen does not trap it there.
    pub fn set_alternate_screen_enabled(&mut self, enabled: bool) {
        self.alternate_screen_enabled = enabled;
    }

    pub(crate) fn set_preserve_alternate_screen_cursor(&mut self, enabled: bool) {
        self.preserve_alternate_screen_cursor = enabled;
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

    /// Returns the cursor saved by DEC alternate-screen entry, when present.
    #[must_use]
    pub fn alternate_saved_cursor_position(&self) -> Option<(u32, u32)> {
        self.saved_cursor_x.zip(self.saved_cursor_y)
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

    /// Trims all lines below the cursor and pulls history into the viewport.
    pub fn trim_below_cursor(&mut self) -> bool {
        let cursor_absolute_y = self.cursor_absolute_y();
        if !self.grid.truncate_after_absolute_line(cursor_absolute_y) {
            return false;
        }

        let history_size = self.grid.hsize();
        self.cursor_y = cursor_absolute_y
            .saturating_sub(history_size)
            .min(self.grid.sy().saturating_sub(1) as usize) as u32;
        self.cursor_x = self.cursor_x.min(self.max_cursor_x());
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
        self.clear_selected_cells();
        let cols = u32::from(size.cols.max(1));
        let rows = u32::from(size.rows.max(1));
        if cols != self.grid.sx() {
            let remapped = if self.is_alternate() {
                self.grid.resize_visible_width_preserving_cursor(
                    cols,
                    COLOUR_DEFAULT,
                    self.cursor_y,
                    self.cursor_x,
                    self.pending_wrap,
                )
            } else {
                let cursor =
                    self.grid
                        .logical_cursor(self.cursor_y, self.cursor_x, self.pending_wrap);
                self.grid
                    .resize_width_remapping_cursor(cols, COLOUR_DEFAULT, cursor)
            };
            self.apply_grid_cursor(remapped);
            self.reset_tabs();
        }
        if rows != self.grid.sy() {
            self.grid
                .resize_height(rows, &mut self.cursor_y, COLOUR_DEFAULT);
        }
        self.rupper = 0;
        self.rlower = rows.saturating_sub(1);
        self.cursor_x = self.cursor_x.min(self.max_cursor_x());
        self.pending_wrap = self.pending_wrap
            && (self.mode & mode::MODE_WRAP) != 0
            && self.cursor_x == self.max_cursor_x();
    }

    /// Clears history and optionally resets stored hyperlinks.
    pub fn clear_history_and_hyperlinks(&mut self, reset_hyperlinks: bool) {
        self.clear_selected_cells();
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

    fn logical_insert_column(&self) -> u32 {
        let x = self.cursor_column();
        let Some(line) = self.grid.visible_line(self.cursor_y) else {
            return x;
        };
        let Some(owner_x) = line.owning_cell_x(x).filter(|owner_x| *owner_x != x) else {
            return x;
        };
        let width = line
            .cell(owner_x)
            .map_or(1, |cell| u32::from(cell.width()).max(1));
        owner_x.saturating_add(width)
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

    fn apply_grid_cursor(&mut self, cursor: GridPhysicalCursor) {
        let history_size = self.grid.hsize();
        self.cursor_x = cursor.x.min(self.max_cursor_x());
        self.cursor_y = cursor
            .absolute_y
            .saturating_sub(history_size)
            .min(self.grid.sy().saturating_sub(1) as usize) as u32;
        self.pending_wrap = cursor.pending_wrap
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

    fn repair_wide_cells_on_line(line: &mut GridLine, sx: u32, bg: i32) {
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
        self.clear_selected_cells();
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
        Self::repair_wide_cells_on_line(line, sx, bg);
        line.set_wrapped(false);
        line.touch();
    }

    fn clear_screen_region(&mut self, start_y: u32, end_y_inclusive: u32, bg: i32) {
        self.clear_selected_cells();
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
        self.clear_selected_cells();

        let ch = if acs { acs::translate_acs(ch) } else { ch };
        let requested_width = u32::from(self.utf8_config.width(ch));
        if self.combine_char(ch) {
            return;
        }
        // A cell wider than the entire viewport cannot be represented with
        // its normal padding. Preserve the glyph as a one-column cell rather
        // than creating an out-of-bounds wide owner.
        let width = requested_width.clamp(1, self.grid.sx());

        let wrap_enabled = (self.mode & mode::MODE_WRAP) != 0;
        let mut automatic_wrap_continuation = self.pending_wrap && wrap_enabled;
        self.apply_pending_wrap();

        if (self.mode & mode::MODE_INSERT) != 0 {
            let insert_x = self.logical_insert_column();
            if insert_x >= self.grid.sx() {
                if !wrap_enabled {
                    return;
                }
                if let Some(line) = self.current_line_mut() {
                    line.set_wrapped(true);
                }
                self.linefeed(false, COLOUR_DEFAULT);
                self.cursor_x = 0;
                automatic_wrap_continuation = true;
            } else {
                self.cursor_x = insert_x;
            }
        }

        let write_would_cross_right_edge =
            width > self.grid.sx() || self.cursor_x > self.grid.sx().saturating_sub(width);
        if !wrap_enabled && width > 1 && write_would_cross_right_edge {
            return;
        }

        // tmux shifts the current row before an auto-wrapped wide glyph moves
        // to the next row; the no-wrap rejection above must leave it untouched.
        if (self.mode & mode::MODE_INSERT) != 0 {
            <Self as ScreenWriter>::insert_character(self, width, cell.bg());
        }

        if wrap_enabled && write_would_cross_right_edge {
            let gap_start = self.cursor_column();
            if let Some(line) = self.current_line_mut() {
                line.mark_unused_suffix_as_reflow_gap(gap_start);
                line.set_wrapped(true);
            }
            self.linefeed(false, COLOUR_DEFAULT);
            self.cursor_x = 0;
            automatic_wrap_continuation = true;
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

        if wrap_enabled && x + width >= self.grid.sx() {
            self.cursor_x = self.max_cursor_x();
            self.pending_wrap = true;
        } else {
            self.cursor_x = x.saturating_add(width).min(self.max_cursor_x());
            self.pending_wrap = false;
        }
    }

    fn write_plain_ascii_run(&mut self, mut bytes: &[u8], cell: &CellState, acs: bool) -> bool {
        if bytes.is_empty() {
            return true;
        }
        if acs
            || (self.mode & mode::MODE_INSERT) != 0
            || cell.attr() != 0
            || cell.fg() != COLOUR_DEFAULT
            || cell.bg() != COLOUR_DEFAULT
            || cell.us() != COLOUR_DEFAULT
            || cell.link() != 0
            || self.grid.sx() == 0
            || self.grid.sy() == 0
        {
            return false;
        }
        self.clear_selected_cells();

        while !bytes.is_empty() {
            let automatic_wrap_continuation =
                self.pending_wrap && (self.mode & mode::MODE_WRAP) != 0;
            self.apply_pending_wrap();
            if self.cursor_y >= self.grid.sy() {
                self.write_ascii_run_slow(bytes, cell, acs);
                return true;
            }

            let sx = self.grid.sx();
            let x = self.cursor_column();
            if x == 0 && !automatic_wrap_continuation {
                self.break_previous_wrapped_line();
            }

            if (self.mode & mode::MODE_WRAP) == 0 {
                let available = sx.saturating_sub(x) as usize;
                if bytes.len() > available {
                    self.write_ascii_run_slow(bytes, cell, acs);
                    return true;
                }
            }

            let writable = sx.saturating_sub(x) as usize;
            if writable == 0 {
                self.write_ascii_run_slow(bytes, cell, acs);
                return true;
            }
            let chunk_len = bytes.len().min(writable);
            let (chunk, rest) = bytes.split_at(chunk_len);
            let wrote_chunk = self
                .current_line_mut()
                .is_some_and(|line| line.write_plain_ascii_run(x, chunk));
            if !wrote_chunk {
                // Earlier chunks in this run may already have changed prior
                // rows. Consume only the untouched suffix through the general
                // writer so the caller never replays bytes already written.
                self.write_ascii_run_slow(bytes, cell, acs);
                return true;
            }

            if (self.mode & mode::MODE_WRAP) != 0 && x + chunk_len as u32 >= sx {
                self.cursor_x = self.max_cursor_x();
                self.pending_wrap = true;
            } else {
                self.cursor_x = x.saturating_add(chunk_len as u32).min(self.max_cursor_x());
                self.pending_wrap = false;
            }
            bytes = rest;
        }
        true
    }

    fn write_ascii_run_slow(&mut self, bytes: &[u8], cell: &CellState, acs: bool) {
        for &byte in bytes {
            self.write_char(char::from(byte), cell, acs);
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

        let Some((target_x, previous)) = self.grid.visible_line(self.cursor_y).map(|line| {
            let target_x = line.owning_cell_x(x).unwrap_or(x);
            let previous = line
                .cell(target_x)
                .map(|cell| (cell.text().to_owned(), cell.width()));
            (target_x, previous)
        }) else {
            return matches!(
                utf8_combine_char(None, ch, &self.utf8_config),
                CombineResult::Discard
            );
        };
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
                let available_width = self.grid.sx().saturating_sub(target_x).max(1);
                let width = width.min(u8::try_from(available_width).unwrap_or(u8::MAX));
                if width != previous_width {
                    // Width promotion may replace the owner of an adjacent
                    // wide cell. Clear that owner's displaced padding before
                    // installing the new owner/padding pair.
                    self.overwrite_for_write(target_x, u32::from(width));
                }
                let Some(line) = self.grid.visible_line_mut(self.cursor_y) else {
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
