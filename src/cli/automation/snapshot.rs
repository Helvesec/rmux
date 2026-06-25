use std::path::Path;

use rmux_proto::{PaneSnapshotCell, PaneSnapshotResponse};
use serde_json::{json, Value};

use crate::cli_args::{PaneSnapshotArgs, SnapshotRegion};

use super::super::ExitFailure;
use super::common::{
    check_disabled, connect_cli, pane_snapshot, resolve_pane_ref, visible_line_from_cells,
    write_json, write_stdout_line, SCHEMA_VERSION,
};

pub(crate) fn run_pane_snapshot(
    args: PaneSnapshotArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    check_disabled("RMUX_DISABLE_PANE_SNAPSHOT", "pane-snapshot")?;
    let mut connection = connect_cli(socket_path)?;
    let target = resolve_pane_ref(&mut connection, args.target.as_ref(), "pane-snapshot")?;
    let snapshot = pane_snapshot(&mut connection, target)?;
    let view = SnapshotView::new(&snapshot, args.region)?;
    if args.json {
        return write_json(&snapshot_json(&snapshot, &view, args.style));
    }
    write_stdout_line(&view.lines.join("\n"))
}

struct SnapshotView {
    row: u16,
    col: u16,
    rows: u16,
    cols: u16,
    lines: Vec<String>,
    cells: Vec<Vec<PaneSnapshotCell>>,
}

impl SnapshotView {
    fn new(
        snapshot: &PaneSnapshotResponse,
        region: Option<SnapshotRegion>,
    ) -> Result<Self, ExitFailure> {
        let region = region.unwrap_or(SnapshotRegion {
            row: 0,
            col: 0,
            rows: snapshot.rows,
            cols: snapshot.cols,
        });
        validate_region(snapshot, region)?;
        let mut lines = Vec::with_capacity(usize::from(region.rows));
        let mut cells = Vec::with_capacity(usize::from(region.rows));
        let snapshot_cols = usize::from(snapshot.cols);
        for row_offset in 0..usize::from(region.rows) {
            let row = usize::from(region.row) + row_offset;
            let col = usize::from(region.col);
            let end_col = col + usize::from(region.cols);
            let start = row * snapshot_cols + col;
            let end = row * snapshot_cols + end_col;
            let row_cells = snapshot.cells[start..end].to_vec();
            lines.push(visible_line_from_cells(&row_cells));
            cells.push(row_cells);
        }
        Ok(Self {
            row: region.row,
            col: region.col,
            rows: region.rows,
            cols: region.cols,
            lines,
            cells,
        })
    }
}

fn validate_region(
    snapshot: &PaneSnapshotResponse,
    region: SnapshotRegion,
) -> Result<(), ExitFailure> {
    let expected_cells = usize::from(snapshot.rows).saturating_mul(usize::from(snapshot.cols));
    if snapshot.cells.len() < expected_cells {
        return Err(ExitFailure::new(
            1,
            "pane-snapshot response has an incomplete cell grid",
        ));
    }
    let row_end = u32::from(region.row) + u32::from(region.rows);
    let col_end = u32::from(region.col) + u32::from(region.cols);
    if row_end > u32::from(snapshot.rows) || col_end > u32::from(snapshot.cols) {
        return Err(ExitFailure::new(
            1,
            "pane-snapshot --region exceeds pane bounds",
        ));
    }
    Ok(())
}

fn snapshot_json(
    snapshot: &PaneSnapshotResponse,
    view: &SnapshotView,
    include_style: bool,
) -> Value {
    let mut payload = json!({
        "schema_version": SCHEMA_VERSION,
        "ok": true,
        "rows": snapshot.rows,
        "cols": snapshot.cols,
        "revision": snapshot.revision,
        "cursor": {
            "row": snapshot.cursor.row,
            "col": snapshot.cursor.col,
            "visible": snapshot.cursor.visible,
            "style": snapshot.cursor.style,
        },
        "region": {
            "row": view.row,
            "col": view.col,
            "rows": view.rows,
            "cols": view.cols,
        },
        "text": view.lines.join("\n"),
        "lines": view.lines,
    });
    if include_style {
        payload["cells"] = Value::Array(
            view.cells
                .iter()
                .map(|row| {
                    Value::Array(
                        row.iter()
                            .map(|cell| {
                                json!({
                                    "text": cell.text,
                                    "width": cell.width,
                                    "padding": cell.padding,
                                    "attributes": cell.attributes,
                                    "fg": cell.fg,
                                    "bg": cell.bg,
                                    "us": cell.us,
                                    "link": cell.link,
                                })
                            })
                            .collect(),
                    )
                })
                .collect(),
        );
    }
    payload
}

#[cfg(test)]
mod tests {
    use rmux_proto::{PaneSnapshotCell, PaneSnapshotCursor, PaneSnapshotResponse};

    use crate::cli_args::SnapshotRegion;

    use super::SnapshotView;

    fn cell(text: &str, width: u8, padding: bool) -> PaneSnapshotCell {
        PaneSnapshotCell {
            text: text.to_owned(),
            width,
            padding,
            attributes: 0,
            fg: 0,
            bg: 0,
            us: 0,
            link: 0,
        }
    }

    fn snapshot() -> PaneSnapshotResponse {
        PaneSnapshotResponse {
            cols: 4,
            rows: 1,
            cells: vec![
                cell("A", 1, false),
                cell("界", 2, false),
                cell(" ", 0, true),
                cell("B", 1, false),
            ],
            cursor: PaneSnapshotCursor {
                row: 0,
                col: 0,
                visible: false,
                style: 0,
            },
            revision: 1,
        }
    }

    #[test]
    fn region_text_uses_terminal_cell_columns_not_character_offsets() {
        let snapshot = snapshot();
        let view = SnapshotView::new(
            &snapshot,
            Some(SnapshotRegion {
                row: 0,
                col: 1,
                rows: 1,
                cols: 3,
            }),
        )
        .expect("region is valid");

        assert_eq!(view.lines, vec!["界B"]);
    }

    #[test]
    fn region_starting_on_wide_padding_does_not_leak_owner_glyph() {
        let snapshot = snapshot();
        let view = SnapshotView::new(
            &snapshot,
            Some(SnapshotRegion {
                row: 0,
                col: 2,
                rows: 1,
                cols: 2,
            }),
        )
        .expect("region is valid");

        assert_eq!(view.lines, vec!["B"]);
    }

    #[test]
    fn incomplete_snapshot_grid_is_rejected_before_slicing() {
        let mut snapshot = snapshot();
        snapshot.cells.pop();

        let error = match SnapshotView::new(&snapshot, None) {
            Ok(_) => panic!("grid is incomplete"),
            Err(error) => error,
        };

        assert!(
            error
                .message()
                .contains("pane-snapshot response has an incomplete cell grid"),
            "{}",
            error.message()
        );
    }
}
