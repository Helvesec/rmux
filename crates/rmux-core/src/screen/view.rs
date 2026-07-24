use crate::grid::{Grid, GridLine, GridLineFlags};
use crate::input::{Colour, COLOUR_DEFAULT};

use super::{SavedGrid, Screen};

/// Borrowed read-only view of one rendered screen cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenCellRef<'a> {
    text: &'a str,
    width: u8,
    padding: bool,
    attr: u16,
    fg: Colour,
    bg: Colour,
    us: Colour,
    link: u32,
}

impl<'a> ScreenCellRef<'a> {
    /// Returns the stored cell text.
    #[must_use]
    pub const fn text(&self) -> &'a str {
        self.text
    }

    /// Returns the display width of the cell.
    #[must_use]
    pub const fn width(&self) -> u8 {
        self.width
    }

    /// Returns whether this cell is padding for a wide glyph.
    #[must_use]
    pub const fn is_padding(&self) -> bool {
        self.padding
    }

    /// Returns the cell attributes.
    #[must_use]
    pub const fn attr(&self) -> u16 {
        self.attr
    }

    /// Returns the foreground colour.
    #[must_use]
    pub const fn fg(&self) -> Colour {
        self.fg
    }

    /// Returns the background colour.
    #[must_use]
    pub const fn bg(&self) -> Colour {
        self.bg
    }

    /// Returns the underline colour.
    #[must_use]
    pub const fn us(&self) -> Colour {
        self.us
    }

    /// Returns the hyperlink inner ID for the cell.
    #[must_use]
    pub const fn link(&self) -> u32 {
        self.link
    }
}

fn blank_cell_ref() -> ScreenCellRef<'static> {
    ScreenCellRef {
        text: " ",
        width: 1,
        padding: false,
        attr: 0,
        fg: COLOUR_DEFAULT,
        bg: COLOUR_DEFAULT,
        us: COLOUR_DEFAULT,
        link: 0,
    }
}

/// Read-only copy of one rendered screen cell for copy-mode consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenCellView {
    pub(super) text: String,
    pub(super) width: u8,
    pub(super) padding: bool,
    pub(super) attr: u16,
    pub(super) fg: crate::input::Colour,
    pub(super) bg: crate::input::Colour,
    pub(super) us: crate::input::Colour,
    pub(super) link: u32,
}

impl ScreenCellView {
    /// Returns the stored cell text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the display width of the cell.
    #[must_use]
    pub const fn width(&self) -> u8 {
        self.width
    }

    /// Returns whether the cell is padding for a wide glyph.
    #[must_use]
    pub const fn is_padding(&self) -> bool {
        self.padding
    }

    /// Returns the cell attributes.
    #[must_use]
    pub const fn attr(&self) -> u16 {
        self.attr
    }

    /// Returns the cell foreground colour.
    #[must_use]
    pub const fn fg(&self) -> crate::input::Colour {
        self.fg
    }

    /// Returns the cell background colour.
    #[must_use]
    pub const fn bg(&self) -> crate::input::Colour {
        self.bg
    }

    /// Returns the cell underline colour.
    #[must_use]
    pub const fn us(&self) -> crate::input::Colour {
        self.us
    }

    /// Returns the hyperlink inner ID for the cell.
    #[must_use]
    pub const fn link(&self) -> u32 {
        self.link
    }
}

/// Read-only copy of one absolute screen line for copy-mode consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenLineView {
    pub(super) cells: Vec<ScreenCellView>,
    width: u32,
    pub(super) wrapped: bool,
    pub(super) start_prompt: bool,
    pub(super) start_output: bool,
    pub(super) time: i64,
}

impl ScreenLineView {
    /// Returns the stored cells for the line.
    #[must_use]
    pub fn cells(&self) -> &[ScreenCellView] {
        &self.cells
    }

    /// Returns the terminal-width column span represented by this line.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns one cell by column.
    #[must_use]
    pub fn cell(&self, x: u32) -> Option<&ScreenCellView> {
        self.cells.get(x as usize)
    }

    /// Returns whether the line wraps onto the following row.
    #[must_use]
    pub const fn wrapped(&self) -> bool {
        self.wrapped
    }

    /// Returns whether the line starts a shell prompt block.
    #[must_use]
    pub const fn start_prompt(&self) -> bool {
        self.start_prompt
    }

    /// Returns whether the line starts a shell output block.
    #[must_use]
    pub const fn start_output(&self) -> bool {
        self.start_output
    }

    /// Returns the line timestamp.
    #[must_use]
    pub const fn time(&self) -> i64 {
        self.time
    }

