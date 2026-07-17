use compact_str::CompactString;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::hyperlinks::Hyperlinks;
use crate::input::{CellState, Colour, GridAttr, COLOUR_DEFAULT, COLOUR_NONE, COLOUR_TERMINAL};
use crate::style::Style;

use super::{
    append_cell_text, append_grid_string_code, append_hyperlink, GridRenderOptions, GridStringState,
};

/// Per-cell flags matching tmux `GRID_FLAG_*`, plus internal-only render metadata.
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
    /// Internal marker for an unused physical cell left before a wrapped wide glyph.
    pub const REFLOW_GAP: Self = Self(0x40);

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
    pub(super) text: CellText,
    width: u8,
    pub(super) flags: GridCellFlags,
    attr: u16,
    fg: Colour,
    bg: Colour,
    us: Colour,
    link: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CellText {
    inline: [u8; 4],
    inline_len: u8,
    extended: Option<Box<CompactString>>,
}

impl CellText {
    fn from_char(ch: char) -> Self {
        let mut inline = [0_u8; 4];
        let inline_len = ch.encode_utf8(&mut inline).len() as u8;
        Self {
            inline,
            inline_len,
            extended: None,
        }
    }

    fn from_ascii_byte(byte: u8) -> Self {
        debug_assert!(byte.is_ascii());
        Self {
            inline: [byte, 0, 0, 0],
            inline_len: 1,
            extended: None,
        }
    }

    fn new(text: String) -> Self {
        let mut chars = text.chars();
        if let Some(ch) = chars.next() {
            if chars.next().is_none() {
                return Self::from_char(ch);
            }
        }
        Self {
            inline: [0_u8; 4],
            inline_len: 0,
            extended: Some(Box::new(CompactString::new(text))),
        }
    }

    pub(super) fn as_str(&self) -> &str {
        if let Some(text) = &self.extended {
            return text.as_str();
        }
        std::str::from_utf8(&self.inline[..usize::from(self.inline_len)])
            .expect("inline cell text must be valid utf-8")
    }

    fn is_single_space(&self) -> bool {
        self.extended.is_none() && self.inline_len == 1 && self.inline[0] == b' '
    }
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
            text: CellText::from_char(' '),
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
            text: CellText::from_char(ch),
            width,
            flags: resolved_flags,
            attr: state.attr(),
            fg: state.fg(),
            bg: state.bg(),
            us: state.us(),
            link: state.link(),
        }
    }

    pub(super) fn from_plain_ascii(byte: u8) -> Self {
        Self {
            text: CellText::from_ascii_byte(byte),
            width: 1,
            flags: GridCellFlags::default(),
            attr: 0,
            fg: COLOUR_DEFAULT,
            bg: COLOUR_DEFAULT,
            us: COLOUR_DEFAULT,
            link: 0,
        }
    }

    fn set_plain_ascii(&mut self, byte: u8) {
        self.text = CellText::from_ascii_byte(byte);
        self.width = 1;
        self.flags = GridCellFlags::default();
        self.attr = 0;
        self.fg = COLOUR_DEFAULT;
        self.bg = COLOUR_DEFAULT;
        self.us = COLOUR_DEFAULT;
        self.link = 0;
    }

    /// Returns the stored text payload.
    #[must_use]
    pub fn text(&self) -> &str {
        self.text.as_str()
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

    /// Returns whether this blank cell exists only to place a following wide glyph.
    #[must_use]
    pub const fn is_reflow_gap(&self) -> bool {
        self.flags.contains(GridCellFlags::REFLOW_GAP)
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
            && self.text.is_single_space()
            && self.attr == 0
            && self.fg == COLOUR_DEFAULT
            && self.bg == COLOUR_DEFAULT
            && self.us == COLOUR_DEFAULT
            && self.link == 0
    }

    fn is_explicit_default_space(&self) -> bool {
        !self.flags.contains(GridCellFlags::CLEARED)
            && !self.flags.contains(GridCellFlags::PADDING)
            && self.width == 1
            && self.text.is_single_space()
            && self.has_default_style()
    }

    fn has_default_style(&self) -> bool {
        self.attr == 0
            && self.fg == COLOUR_DEFAULT
            && self.bg == COLOUR_DEFAULT
            && self.us == COLOUR_DEFAULT
            && self.link == 0
    }

    fn has_non_default_style(&self) -> bool {
        !self.has_default_style()
    }

    pub(crate) fn set_text(&mut self, text: String) {
        self.text = CellText::new(text);
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

    fn is_plain_default_ascii(&self) -> bool {
        self.width == 1
            && self.flags == GridCellFlags::default()
            && self.attr == 0
            && self.fg == COLOUR_DEFAULT
            && self.bg == COLOUR_DEFAULT
            && self.us == COLOUR_DEFAULT
            && self.link == 0
            && self.text.as_str().is_ascii()
            && self.text.as_str().len() == 1
    }
}

/// One absolute grid line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridLine {
    pub(super) cells: Vec<GridCell>,
    plain_text: Option<CompactString>,
    width: u32,
    pub(super) flags: GridLineFlags,
    time: i64,
    revision: u64,
}

