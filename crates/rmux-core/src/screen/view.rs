use crate::grid::{GridLine, GridLineFlags};

use super::Screen;

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