    /// Resolves the owning non-padding cell for a column.
    #[must_use]
    pub fn owning_cell_x(&self, x: u32) -> Option<u32> {
        if x >= self.width {
            return None;
        }
        let Some(cell) = self.cell(x) else {
            return Some(x);
        };
        if !cell.is_padding() {
            return Some(x);
        }

        let mut owner = x;
        while owner > 0 {
            owner -= 1;
            let candidate = self.cell(owner)?;
            if !candidate.is_padding() {
                let width = u32::from(candidate.width().max(1));
                if owner.saturating_add(width) > x {
                    return Some(owner);
                }
                return None;
            }
        }
        None
    }
}

impl Screen {
    /// Reports whether recovery metadata fits the supplied bounds without
    /// cloning viewport or scrollback rows.
    #[must_use]
    pub fn recovery_metadata_fits(
        &self,
        max_string_bytes: usize,
        max_title_stack_bytes: usize,
        max_hyperlink_entry_bytes: usize,
        max_hyperlink_total_bytes: usize,
    ) -> bool {
        if self.title.len() > max_string_bytes
            || self.window_name.len() > max_string_bytes
            || self.path.len() > max_string_bytes
            || self
                .title_stack
                .iter()
                .any(|title| title.len() > max_string_bytes)
            || self
                .title_stack
                .iter()
                .try_fold(0_usize, |total, title| total.checked_add(title.len()))
                .is_none_or(|total| total > max_title_stack_bytes)
        {
            return false;
        }
        let (hyperlinks, complete) = self
            .hyperlinks
            .clone_bounded(max_hyperlink_entry_bytes, max_hyperlink_total_bytes);
        complete && (self.active_hyperlink == 0 || hyperlinks.get(self.active_hyperlink).is_some())
    }

    /// Clones only the visible viewport and bounded terminal metadata.
    ///
    /// Recovery callers validate geometry before invoking this method. Unlike
    /// [`Screen::clone`], this path never copies scrollback, saved alternate
    /// grids, passthrough payloads, or unbounded title/link strings.
    #[must_use]
    pub fn clone_recovery_viewport_bounded(
        &self,
        max_string_bytes: usize,
        max_title_stack_bytes: usize,
        max_hyperlink_entry_bytes: usize,
        max_hyperlink_total_bytes: usize,
    ) -> (Self, bool) {
        let (mut viewport, metadata_complete) = self.recovery_viewport_shell(
            max_string_bytes,
            max_title_stack_bytes,
            max_hyperlink_entry_bytes,
            max_hyperlink_total_bytes,
        );

        viewport.grid.replace_visible(self.grid.visible_lines());
        viewport.cursor_x = self.cursor_x.min(viewport.max_cursor_x());
        viewport.cursor_y = self.cursor_y.min(viewport.grid.sy().saturating_sub(1));
        viewport.pending_wrap = self.pending_wrap;
        viewport.saved_cursor_x = None;
        viewport.saved_cursor_y = None;
        viewport.saved_cursor_pending_wrap = false;
        viewport.rupper = self.rupper.min(viewport.grid.sy().saturating_sub(1));
        viewport.rlower = self.rlower.min(viewport.grid.sy().saturating_sub(1));
        viewport.tabs = self.tabs.clone();
        viewport.title_rename_enabled = self.title_rename_enabled;
        viewport.alternate_screen_enabled = self.alternate_screen_enabled;
        viewport.preserve_alternate_screen_cursor = self.preserve_alternate_screen_cursor;

        (viewport, metadata_complete)
    }

