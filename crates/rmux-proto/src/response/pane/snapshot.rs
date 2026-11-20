//! Daemon-backed pane snapshot wire payloads.

use serde::{Deserialize, Serialize};

/// One captured pane cell on the daemon snapshot wire.
///
/// Cells are produced from rmux-core's structured `ScreenCellView`, so the
/// glyph text, recorded display width, and padding flag travel verbatim
/// across the wire. Padding cells (the trailing column of a wide glyph)
/// carry `width = 0` and `padding = true`; their `text` field carries the
/// space sentinel rmux-core uses to represent owned padding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSnapshotCell {
    /// Recorded glyph text payload.
    pub text: String,
    /// Recorded display width of the leading glyph; `0` for padding cells.
    pub width: u8,
    /// Whether this cell is wide-glyph padding for the preceding column.
    pub padding: bool,
    /// Raw cell attribute bitset.
    pub attributes: u16,
    /// Raw foreground colour encoding.
    pub fg: i32,
    /// Raw background colour encoding.
    pub bg: i32,
    /// Raw underline colour encoding.
    pub us: i32,
    /// Hyperlink inner ID.
    pub link: u32,
}

/// Captured cursor position on the daemon snapshot wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSnapshotCursor {
    /// Zero-based cursor row within the visible viewport.
    pub row: u16,
    /// Zero-based cursor column within the visible viewport.
    pub col: u16,
    /// Whether the cursor is visible according to the live mode bits.
    pub visible: bool,
    /// Raw cursor style value.
    pub style: u32,
}

/// Response payload for the daemon-backed pane snapshot endpoint.
///
/// `cells` is row-major with `row * cols + col` indexing and exactly
/// `cols * rows` entries. The daemon-derived `revision` is non-zero for
/// every captured live pane. It is assigned per materialised capture from a
/// fingerprint of exactly these inputs: cols, rows, cells, cursor,
/// output_sequence, history_size (lines), history_bytes and pane id. Four of
/// them are not carried by this response, so the revision can advance between
/// two responses that are equal in every field other than `revision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSnapshotResponse {
    /// Visible pane width in terminal columns.
    pub cols: u16,
    /// Visible pane height in terminal rows.
    pub rows: u16,
    /// Row-major cells, `cols * rows` long.
    pub cells: Vec<PaneSnapshotCell>,
    /// Captured cursor coordinates and state.
    pub cursor: PaneSnapshotCursor,
    /// Daemon-derived revision counter for this captured state.
    ///
    /// One counter per pane is shared with `PaneSurfaceSnapshot::revision`:
    /// snapshot requests, surface-stream frames and recovery snapshots all
    /// advance it. A capture keeps the revision when its fingerprint equals
    /// the previous capture's and advances it otherwise, even when returning
    /// to an older state; a surface-stream reset can also raise it for an
    /// identical state. Equal revisions mean two captures observed the same
    /// state, not that the pane was unmodified between them.
    pub revision: u64,
}
