use chrono::Local;

use crate::hyperlinks::Hyperlinks;
use crate::input::{CellState, Colour, COLOUR_DEFAULT};

use super::{
    append_cell_text, append_grid_string_code, append_hyperlink, GridRenderOptions, GridStringState,
};

/// Per-cell flags matching tmux `GRID_FLAG_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GridCellFlags(u8);

#[allow(dead_code)]
impl GridCellFlags {
    /// This cell is a padding cell belonging to a wide glyph.
    pub const PADDING: Self = Self(0x1);
    /// This cell was produced by a clear operation.
    pub const CLEARED: Self = Self(0x2);
    /// This cell represents tab-expanded whitespace.
    pub const TAB: Self = Self(0x4);
    /// This cell is part of an alternate representation.
    pub const EXTENDED: Self = Self(0x8);
    /// This cell is selected.
    pub const SELECTED: Self = Self(0x10);
    /// This cell should not inherit the palette.
    pub const NOPALETTE: Self = Self(0x20);

    /// Returns the raw bit value.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns whether all bits from `other` are present.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Adds the bits from `other`.
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Removes the bits from `other`.
    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

/// Per-line flags matching tmux `GRID_LINE_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GridLineFlags(u8);

#[allow(dead_code)]
impl GridLineFlags {
    /// The logical line continues on the following row.
    pub const WRAPPED: Self = Self(0x1);
    /// The line uses extended cell storage.
    pub const EXTENDED: Self = Self(0x2);
    /// The line is dead.
    pub const DEAD: Self = Self(0x4);
    /// The line starts a shell prompt block.
    pub const START_PROMPT: Self = Self(0x8);
    /// The line starts a shell output block.
    pub const START_OUTPUT: Self = Self(0x10);

    /// Returns the raw bit value.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns whether all bits from `other` are present.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Adds the bits from `other`.
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Removes the bits from `other`.
    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

/// One stored grid cell, including text and style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridCell {
    pub(super) text: String,
    width: u8,
    pub(super) flags: GridCellFlags,
    attr: u16,
    fg: Colour,
    bg: Colour,
    us: Colour,
    link: u32,
}

impl Default for GridCell {
    fn default() -> Self {
        Self::blank_with_bg(COLOUR_DEFAULT)
    }
}

#[allow(dead_code)]
impl GridCell {
    /// Creates a blank cell with the given background colour.
    #[must_use]
    pub fn blank_with_bg(bg: Colour) -> Self {
        Self {
            text: " ".to_owned(),
            width: 1,
            flags: GridCellFlags::CLEARED,
            attr: 0,
            fg: COLOUR_DEFAULT,
            bg,
            us: COLOUR_DEFAULT,
            link: 0,
        }
    }

    /// Creates a printable cell from the parser cell state.
    #[must_use]
    pub fn from_state(ch: char, width: u8, state: &CellState, flags: GridCellFlags) -> Self {
        let mut resolved_flags = flags;
        resolved_flags.remove(GridCellFlags::CLEARED);
        Self {
            text: ch.to_string(),
            width,
            flags: resolved_flags,
            attr: state.attr(),
            fg: state.fg(),
            bg: state.bg(),
            us: state.us(),
            link: state.link(),
        }
    }

    /// Returns the stored text payload.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the display width of the cell.
    #[must_use]
    pub const fn width(&self) -> u8 {
        self.width
    }

    /// Returns the cell flags.
    #[must_use]
    pub const fn flags(&self) -> GridCellFlags {
        self.flags
    }

    /// Returns whether this cell is a padding cell.
    #[must_use]
    pub const fn is_padding(&self) -> bool {
        self.flags.contains(GridCellFlags::PADDING)
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

    /// Returns the hyperlink inner ID.
    #[must_use]
    pub const fn link(&self) -> u32 {
        self.link
    }

    /// Returns whether the cell is visually blank.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.flags.contains(GridCellFlags::CLEARED)
            && !self.flags.contains(GridCellFlags::PADDING)
            && self.width == 1
            && self.text == " "
            && self.attr == 0
            && self.fg == COLOUR_DEFAULT
            && self.bg == COLOUR_DEFAULT
            && self.us == COLOUR_DEFAULT
            && self.link == 0
    }

    pub(crate) fn set_text(&mut self, text: String) {
        self.text = text;
    }

    pub(crate) fn set_width(&mut self, width: u8) {
        self.width = width;
    }

    pub(crate) fn set_flags(&mut self, flags: GridCellFlags) {
        self.flags = flags;
    }

    pub(crate) fn set_attr(&mut self, attr: u16) {
        self.attr = attr;
    }

    pub(crate) fn set_fg(&mut self, fg: Colour) {
        self.fg = fg;
    }

    pub(crate) fn set_bg(&mut self, bg: Colour) {
        self.bg = bg;
    }

    pub(crate) fn set_us(&mut self, us: Colour) {
        self.us = us;
    }
}

/// One absolute grid line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridLine {
    pub(super) cells: Vec<GridCell>,
    pub(super) flags: GridLineFlags,
    time: i64,
}

impl GridLine {
    /// Creates a blank line with `width` cells.
    #[must_use]
    pub fn new(width: u32) -> Self {
        Self {
            cells: vec![GridCell::default(); width as usize],
            flags: GridLineFlags::default(),
            time: current_time(),
        }
    }