    /// Clones a bounded viewport over absolute rows for Web scroll/copy-mode
    /// recovery without first cloning the backing history.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn clone_recovery_viewport_at_bounded(
        &self,
        top_line: usize,
        cursor_x: u32,
        cursor_absolute_y: usize,
        max_string_bytes: usize,
        max_title_stack_bytes: usize,
        max_hyperlink_entry_bytes: usize,
        max_hyperlink_total_bytes: usize,
    ) -> (Self, bool) {
        let size = self.grid.size();
        let rows = usize::from(size.rows.max(1));
        let cols = u32::from(size.cols.max(1));
        let total_lines = self.absolute_line_count();
        let top_line = top_line.min(total_lines.saturating_sub(rows));
        let (mut viewport, metadata_complete) = self.recovery_viewport_shell(
            max_string_bytes,
            max_title_stack_bytes,
            max_hyperlink_entry_bytes,
            max_hyperlink_total_bytes,
        );
        let lines = (0..rows)
            .map(|offset| {
                self.grid
                    .absolute_line(top_line + offset)
                    .cloned()
                    .unwrap_or_else(|| GridLine::new(cols))
            })
            .collect();
        viewport.grid.replace_visible(lines);
        viewport.cursor_x = cursor_x.min(viewport.max_cursor_x());
        viewport.cursor_y = if (top_line..top_line + rows).contains(&cursor_absolute_y) {
            (cursor_absolute_y - top_line) as u32
        } else {
            0
        };
        viewport.pending_wrap = false;
        viewport.rupper = 0;
        viewport.rlower = u32::from(size.rows.max(1)).saturating_sub(1);
        viewport.reset_tabs();
        (viewport, metadata_complete)
    }

    #[allow(clippy::too_many_arguments)]
    fn recovery_viewport_shell(
        &self,
        max_string_bytes: usize,
        max_title_stack_bytes: usize,
        max_hyperlink_entry_bytes: usize,
        max_hyperlink_total_bytes: usize,
    ) -> (Self, bool) {
        let mut viewport = Self::new(self.grid.size(), 0);
        let mut metadata_complete = true;
        viewport.mode = self.mode;
        viewport.cursor_style = self.cursor_style;
        viewport.title = bounded_utf8_string(&self.title, max_string_bytes, &mut metadata_complete);
        viewport.window_name =
            bounded_utf8_string(&self.window_name, max_string_bytes, &mut metadata_complete);
        viewport.path = bounded_utf8_string(&self.path, max_string_bytes, &mut metadata_complete);
        viewport.title_stack = bounded_title_stack(
            &self.title_stack,
            max_string_bytes,
            max_title_stack_bytes,
            &mut metadata_complete,
        );
        let (hyperlinks, hyperlinks_complete) = self
            .hyperlinks
            .clone_bounded(max_hyperlink_entry_bytes, max_hyperlink_total_bytes);
        metadata_complete &= hyperlinks_complete;
        viewport.active_hyperlink = if hyperlinks.get(self.active_hyperlink).is_some() {
            self.active_hyperlink
        } else {
            metadata_complete &= self.active_hyperlink == 0;
            0
        };
        viewport.hyperlinks = hyperlinks;
        viewport.utf8_config = self.utf8_config.clone();
        viewport.saved_cursor_x = None;
        viewport.saved_cursor_y = None;
        viewport.saved_cursor_pending_wrap = false;
        viewport.saved_grid = self.saved_grid.as_ref().map(|saved| SavedGrid {
            grid: Grid::new(self.grid.size(), 0),
            history_enabled: saved.history_enabled,
        });
        viewport.title_rename_enabled = self.title_rename_enabled;
        viewport.alternate_screen_enabled = self.alternate_screen_enabled;
        viewport.preserve_alternate_screen_cursor = self.preserve_alternate_screen_cursor;
        (viewport, metadata_complete)
    }

    /// Visits borrowed cells for one visible row, padding to `cols` cells.
    ///
    /// Returns `false` when `row` is outside the visible viewport. Plain ASCII
    /// compact rows are visited directly from their compact text storage, so
    /// callers that only need the visible viewport can avoid the owned
    /// [`ScreenLineView`] allocation path.
    pub fn visit_visible_line_cells(
        &self,
        row: usize,
        cols: usize,
        mut visit: impl FnMut(ScreenCellRef<'_>),
    ) -> bool {
        let Some(line) = self
            .grid
            .visible_line(u32::try_from(row).unwrap_or(u32::MAX))
        else {
            return false;
        };
        if let Some(text) = line.plain_text() {
            let text_cols = text.len().min(cols);
            for col in 0..text_cols {
                visit(ScreenCellRef {
                    text: &text[col..col + 1],
                    width: 1,
                    padding: false,
                    attr: 0,
                    fg: COLOUR_DEFAULT,
                    bg: COLOUR_DEFAULT,
                    us: COLOUR_DEFAULT,
                    link: 0,
                });
            }
            for _ in text_cols..cols {
                visit(blank_cell_ref());
            }
            return true;
        }

        let mut emitted = 0_usize;
        for cell in line.cells().iter().take(cols) {
            visit(ScreenCellRef {
                text: cell.text(),
                width: cell.width(),
                padding: cell.is_padding(),
                attr: cell.attr(),
                fg: cell.fg(),
                bg: cell.bg(),
                us: cell.us(),
                link: cell.link(),
            });
            emitted += 1;
        }
        for _ in emitted..cols {
            visit(blank_cell_ref());
        }
        true
    }

    /// Returns a read-only copy of one absolute line.
    #[must_use]
    pub fn absolute_line_view(&self, absolute_y: usize) -> Option<ScreenLineView> {
        let line = self.grid.absolute_line(absolute_y)?;
        let width = u32::from(self.grid.size().cols.max(1));
        let cells = if let Some(text) = line.plain_text() {
            let mut cells = text
                .bytes()
                .map(|byte| ScreenCellView {
                    text: char::from(byte).to_string(),
                    width: 1,
                    padding: false,
                    attr: 0,
                    fg: crate::input::COLOUR_DEFAULT,
                    bg: crate::input::COLOUR_DEFAULT,
                    us: crate::input::COLOUR_DEFAULT,
                    link: 0,
                })
                .collect::<Vec<_>>();
            cells.resize_with(width as usize, || ScreenCellView {
                text: " ".to_owned(),
                width: 1,
                padding: false,
                attr: 0,
                fg: crate::input::COLOUR_DEFAULT,
                bg: crate::input::COLOUR_DEFAULT,
                us: crate::input::COLOUR_DEFAULT,
                link: 0,
            });
            cells
        } else {
            line.cells()
                .iter()
                .map(|cell| ScreenCellView {
                    text: cell.text().to_owned(),
                    width: cell.width(),
                    padding: cell.is_padding(),
                    attr: cell.attr(),
                    fg: cell.fg(),
                    bg: cell.bg(),
                    us: cell.us(),
                    link: cell.link(),
                })
                .collect()
        };
        Some(ScreenLineView {
            cells,
            width,
            wrapped: line.flags().contains(GridLineFlags::WRAPPED),
            start_prompt: line.flags().contains(GridLineFlags::START_PROMPT),
            start_output: line.flags().contains(GridLineFlags::START_OUTPUT),
            time: line.time(),
        })
    }

    /// Clones the screen as a standalone viewport over its absolute lines.
    #[must_use]
    pub fn clone_viewport(&self, top_line: usize, cursor_x: u32, cursor_absolute_y: usize) -> Self {
        let size = self.grid.size();
        let rows = usize::from(size.rows.max(1));
        let cols = u32::from(size.cols.max(1));
        let total_lines = self.absolute_line_count();
        let top_line = top_line.min(total_lines.saturating_sub(rows));
        let mut viewport = Self::new(size, 0);

        viewport.mode = self.mode;
        viewport.cursor_style = self.cursor_style;
        viewport.title = self.title.clone();
        viewport.window_name = self.window_name.clone();
        viewport.path = self.path.clone();
        viewport.title_stack = self.title_stack.clone();
        viewport.hyperlinks = self.hyperlinks.clone();
        viewport.active_hyperlink = self.active_hyperlink;
        viewport.bell_count = 0;
        viewport.utf8_config = self.utf8_config.clone();

        let lines = (0..rows)
            .map(|offset| {
                self.grid
                    .absolute_line(top_line + offset)
                    .cloned()
                    .unwrap_or_else(|| GridLine::new(cols))
            })
            .collect();
        viewport.grid.replace_visible(lines);

        viewport.cursor_x = cursor_x.min(viewport.max_cursor_x());
        viewport.cursor_y = if (top_line..top_line + rows).contains(&cursor_absolute_y) {
            (cursor_absolute_y - top_line) as u32
        } else {
            0
        };
        viewport.pending_wrap = false;
        viewport.saved_cursor_x = None;
        viewport.saved_cursor_y = None;
        viewport.saved_cursor_pending_wrap = false;
        viewport.saved_grid = None;
        viewport.rupper = 0;
        viewport.rlower = u32::from(size.rows.max(1)).saturating_sub(1);
        viewport.reset_tabs();
        viewport
    }
}

fn bounded_utf8_string(value: &str, max_bytes: usize, complete: &mut bool) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    *complete = false;
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn bounded_title_stack(
    titles: &[String],
    max_entry_bytes: usize,
    max_total_bytes: usize,
    complete: &mut bool,
) -> Vec<String> {
    let mut retained_reversed = Vec::new();
    let mut retained_bytes = 0_usize;
    for title in titles.iter().rev() {
        let title = bounded_utf8_string(title, max_entry_bytes, complete);
        let next = retained_bytes.saturating_add(title.len());
        if next > max_total_bytes {
            *complete = false;
            break;
        }
        retained_reversed.push(title);
        retained_bytes = next;
    }
    if retained_reversed.len() != titles.len() {
        *complete = false;
    }
    retained_reversed.reverse();
    retained_reversed
}