impl GridLine {
    /// Creates a blank line with `width` cells.
    #[must_use]
    pub fn new(width: u32) -> Self {
        Self {
            cells: Vec::new(),
            plain_text: Some(CompactString::new("")),
            width,
            flags: GridLineFlags::default(),
            time: 0,
            revision: next_line_revision(),
        }
    }

    /// Creates a blank line with a specific background colour.
    #[must_use]
    pub fn blank_with_bg(width: u32, bg: Colour) -> Self {
        if bg == COLOUR_DEFAULT {
            return Self::new(width);
        }
        Self {
            cells: vec![GridCell::blank_with_bg(bg); width as usize],
            plain_text: None,
            width,
            flags: GridLineFlags::default(),
            time: 0,
            revision: next_line_revision(),
        }
    }

    pub(super) fn from_plain_ascii_text(width: u32, flags: GridLineFlags, text: String) -> Self {
        debug_assert!(text.is_ascii());
        Self {
            cells: Vec::new(),
            plain_text: Some(CompactString::from(text)),
            width,
            flags,
            time: 0,
            revision: next_line_revision(),
        }
    }

    /// Returns all cells in the line.
    #[must_use]
    pub fn cells(&self) -> &[GridCell] {
        &self.cells
    }

    #[must_use]
    pub(super) const fn width(&self) -> u32 {
        self.width
    }

    pub(crate) fn plain_text(&self) -> Option<&str> {
        self.plain_text.as_ref().map(CompactString::as_str)
    }

    /// Returns a mutable cell by column.
    pub(crate) fn cell_mut(&mut self, x: u32) -> Option<&mut GridCell> {
        self.materialize_for_cell_mutation();
        self.cells.get_mut(x as usize)
    }

    pub(crate) fn materialize_for_cell_mutation(&mut self) {
        if self.plain_text.is_some() {
            self.materialize_plain_text(self.width as usize);
        }
    }

    pub(crate) fn insert_cells(&mut self, start: u32, count: u32, blank: &GridCell) {
        self.materialize_for_cell_mutation();
        let reflow_gap_count = self
            .cells
            .iter()
            .filter(|cell| cell.is_reflow_gap())
            .count();
        let start = start as usize;
        if start >= self.cells.len() {
            return;
        }
        let count = (count as usize).min(self.cells.len() - start);
        if count == 0 {
            return;
        }
        self.cells[start..].rotate_right(count);
        for cell in &mut self.cells[start..start + count] {
            *cell = blank.clone();
        }
        // tmux 3.7b raises cellused to the row width when ICH moves cells,
        // but a clear at the final column leaves the existing gap untouched.
        if start + 1 == self.cells.len() {
            self.restore_trailing_reflow_gaps(reflow_gap_count);
        } else {
            self.materialize_reflow_gaps();
        }
    }

    pub(crate) fn delete_cells(&mut self, start: u32, count: u32, blank: &GridCell) {
        self.materialize_for_cell_mutation();
        let reflow_gap_count = self
            .cells
            .iter()
            .filter(|cell| cell.is_reflow_gap())
            .count();
        let start = start as usize;
        if start >= self.cells.len() {
            return;
        }
        let count = (count as usize).min(self.cells.len() - start);
        if count == 0 {
            return;
        }
        self.cells[start..].rotate_left(count);
        let fill_start = self.cells.len() - count;
        for cell in &mut self.cells[fill_start..] {
            *cell = blank.clone();
        }
        // DCH keeps cellused at max(old cellused, width - count), so only the
        // overlapping suffix remains a structural placement gap.
        self.restore_trailing_reflow_gaps(reflow_gap_count.min(count));
    }