    /// Creates a blank line with a specific background colour.
    #[must_use]
    pub fn blank_with_bg(width: u32, bg: Colour) -> Self {
        Self {
            cells: vec![GridCell::blank_with_bg(bg); width as usize],
            flags: GridLineFlags::default(),
            time: current_time(),
        }
    }

    /// Returns all cells in the line.
    #[must_use]
    pub fn cells(&self) -> &[GridCell] {
        &self.cells
    }

    /// Returns a mutable cell by column.
    pub(crate) fn cell_mut(&mut self, x: u32) -> Option<&mut GridCell> {
        self.cells.get_mut(x as usize)
    }

    /// Returns an immutable cell by column.
    #[must_use]
    pub fn cell(&self, x: u32) -> Option<&GridCell> {
        self.cells.get(x as usize)
    }

    /// Returns whether the cell at the given column is padding.
    #[must_use]
    pub fn is_padding_cell(&self, x: u32) -> bool {
        self.cell(x).is_some_and(GridCell::is_padding)
    }

    /// Returns the owning non-padding cell for the column, when present.
    #[must_use]
    pub fn owning_cell_x(&self, x: u32) -> Option<u32> {
        let cell = self.cell(x)?;
        if !cell.is_padding() {
            return Some(x);
        }

        let mut owner = x;
        while owner > 0 {
            owner -= 1;
            let cell = self.cell(owner)?;
            if !cell.is_padding() {
                let width = u32::from(cell.width().max(1));
                if owner.saturating_add(width) > x {
                    return Some(owner);
                }
                return None;
            }
        }
        None
    }

    /// Returns the line flags.
    #[must_use]
    pub const fn flags(&self) -> GridLineFlags {
        self.flags
    }

    /// Returns the last mutation timestamp.
    #[allow(dead_code)]
    #[must_use]
    pub const fn time(&self) -> i64 {
        self.time
    }

    pub(crate) fn touch(&mut self) {
        self.time = current_time();
    }

    pub(crate) fn set_wrapped(&mut self, wrapped: bool) {
        if wrapped {
            self.flags.insert(GridLineFlags::WRAPPED);
        } else {
            self.flags.remove(GridLineFlags::WRAPPED);
        }
    }

    pub(crate) fn clear(&mut self, bg: Colour) {
        self.cells.fill(GridCell::blank_with_bg(bg));
        self.flags = GridLineFlags::default();
        self.touch();
    }

    pub(crate) fn resize_width_preserving_wrap(&mut self, width: u32, bg: Colour) {
        self.resize_width_internal(width, bg, true);
    }

    fn resize_width_internal(&mut self, width: u32, bg: Colour, preserve_wrap: bool) {
        let width = width as usize;
        let resized = self.cells.len() != width;
        if resized {
            self.cells.resize(width, GridCell::blank_with_bg(bg));
        }
        let wrapped_before = self.flags.contains(GridLineFlags::WRAPPED);
        if !preserve_wrap {
            self.flags.remove(GridLineFlags::WRAPPED);
        }
        if resized || (wrapped_before && !preserve_wrap) {
            self.touch();
        }
    }

    pub(super) fn render_text(&self) -> String {
        let mut rendered = String::new();
        for cell in &self.cells {
            if cell.flags.contains(GridCellFlags::PADDING) {
                continue;
            }
            rendered.push_str(&cell.text);
        }
        while rendered.ends_with(' ') {
            rendered.pop();
        }
        rendered
    }

    pub(super) fn used_end(&self) -> usize {
        self.cells
            .iter()
            .rposition(|cell| !cell.is_blank())
            .map_or(0, |index| index + 1)
    }

    pub(super) fn tmux_cell_capacity(&self, line_width: usize) -> usize {
        let used_end = self.used_end();
        if used_end == 0 {
            return 0;
        }

        let quarter = (line_width / 4).max(1);
        let half = (line_width / 2).max(quarter);
        if used_end < quarter {
            quarter
        } else if used_end < half {
            half
        } else {
            line_width
        }
    }

    pub(super) fn extended_cell_count(&self) -> usize {
        self.cells[..self.used_end()]
            .iter()
            .filter(|cell| !cell.is_blank() && !cell.is_padding())
            .count()
    }

    pub(super) fn render_with_options(
        &self,
        options: GridRenderOptions,
        state: &mut GridStringState,
        hyperlinks: Option<&Hyperlinks>,
    ) -> String {
        let mut rendered = String::new();
        let mut has_link = false;
        let end = if options.include_empty_cells {
            self.cells.len()
        } else {
            self.used_end()
        };

        for cell in &self.cells[..end] {
            if cell.flags.contains(GridCellFlags::PADDING) {
                continue;
            }
            if options.with_sequences {
                append_grid_string_code(
                    &state.last_cell,
                    cell,
                    &mut rendered,
                    options.escape_sequences,
                    hyperlinks,
                    &mut has_link,
                );
                state.last_cell = cell.clone();
            }
            append_cell_text(cell, &mut rendered, options.escape_sequences);
        }

        if has_link {
            append_hyperlink(&mut rendered, "", "", options.escape_sequences);
        }
        if options.trim_spaces {
            while rendered.ends_with(' ') {
                rendered.pop();
            }
        }
        rendered
    }
}

fn current_time() -> i64 {
    Local::now().timestamp()
}
