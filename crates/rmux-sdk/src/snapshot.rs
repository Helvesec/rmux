//! Inert pane snapshot DTOs for SDK consumers.
//!
//! These types model an already-captured pane grid. They do not parse
//! terminal output, resolve tmux targets, or depend on RMUX core/server
//! internals.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

mod attrs;
mod cell;
mod color;
mod cursor;
mod glyph;

pub use attrs::PaneAttributes;
pub use cell::PaneCell;
pub use color::PaneColor;
pub use cursor::PaneCursor;
pub use glyph::PaneGlyph;

/// A captured pane grid in row-major cell order.
///
/// `revision` is a daemon-derived counter assigned each time the daemon
/// materialises a capture of the pane, not each time the pane mutates; see
/// the field documentation for what equal or different revisions prove.
/// There is no separate `current_revision()` getter on the SDK pane handle,
/// because the only authoritative point-in-time revision value is the one
/// carried by a freshly captured snapshot (or by a revision-carrying
/// [`PaneEvent`](crate::PaneEvent) variant).
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct PaneSnapshot {
    /// Visible pane width in terminal columns.
    pub cols: u16,
    /// Visible pane height in terminal rows.
    pub rows: u16,
    /// Row-major cells, with `row * cols + col` indexing.
    pub cells: Vec<PaneCell>,
    /// Captured cursor coordinates and state.
    pub cursor: PaneCursor,
    /// Daemon-derived revision counter for this captured pane state.
    ///
    /// The daemon assigns it whenever it materialises a capture of the pane:
    /// a snapshot request, a surface-stream frame or a recovery snapshot.
    /// Each capture hashes a fingerprint of `cols`, `rows`, `cells`,
    /// `cursor`, the pane's output sequence, its history line and byte
    /// counts, and the pane id. A capture keeps the revision for an unchanged
    /// fingerprint unless a surface-stream floor raises it, including on reset.
    /// Any other fingerprint, including a return to an older state, advances
    /// it. All producers share one counter per pane, so the revision can advance
    /// between two snapshots that are equal in every field other than `revision`.
    ///
    /// Equal revisions on two snapshots therefore mean that those two captures
    /// observed the same daemon-side state. They do not prove that the pane
    /// was unmodified between them: a change that no capture observed leaves
    /// no trace. Live captures always carry a non-zero revision; a snapshot
    /// for a pane that is no longer listed is the empty default with
    /// revision `0`.
    pub revision: u64,
}

impl PaneSnapshot {
    /// Creates a snapshot after checking the row-major cell count.
    ///
    /// The expected cell count is `rows * cols`. Zero-sized dimensions are
    /// allowed and therefore expect zero cells. The revision defaults to `0`;
    /// use [`Self::with_revision`] to attach a daemon-derived revision.
    pub fn new(
        cols: u16,
        rows: u16,
        cells: Vec<PaneCell>,
        cursor: PaneCursor,
    ) -> Result<Self, PaneSnapshotShapeError> {
        let snapshot = Self {
            cols,
            rows,
            cells,
            cursor,
            revision: 0,
        };
        snapshot.validate_shape()?;
        Ok(snapshot)
    }

    /// Returns a copy of this snapshot with the supplied revision.
    #[must_use]
    pub fn with_revision(mut self, revision: u64) -> Self {
        self.revision = revision;
        self
    }

    /// Returns the number of row-major cells implied by `rows * cols`.
    #[must_use]
    pub fn expected_cell_count(&self) -> usize {
        expected_cell_count(self.cols, self.rows)
    }

    /// Returns whether `cells.len()` exactly matches `rows * cols`.
    #[must_use]
    pub fn is_row_major_shape(&self) -> bool {
        self.cells.len() == self.expected_cell_count()
    }

    /// Checks the row-major cell-count invariant.
    pub fn validate_shape(&self) -> Result<(), PaneSnapshotShapeError> {
        let expected = self.expected_cell_count();
        if self.cells.len() == expected {
            Ok(())
        } else {
            Err(PaneSnapshotShapeError {
                cols: self.cols,
                rows: self.rows,
                actual_cells: self.cells.len(),
                expected_cells: expected,
            })
        }
    }

