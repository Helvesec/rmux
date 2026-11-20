use compact_str::CompactString;

mod extent;
mod grid_cell;
mod plain_text;
mod recovery;
mod reflow;
mod render;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::input::{Colour, COLOUR_DEFAULT};
use crate::utf8::CombinedSequenceDiscard;

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

/// Columns a rendered grid line paints when it is replayed into a terminal.
///
/// A replay places the following line either by autowrap or by an explicit
/// line break, and that choice depends on the columns the line occupies, not
/// on the byte length of its escape-laden rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderedLineSpan {
    columns: u32,
    leading_columns: u32,
}

impl RenderedLineSpan {
    /// Returns the columns painted by the whole line.
    #[must_use]
    pub const fn columns(self) -> u32 {
        self.columns
    }

    /// Returns the columns painted by the first cell, zero when the line
    /// paints nothing.
    #[must_use]
    pub const fn leading_columns(self) -> u32 {
        self.leading_columns
    }

    /// Builds a span for a line of single-column cells.
    const fn leading_one(columns: u32) -> Self {
        Self {
            columns,
            leading_columns: if columns == 0 { 0 } else { 1 },
        }
    }

    fn push(&mut self, width: u32) {
        if self.columns == 0 {
            self.leading_columns = width;
        }
        self.columns = self.columns.saturating_add(width);
    }

    fn pop_column(&mut self) {
        self.columns = self.columns.saturating_sub(1);
        if self.columns == 0 {
            self.leading_columns = 0;
        }
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
    combined_sequence_discard: CombinedSequenceDiscard,
    attr: u16,
    fg: Colour,
    bg: Colour,
    us: Colour,
    link: u32,
}

#[allow(dead_code)]
impl GridCell {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CellText {
    inline: [u8; 4],
    inline_len: u8,
    extended: Option<Box<CompactString>>,
}

/// One absolute grid line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridLine {
    pub(super) cells: Vec<GridCell>,
    plain_text: Option<CompactString>,
    width: u32,
    /// Exclusive end of compact ASCII data, including trailing spaces omitted
    /// from `plain_text`. The remaining physical suffix is still cleared.
    plain_text_data_end: u16,
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
            plain_text_data_end: 0,
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
            plain_text_data_end: 0,
            flags: GridLineFlags::default(),
            time: 0,
            revision: next_line_revision(),
        }
    }

    pub(super) fn from_plain_ascii_text(width: u32, flags: GridLineFlags, text: String) -> Self {
        debug_assert!(text.is_ascii());
        let plain_text_data_end = u16::try_from(text.len()).unwrap_or(u16::MAX);
        Self {
            cells: Vec::new(),
            plain_text: Some(CompactString::from(text)),
            width,
            plain_text_data_end,
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

    pub(crate) fn combined_sequence_discard(&self, x: u32) -> CombinedSequenceDiscard {
        if self.plain_text.is_some() {
            return CombinedSequenceDiscard::default();
        }
        self.cells
            .get(x as usize)
            .map_or_else(CombinedSequenceDiscard::default, |cell| {
                cell.combined_sequence_discard()
            })
    }

    pub(crate) fn clear_combined_sequence_discards(&mut self) {
        for cell in &mut self.cells {
            cell.clear_combined_sequence_discard();
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

    /// Returns an immutable cell by column.
    #[must_use]
    pub fn cell(&self, x: u32) -> Option<GridCell> {
        if x >= self.width {
            return None;
        }
        if let Some(text) = &self.plain_text {
            if let Some(byte) = text.as_bytes().get(x as usize).copied() {
                return Some(GridCell::from_plain_ascii(byte));
            }
            return Some(if x < u32::from(self.plain_text_data_end) {
                GridCell::from_plain_ascii(b' ')
            } else {
                GridCell::default()
            });
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

    pub(crate) fn mark_shell_line(&mut self, output: bool) {
        let flag = if output {
            GridLineFlags::START_OUTPUT
        } else {
            GridLineFlags::START_PROMPT
        };
        if !self.flags.contains(flag) {
            self.flags.insert(flag);
            self.touch();
        }
    }

    pub(crate) fn clear_shell_marks(&mut self) {
        self.flags.remove(GridLineFlags::START_PROMPT);
        self.flags.remove(GridLineFlags::START_OUTPUT);
    }

    pub(crate) fn clear(&mut self, bg: Colour) {
        if bg == COLOUR_DEFAULT {
            self.cells.clear();
            self.plain_text = Some(CompactString::new(""));
            self.plain_text_data_end = 0;
        } else {
            self.plain_text = None;
            self.plain_text_data_end = 0;
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

    fn resize_width_internal(&mut self, width: u32, bg: Colour, preserve_wrap: bool) {
        let width = width as usize;
        let old_width = self.width as usize;
        if self.plain_text.is_some() && bg == COLOUR_DEFAULT {
            if let Some(text) = &mut self.plain_text {
                if text.len() > width {
                    text.truncate(width);
                }
            }
            self.plain_text_data_end = self
                .plain_text_data_end
                .min(u16::try_from(width).unwrap_or(u16::MAX));
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
        }
        // The stored width follows every re-width, including the case the cell
        // vector is already the right length. A history line compacted to its
        // painted end carries fewer cells than its width, so re-widthing it to
        // exactly that count left the old width behind and later wrapped-cursor
        // arithmetic counted phantom columns.
        let rewidthed = old_width != width;
        self.width = u32::try_from(width).unwrap_or(u32::MAX);
        let wrapped_before = self.flags.contains(GridLineFlags::WRAPPED);
        if !preserve_wrap {
            self.flags.remove(GridLineFlags::WRAPPED);
        }
        if resized || rewidthed || (wrapped_before && !preserve_wrap) {
            self.touch();
        }
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
    use super::{render::plain_text_capture_end, CellText, GridCell, GridLine};
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

#[cfg(test)]
#[path = "erase_usage_tests.rs"]
mod erase_usage_tests;
