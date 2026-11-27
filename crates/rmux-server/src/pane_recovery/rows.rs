use rmux_core::RecoveryRow;

use super::{LINE_BREAK, RESET_RENDITION};

/// One row of the keyframe, with the columns its bytes paint on replay.
#[derive(Debug)]
pub(super) struct RenderedRow {
    pub(super) bytes: Vec<u8>,
    pub(super) wrapped: bool,
    pub(super) columns: u32,
    pub(super) leading_columns: u32,
    pub(super) trailing_reflow_gap: u32,
    extent_restore: Vec<u8>,
}

impl RenderedRow {
    pub(super) fn new(row: RecoveryRow) -> Self {
        let extent_restore = row.written_extent_restore_ansi();
        let written_columns = row.written_columns.unwrap_or(0);
        let mut rendered = Self {
            bytes: row.bytes,
            wrapped: row.wrapped,
            columns: row.span.columns(),
            leading_columns: row.span.leading_columns(),
            trailing_reflow_gap: row.trailing_reflow_gap,
            extent_restore,
        };
        // Capture may omit written trailing spaces. Paint those before adding
        // any padding whose sole purpose is to trigger the next autowrap.
        rendered.fill_to_width(written_columns);
        rendered
    }

    /// Returns whether the replaying terminal moves `next`'s first cell onto
    /// the following line on its own, reproducing this row exactly.
    ///
    /// That happens when the cell no longer fits in the columns this row
    /// leaves free, and those free columns are precisely the gap a wide glyph
    /// left behind when it pre-wrapped. Any other shortfall holds blanks that
    /// belong to this row, and the terminal would paint over them.
    fn wraps_into(&self, next: &Self, cols: u32) -> bool {
        self.columns.saturating_add(next.leading_columns) > cols
            && self.columns.saturating_add(self.trailing_reflow_gap) >= cols
    }

    /// Fills the row with blanks up to the terminal width.
    ///
    /// A soft-wrapped row that paints fewer columns than the terminal has
    /// leaves the cursor inside the row, so the next row's cells would be
    /// painted onto it. Only cells past the row's last used cell are missing,
    /// and those are always default blanks, so plain spaces reproduce them.
    fn fill_to_width(&mut self, cols: u32) {
        let missing = cols.saturating_sub(self.columns);
        if missing == 0 {
            return;
        }
        self.bytes.extend_from_slice(RESET_RENDITION);
        self.bytes
            .extend(std::iter::repeat_n(b' ', missing as usize));
        if self.columns == 0 {
            self.leading_columns = 1;
        }
        self.columns = cols;
        self.bytes.extend_from_slice(&self.extent_restore);
    }

    /// Returns the most bytes [`fill_to_width`](Self::fill_to_width) can add to
    /// this row while it is being linked to its neighbours.
    pub(super) fn reserved_fill_len(&self, cols: u32) -> usize {
        let missing = cols.saturating_sub(self.columns);
        if missing == 0 || !(self.wrapped || self.leading_columns == 0) {
            return 0;
        }
        RESET_RENDITION
            .len()
            .saturating_add(missing as usize)
            .saturating_add(self.extent_restore.len())
    }
}

/// Places every row of a repainted run at its own terminal line.
///
/// Rows are painted back to back, so the replaying terminal decides where a
/// row ends up: a soft-wrapped row hands the next row's first cell to the next
/// line only when that cell no longer fits in the columns the row leaves free.
/// [`link_row_pair`] restores that condition wherever the grid no longer
/// satisfies it, which happens whenever a wrapped row was shortened after it
/// wrapped (DCH, or a wide glyph that pre-wrapped and left a gap).
pub(super) fn link_rows(rows: &mut [RenderedRow], cols: u32) {
    for index in 1..rows.len() {
        let (head, tail) = rows.split_at_mut(index);
        let _ = link_row_pair(head.last_mut(), tail.first_mut(), cols);
    }
}

/// Links one repainted row to its successor and returns the bytes that must
/// separate them.
///
/// A row that is not soft-wrapped is separated by CRLF. A soft-wrapped row is
/// separated by nothing at all: the terminal's own wrap moves to the next line
/// and marks the row wrapped, which is the only way a replay can reproduce the
/// flag. That wrap needs a cell to land on, and it must not fit on the wrapped
/// row, so both rows are filled to the full width when necessary.
pub(super) fn link_row_pair(
    previous: Option<&mut RenderedRow>,
    next: Option<&mut RenderedRow>,
    cols: u32,
) -> &'static [u8] {
    let (Some(previous), Some(next)) = (previous, next) else {
        return b"";
    };
    if !previous.wrapped {
        return LINE_BREAK;
    }
    if next.leading_columns == 0 {
        // A row that paints nothing cannot resolve a pending wrap, so the
        // following rows would climb one line each. Filling it keeps the wrap
        // and repaints the same blank cells.
        next.fill_to_width(cols);
    }
    if !previous.wraps_into(next, cols) {
        previous.fill_to_width(cols);
    }
    b""
}