    /// Returns one cell by visible row and column.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<&PaneCell> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        let index = usize::from(row)
            .saturating_mul(usize::from(self.cols))
            .saturating_add(usize::from(col));
        self.cells.get(index)
    }

    /// Returns one row slice by visible row.
    ///
    /// Malformed snapshots with too few cells return `None` for incomplete
    /// rows rather than panicking.
    #[must_use]
    pub fn row_cells(&self, row: u16) -> Option<&[PaneCell]> {
        if row >= self.rows {
            return None;
        }

        let cols = usize::from(self.cols);
        let start = usize::from(row).checked_mul(cols)?;
        let end = start.checked_add(cols)?;
        self.cells.get(start..end)
    }

    /// Resolves the owning non-padding cell column for a visible position.
    ///
    /// If the addressed cell is not padding, its own column is returned. If it
    /// is padding for a wide glyph, the leading glyph column is returned only
    /// when that glyph's recorded display width spans the requested column.
    #[must_use]
    pub fn owning_cell_col(&self, row: u16, col: u16) -> Option<u16> {
        let cell = self.cell(row, col)?;
        if !cell.is_padding() {
            return Some(col);
        }

        let mut owner = col;
        while owner > 0 {
            owner -= 1;
            let candidate = self.cell(row, owner)?;
            if !candidate.is_padding() {
                let width = u16::from(candidate.glyph.width.max(1));
                if owner.saturating_add(width) > col {
                    return Some(owner);
                }
                return None;
            }
        }

        None
    }

    /// Iterates visible, non-padding cells with their original row and column.
    ///
    /// Padding cells belonging to wide glyphs are skipped, while the leading
    /// glyph keeps its original display column.
    pub fn visible_cells(&self) -> impl Iterator<Item = (u16, u16, &PaneCell)> + '_ {
        let cols = usize::from(self.cols);
        let rows = usize::from(self.rows);
        self.cells
            .iter()
            .enumerate()
            .filter_map(move |(index, cell)| {
                if cols == 0 || cell.is_padding() {
                    return None;
                }

                let row = index / cols;
                if row >= rows {
                    return None;
                }
                let col = index % cols;
                Some((row as u16, col as u16, cell))
            })
    }

    /// Renders one visible row using RMUX core's lossy plain-text behavior.
    ///
    /// Padding cells are skipped and trailing space characters are trimmed.
    /// Other whitespace and control-like payloads are preserved verbatim. If a
    /// malformed snapshot ends partway through this row, the available cells
    /// are rendered instead of panicking.
    #[must_use]
    pub fn visible_row_text(&self, row: u16) -> Option<String> {
        self.lossy_row_cells(row).map(render_cells_lossy)
    }

    /// Renders one visible row, returning an empty string for out-of-bounds rows.
    #[must_use]
    pub fn row_text(&self, row: u16) -> String {
        self.visible_row_text(row).unwrap_or_default()
    }

    /// Renders all visible rows using lossy plain-text behavior.
    ///
    /// Incomplete malformed rows render their available cells instead of
    /// panicking.
    #[must_use]
    pub fn visible_lines(&self) -> Vec<String> {
        (0..self.rows)
            .map(|row| self.visible_row_text(row).unwrap_or_default())
            .collect()
    }

    /// Renders all visible rows joined by `\n`.
    ///
    /// The returned string has no synthetic trailing newline.
    #[must_use]
    pub fn visible_text(&self) -> String {
        self.visible_lines().join("\n")
    }

    fn lossy_row_cells(&self, row: u16) -> Option<&[PaneCell]> {
        if row >= self.rows {
            return None;
        }

        let cols = usize::from(self.cols);
        if cols == 0 {
            return Some(&[]);
        }

        let start = usize::from(row).checked_mul(cols)?;
        if start >= self.cells.len() {
            return Some(&[]);
        }
        let end = start.saturating_add(cols).min(self.cells.len());
        Some(&self.cells[start..end])
    }
}

impl Serialize for PaneSnapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate_shape().map_err(serde::ser::Error::custom)?;
        PaneSnapshotFieldsRef {
            cols: self.cols,
            rows: self.rows,
            cells: &self.cells,
            cursor: &self.cursor,
            revision: self.revision,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PaneSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = PaneSnapshotFields::deserialize(deserializer)?;
        Self::new(fields.cols, fields.rows, fields.cells, fields.cursor)
            .map(|snapshot| snapshot.with_revision(fields.revision))
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Serialize)]
struct PaneSnapshotFieldsRef<'a> {
    cols: u16,
    rows: u16,
    cells: &'a [PaneCell],
    cursor: &'a PaneCursor,
    revision: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PaneSnapshotFields {
    cols: u16,
    rows: u16,
    cells: Vec<PaneCell>,
    cursor: PaneCursor,
    revision: u64,
}

/// Error returned when a snapshot's dimensions do not match its cell vector.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneSnapshotShapeError {
    cols: u16,
    rows: u16,
    actual_cells: usize,
    expected_cells: usize,
}

impl PaneSnapshotShapeError {
    /// Returns the snapshot column count.
    #[must_use]
    pub const fn cols(&self) -> u16 {
        self.cols
    }

    /// Returns the snapshot row count.
    #[must_use]
    pub const fn rows(&self) -> u16 {
        self.rows
    }

    /// Returns the actual number of cells supplied.
    #[must_use]
    pub const fn actual_cells(&self) -> usize {
        self.actual_cells
    }

    /// Returns the expected `rows * cols` cell count.
    #[must_use]
    pub const fn expected_cells(&self) -> usize {
        self.expected_cells
    }
}

impl fmt::Display for PaneSnapshotShapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pane snapshot shape mismatch: {}x{} expects {} cells, got {}",
            self.cols, self.rows, self.expected_cells, self.actual_cells
        )
    }
}

impl std::error::Error for PaneSnapshotShapeError {}

fn expected_cell_count(cols: u16, rows: u16) -> usize {
    usize::from(cols) * usize::from(rows)
}

pub(crate) fn render_cells_lossy(cells: &[PaneCell]) -> String {
    let mut rendered = String::new();
    for cell in cells {
        if cell.is_padding() {
            continue;
        }
        rendered.push_str(cell.text());
    }
    while rendered.ends_with(' ') {
        rendered.pop();
    }
    rendered
}