    pub(crate) fn write_plain_ascii_run(&mut self, start: u32, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return true;
        }
        let start = start as usize;
        let Some(end) = start.checked_add(bytes.len()) else {
            return false;
        };
        if end > self.width as usize {
            return false;
        }
        if self.plain_text.is_some() && self.cells.is_empty() {
            self.update_plain_text_cache(start, bytes);
            self.touch();
            return true;
        }
        if end > self.cells.len() {
            return false;
        }
        if self.cells[start..end]
            .iter()
            .any(|cell| cell.width() != 1 || cell.is_padding())
        {
            return false;
        }
        if self.cells.get(end).is_some_and(GridCell::is_padding) {
            return false;
        }

        let can_cache_plain_text = self.can_cache_plain_ascii_run(start);
        for (cell, byte) in self.cells[start..end].iter_mut().zip(bytes) {
            cell.set_plain_ascii(*byte);
        }
        if can_cache_plain_text {
            self.update_plain_text_cache(start, bytes);
        } else {
            self.plain_text = None;
        }
        self.touch();
        true
    }

    /// Returns an immutable cell by column.
    #[must_use]
    pub fn cell(&self, x: u32) -> Option<GridCell> {
        if x >= self.width {
            return None;
        }
        if let Some(text) = &self.plain_text {
            let byte = text.as_bytes().get(x as usize).copied().unwrap_or(b' ');
            return Some(GridCell::from_plain_ascii(byte));
        }
        self.cells.get(x as usize).cloned()
    }

    /// Returns whether the cell at the given column is padding.
    #[must_use]
    pub fn is_padding_cell(&self, x: u32) -> bool {
        if self.plain_text.is_some() {
            return false;
        }
        self.cells.get(x as usize).is_some_and(GridCell::is_padding)
    }

    /// Returns the owning non-padding cell for the column, when present.
    #[must_use]
    pub fn owning_cell_x(&self, x: u32) -> Option<u32> {
        if self.plain_text.is_some() {
            return (x < self.width).then_some(x);
        }
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

    #[must_use]
    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn touch(&mut self) {
        let line_id = self.revision & LINE_REVISION_ID_MASK;
        let generation = (self.revision & LINE_REVISION_GENERATION_MASK).saturating_add(1);
        self.revision = line_id | generation.min(LINE_REVISION_GENERATION_MASK);
    }

    pub(crate) fn stamp_for_history_at(&mut self, timestamp: i64) {
        if self.time != 0 {
            return;
        }
        self.time = timestamp;
    }

    pub(crate) fn set_wrapped(&mut self, wrapped: bool) {
        let was_wrapped = self.flags.contains(GridLineFlags::WRAPPED);
        if wrapped {
            self.flags.insert(GridLineFlags::WRAPPED);
        } else {
            self.flags.remove(GridLineFlags::WRAPPED);
        }
        if was_wrapped != wrapped {
            self.touch();
        }
    }

    pub(crate) fn clear(&mut self, bg: Colour) {
        if bg == COLOUR_DEFAULT {
            self.cells.clear();
            self.plain_text = Some(CompactString::new(""));
        } else {
            self.plain_text = None;
            self.cells
                .resize(self.width as usize, GridCell::blank_with_bg(bg));
            self.cells.fill(GridCell::blank_with_bg(bg));
        }
        self.flags = GridLineFlags::default();
        self.touch();
    }

    pub(crate) fn resize_width_preserving_wrap(&mut self, width: u32, bg: Colour) {
        self.resize_width_internal(width, bg, true);
    }

    pub(crate) fn compact_for_history(&mut self) {
        if self.try_compact_plain_text() {
            return;
        }
        if self.flags.contains(GridLineFlags::WRAPPED) {
            return;
        }

        let used_end = self.used_end();
        if used_end < self.cells.len() {
            self.cells.truncate(used_end);
            self.cells.shrink_to_fit();
        }
    }

    fn resize_width_internal(&mut self, width: u32, bg: Colour, preserve_wrap: bool) {
        let width = width as usize;
        let old_width = self.width as usize;
        if self.plain_text.is_some() && bg == COLOUR_DEFAULT {
            if let Some(text) = &mut self.plain_text {
                if text.len() > width {
                    text.truncate(width);
                }
            }
            self.width = u32::try_from(width).unwrap_or(u32::MAX);
            let wrapped_before = self.flags.contains(GridLineFlags::WRAPPED);
            if !preserve_wrap {
                self.flags.remove(GridLineFlags::WRAPPED);
            }
            if old_width != width || (wrapped_before && !preserve_wrap) {
                self.touch();
            }
            return;
        }
        if self.plain_text.is_some() {
            self.materialize_plain_text(width);
        }
        let resized = self.cells.len() != width;
        if resized {
            self.cells.resize(width, GridCell::blank_with_bg(bg));
            self.width = u32::try_from(width).unwrap_or(u32::MAX);
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
        if let Some(text) = &self.plain_text {
            return text.trim_end_matches(' ').to_owned();
        }
        let mut rendered = String::new();
        for cell in &self.cells {
            if cell.flags.contains(GridCellFlags::PADDING) {
                continue;
            }
            rendered.push_str(cell.text.as_str());
        }
        while rendered.ends_with(' ') {
            rendered.pop();
        }
        rendered
    }

    pub(super) fn used_end(&self) -> usize {
        if let Some(text) = &self.plain_text {
            return text.len();
        }
        self.cells
            .iter()
            .rposition(|cell| !cell.is_blank())
            .map_or(0, |index| index + 1)
    }

    pub(super) fn mark_reflow_gap(&mut self, start: u32) {
        self.materialize_for_cell_mutation();
        for cell in self.cells.iter_mut().skip(start as usize) {
            debug_assert!(cell.is_blank());
            let mut flags = cell.flags();
            flags.insert(GridCellFlags::REFLOW_GAP);
            cell.set_flags(flags);
        }
    }

    /// Marks only the unused physical suffix skipped before a wide pre-wrap.
    pub(crate) fn mark_unused_suffix_as_reflow_gap(&mut self, start: u32) {
        if self.used_end() <= start as usize {
            self.mark_reflow_gap(start);
        }
    }

    fn restore_trailing_reflow_gaps(&mut self, count: usize) {
        self.materialize_reflow_gaps();
        for cell in self.cells.iter_mut().rev().take(count) {
            debug_assert!(cell.is_blank());
            let mut flags = cell.flags();
            flags.insert(GridCellFlags::REFLOW_GAP);
            cell.set_flags(flags);
        }
    }

    fn materialize_reflow_gaps(&mut self) {
        for cell in &mut self.cells {
            let mut flags = cell.flags();
            if flags.contains(GridCellFlags::REFLOW_GAP) {
                flags.remove(GridCellFlags::REFLOW_GAP);
                cell.set_flags(flags);
            }
        }
    }

    pub(super) fn reflow_logical_width(&self) -> usize {
        let width = self.width as usize;
        width.saturating_sub(
            self.cells
                .iter()
                .take(width)
                .filter(|cell| cell.is_reflow_gap())
                .count(),
        )
    }

    pub(super) fn reflow_logical_column(&self, physical_column: usize) -> usize {
        let physical_column = physical_column.min(self.width as usize);
        physical_column.saturating_sub(
            self.cells
                .iter()
                .take(physical_column)
                .filter(|cell| cell.is_reflow_gap())
                .count(),
        )
    }

    pub(super) fn tmux_cell_capacity(&self, line_width: usize) -> usize {
        let used_end = self.used_end();
        if used_end == 0 {
            return 0;
        }

        let bucket_used = self.extended_cell_count().max(1);
        let quarter = (line_width / 4).max(1);
        let half = (line_width / 2).max(quarter);
        if bucket_used < quarter {
            quarter
        } else if bucket_used < half {
            half
        } else {
            line_width
        }
    }

    fn tmux_capture_cell_end(&self, line_width: usize) -> usize {
        let used_end = self.used_end();
        if used_end == 0 {
            return 0;
        }
        if self.ends_with_default_spaces_after_styled_cells(used_end) {
            return used_end;
        }
        self.tmux_cell_capacity(line_width)
    }

    fn render_cell_end(&self, line_width: usize, options: GridRenderOptions) -> usize {
        let used_end = self.used_end();
        if options.trim_spaces {
            return if options.include_empty_cells {
                used_end.min(line_width)
            } else {
                used_end
            };
        }
        if options.include_empty_cells && options.use_tmux_cell_capacity {
            self.tmux_capture_cell_end(line_width)
        } else if options.include_empty_cells {
            line_width
        } else {
            used_end
        }
    }

    fn ends_with_default_spaces_after_styled_cells(&self, used_end: usize) -> bool {
        let mut first_trailing_space = used_end;
        while first_trailing_space > 0
            && self.cells[first_trailing_space - 1].is_explicit_default_space()
        {
            first_trailing_space -= 1;
        }

        first_trailing_space < used_end
            && self.cells[..first_trailing_space]
                .iter()
                .any(GridCell::has_non_default_style)
    }

    pub(super) fn extended_cell_count(&self) -> usize {
        if let Some(text) = &self.plain_text {
            return text.len();
        }
        self.cells[..self.used_end()]
            .iter()
            .filter(|cell| !cell.is_blank() && !cell.is_padding())
            .count()
    }

    pub(super) fn render_with_options(
        &self,
        line_width: usize,
        options: GridRenderOptions,
        state: &mut GridStringState,
        hyperlinks: Option<&Hyperlinks>,
    ) -> String {
        if let Some(text) = &self.plain_text {
            return render_plain_text_with_state(text, line_width, options, state, hyperlinks);
        }
        let mut rendered = String::new();
        let mut has_link = false;
        let end = self.render_cell_end(line_width, options);

        for cell in self.cells.iter().take(end) {
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
        if options.include_empty_cells && end > self.cells.len() {
            rendered.extend(std::iter::repeat_n(' ', end - self.cells.len()));
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

    pub(super) fn render_bytes_with_options(
        &self,
        line_width: usize,
        options: GridRenderOptions,
        output: &mut Vec<u8>,
    ) -> bool {
        if options.with_sequences || options.escape_sequences {
            return false;
        }
        if let Some(text) = &self.plain_text {
            append_plain_text_bytes_with_options(text, line_width, options, output);
            return true;
        }

        let start = output.len();
        let end = self.render_cell_end(line_width, options);

        for cell in self.cells.iter().take(end) {
            if cell.flags.contains(GridCellFlags::PADDING) {
                continue;
            }
            if cell.flags.contains(GridCellFlags::TAB) {
                output.push(b'\t');
                continue;
            }
            output.extend_from_slice(cell.text().as_bytes());
        }
        if options.include_empty_cells && end > self.cells.len() {
            output.extend(std::iter::repeat_n(b' ', end - self.cells.len()));
        }
        if options.trim_spaces {
            trim_trailing_ascii_spaces(output, start);
        }
        true
    }

    pub(super) fn render_with_default_style(
        &self,
        line_width: usize,
        options: GridRenderOptions,
        state: &mut GridStringState,
        hyperlinks: Option<&Hyperlinks>,
        style: &Style,
    ) -> String {
        let mut line = self.clone();
        line.overlay_default_style(style);
        line.render_with_options(line_width, options, state, hyperlinks)
    }

    fn overlay_default_style(&mut self, style: &Style) {
        if self.plain_text.is_some() {
            self.materialize_plain_text(self.width as usize);
        }
        let background = effective_background(style);
        for cell in &mut self.cells {
            if cell.is_padding() {
                continue;
            }
            if style_colour_is_set(style.cell.fg) && style_colour_is_unset(cell.fg()) {
                cell.set_fg(style.cell.fg);
            }
            if style_colour_is_set(background) && style_colour_is_unset(cell.bg()) {
                cell.set_bg(background);
            }
            if style_colour_is_set(style.cell.us) && style_colour_is_unset(cell.us()) {
                cell.set_us(style.cell.us);
            }
            if style.cell.attr != 0 && cell.attr() == 0 {
                cell.set_attr(style.cell.attr & !GridAttr::NOATTR);
            }
        }
    }

    fn try_compact_plain_text(&mut self) -> bool {
        if self.plain_text.is_some() {
            self.cells.clear();
            self.cells.shrink_to_fit();
            return true;
        }
        let Some(compacted) = self.compacted_plain_text_clone() else {
            return false;
        };
        *self = compacted;
        true
    }

    fn compacted_plain_text_clone(&self) -> Option<Self> {
        if let Some(text) = &self.plain_text {
            return Some(Self {
                cells: Vec::new(),
                plain_text: Some(text.clone()),
                width: self.width,
                flags: self.flags,
                time: self.time,
                revision: self.revision,
            });
        }
        if self.cells.iter().any(GridCell::is_reflow_gap) {
            return None;
        }
        let used_end = self.used_end();
        if used_end == 0 {
            return Some(Self {
                cells: Vec::new(),
                plain_text: Some(CompactString::new("")),
                width: self.width,
                flags: self.flags,
                time: self.time,
                revision: self.revision,
            });
        }

        let cells = self.cells.get(..used_end)?;
        if !cells.iter().all(GridCell::is_plain_default_ascii) {
            return None;
        }

        let mut text = String::with_capacity(used_end);
        for cell in cells {
            text.push_str(cell.text.as_str());
        }
        Some(Self {
            cells: Vec::new(),
            plain_text: Some(CompactString::from(text)),
            width: self.width,
            flags: self.flags,
            time: self.time,
            revision: self.revision,
        })
    }

    fn can_cache_plain_ascii_run(&self, start: usize) -> bool {
        if self.plain_text.is_some() {
            return true;
        }
        start == 0 && self.cells.iter().all(GridCell::is_blank)
    }

    fn update_plain_text_cache(&mut self, start: usize, bytes: &[u8]) {
        let text = self.plain_text.get_or_insert_with(CompactString::default);
        if text.len() < start {
            for _ in text.len()..start {
                text.push(' ');
            }
        }
        let end = start + bytes.len();
        let replacement = std::str::from_utf8(bytes).expect("plain ascii run must be utf-8");
        if text.len() < end {
            text.truncate(start);
            text.push_str(replacement);
        } else {
            text.replace_range(start..end, replacement);
        }
        while text.ends_with(' ') {
            text.pop();
        }
    }

    fn materialize_plain_text(&mut self, width: usize) {
        let Some(text) = self.plain_text.take() else {
            return;
        };
        self.cells = text.bytes().map(GridCell::from_plain_ascii).collect();
        if self.cells.len() > width {
            self.cells.truncate(width);
        }
        if self.cells.len() < width {
            self.cells.resize(width, GridCell::default());
        }
        self.width = u32::try_from(width).unwrap_or(u32::MAX);
    }
}

fn effective_background(style: &Style) -> Colour {
    if style_colour_is_set(style.cell.bg) {
        style.cell.bg
    } else {
        style.fill
    }
}

fn style_colour_is_set(colour: Colour) -> bool {
    !style_colour_is_unset(colour)
}

fn style_colour_is_unset(colour: Colour) -> bool {
    matches!(colour, COLOUR_DEFAULT | COLOUR_TERMINAL | COLOUR_NONE)
}

fn render_plain_text_with_state(
    text: &str,
    line_width: usize,
    options: GridRenderOptions,
    state: &mut GridStringState,
    hyperlinks: Option<&Hyperlinks>,
) -> String {
    let mut rendered = String::new();
    if options.with_sequences {
        let default_cell = GridCell::blank_with_bg(COLOUR_DEFAULT);
        let mut has_link = false;
        append_grid_string_code(
            &state.last_cell,
            &default_cell,
            &mut rendered,
            options.escape_sequences,
            hyperlinks,
            &mut has_link,
        );
        state.last_cell = default_cell;
        if has_link {
            append_hyperlink(&mut rendered, "", "", options.escape_sequences);
        }
    }
    append_plain_text_with_options(text, line_width, options, &mut rendered);
    rendered
}

fn append_plain_text_with_options(
    text: &str,
    line_width: usize,
    options: GridRenderOptions,
    output: &mut String,
) {
    let start = output.len();
    let end = plain_text_capture_end(text.len(), line_width, options);
    let copied_end = text.len().min(end);
    output.push_str(&text[..copied_end]);
    if text.len() < end {
        output.extend(std::iter::repeat_n(' ', end - text.len()));
    }
    if options.trim_spaces {
        trim_trailing_spaces(output, start);
    }
}

fn append_plain_text_bytes_with_options(
    text: &str,
    line_width: usize,
    options: GridRenderOptions,
    output: &mut Vec<u8>,
) {
    let start = output.len();
    let end = plain_text_capture_end(text.len(), line_width, options);
    let bytes = text.as_bytes();
    output.extend_from_slice(&bytes[..bytes.len().min(end)]);
    if bytes.len() < end {
        output.extend(std::iter::repeat_n(b' ', end - bytes.len()));
    }
    if options.trim_spaces {
        trim_trailing_ascii_spaces(output, start);
    }
}

fn plain_text_capture_end(used_end: usize, line_width: usize, options: GridRenderOptions) -> usize {
    if options.trim_spaces {
        return if options.include_empty_cells {
            used_end.min(line_width)
        } else {
            used_end
        };
    }
    if options.include_empty_cells && options.use_tmux_cell_capacity {
        if used_end == 0 {
            return 0;
        }
        let bucket_used = used_end.max(1);
        let quarter = (line_width / 4).max(1);
        let half = (line_width / 2).max(quarter);
        if bucket_used < quarter {
            quarter
        } else if bucket_used < half {
            half
        } else {
            line_width
        }
    } else if options.include_empty_cells {
        line_width
    } else {
        used_end
    }
}

fn trim_trailing_ascii_spaces(output: &mut Vec<u8>, floor: usize) {
    while output.len() > floor && output.last() == Some(&b' ') {
        output.pop();
    }
}

fn trim_trailing_spaces(output: &mut String, floor: usize) {
    while output.len() > floor && output.ends_with(' ') {
        output.pop();
    }
}

fn next_line_revision() -> u64 {
    static NEXT_LINE_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_LINE_ID.fetch_add(1, Ordering::Relaxed) << LINE_REVISION_GENERATION_BITS
}

pub(super) fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

const LINE_REVISION_GENERATION_BITS: u32 = 32;
const LINE_REVISION_GENERATION_MASK: u64 = (1_u64 << LINE_REVISION_GENERATION_BITS) - 1;
const LINE_REVISION_ID_MASK: u64 = !LINE_REVISION_GENERATION_MASK;

#[cfg(test)]
mod tests {
    use super::{plain_text_capture_end, CellText, GridCell, GridLine};
    use crate::grid::GridRenderOptions;

    #[test]
    fn grid_cell_layout_stays_compact() {
        assert!(
            std::mem::size_of::<CellText>() <= 16,
            "cell text regressed to {} bytes",
            std::mem::size_of::<CellText>()
        );
        assert!(
            std::mem::size_of::<GridCell>() <= 40,
            "grid cell regressed to {} bytes",
            std::mem::size_of::<GridCell>()
        );
        assert!(
            std::mem::size_of::<GridLine>() <= 72,
            "grid line header regressed to {} bytes",
            std::mem::size_of::<GridLine>()
        );
    }

    #[test]
    fn trimmed_plain_text_capture_skips_padding_work() {
        let options = GridRenderOptions {
            include_empty_cells: true,
            trim_spaces: true,
            ..GridRenderOptions::default()
        };

        assert_eq!(plain_text_capture_end(4, 120, options), 4);
    }

    #[test]
    fn untrimmed_plain_text_capture_keeps_padding_width() {
        let options = GridRenderOptions {
            include_empty_cells: true,
            trim_spaces: false,
            ..GridRenderOptions::default()
        };

        assert_eq!(plain_text_capture_end(4, 120, options), 120);
    }

    #[test]
    fn history_compaction_keeps_existing_plain_text_without_cells() {
        let mut line = GridLine::new(10);
        line.materialize_for_cell_mutation();
        assert!(!line.cells().is_empty());

        assert!(line.write_plain_ascii_run(0, b"abc"));
        assert_eq!(line.plain_text(), Some("abc"));
        assert!(!line.cells().is_empty());

        line.compact_for_history();

        assert_eq!(line.plain_text(), Some("abc"));
        assert!(line.cells().is_empty());
    }
}
