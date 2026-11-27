use rmux_proto::{PaneSnapshotCell, PaneSnapshotResponse};

use crate::handles::pane::snapshot::{cell_from_wire, cursor_from_wire};
use crate::PaneSnapshot;

/// Projects wire cells through the same glyph and coordinate rules as SDK snapshots.
/// Lossy renderers tolerate incomplete grids; daemon snapshot admission validates shape.
pub fn project_snapshot(response: &PaneSnapshotResponse) -> PaneSnapshot {
    PaneSnapshot {
        cols: response.cols,
        rows: response.rows,
        cells: response.cells.iter().cloned().map(cell_from_wire).collect(),
        cursor: cursor_from_wire(response.cursor),
        revision: response.revision,
    }
}

pub fn render_wire_cells(cells: &[PaneSnapshotCell]) -> String {
    let cells = cells
        .iter()
        .cloned()
        .map(cell_from_wire)
        .collect::<Vec<_>>();
    crate::snapshot::render_cells_lossy(&cells)
}
