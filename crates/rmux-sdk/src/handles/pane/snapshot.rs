use crate::handles::session::unexpected_response;
use crate::{
    Pane, PaneAttributes, PaneCell, PaneColor, PaneCursor, PaneGlyph, PaneSnapshot, Result,
};
use rmux_proto::{
    PaneSnapshotCell, PaneSnapshotCursor, PaneSnapshotRefRequest, PaneSnapshotRequest,
    PaneSnapshotResponse, Request, Response, CAPABILITY_SDK_PANE_BY_ID,
};

use super::target::{is_already_closed_error, is_already_closed_pane_id_error, parse_error};

pub(super) async fn pane_snapshot(pane: &Pane) -> Result<PaneSnapshot> {
    let Some(resolved_id) = pane.id().await? else {
        return Ok(PaneSnapshot::default());
    };

    // The pane was listed at the start of this call, but the daemon can still
    // close it between the existence check and the snapshot endpoint round
    // trip. Treat the already-closed protocol errors emitted in that window as
    // a "vanished mid-snapshot" signal and degrade to a default snapshot,
    // while genuine transport or protocol errors still propagate.
    match request_pane_snapshot(pane, resolved_id).await {
        Ok(response) => snapshot_from_response(response),
        Err(error)
            if is_already_closed_error(&error, pane.target())
                || is_already_closed_pane_id_error(
                    &error,
                    &pane.target().session_name,
                    resolved_id,
                ) =>
        {
            Ok(PaneSnapshot::default())
        }
        Err(error) => Err(error),
    }
}

async fn request_pane_snapshot(
    pane: &Pane,
    resolved_id: rmux_proto::PaneId,
) -> Result<PaneSnapshotResponse> {
    let response = if pane.stable_id.is_some() {
        crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await?;
        pane.transport()
            .request(Request::PaneSnapshotRef(PaneSnapshotRefRequest {
                target: pane.proto_target_ref(),
            }))
            .await?
    } else if crate::capabilities::supports(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await? {
        // Slot handles address the visible (base-index adjusted) coordinates
        // that list-panes reports, which is also what the id resolution above
        // matched. The legacy PaneSnapshot slot endpoint resolves raw slot
        // indexes instead, so under base-index/pane-base-index != 0 it reads
        // a different (usually absent) pane and degrades to the stale-slot
        // default snapshot (issue #94). Route through the pane id we just
        // resolved whenever the daemon supports it.
        pane.transport()
            .request(Request::PaneSnapshotRef(PaneSnapshotRefRequest {
                target: rmux_proto::PaneTargetRef::by_id(
                    pane.target().session_name.clone(),
                    resolved_id,
                ),
            }))
            .await?
    } else {
        pane.transport()
            .request(Request::PaneSnapshot(PaneSnapshotRequest {
                target: pane.target().into(),
            }))
            .await?
    };

    match response {
        Response::PaneSnapshot(response) => Ok(response),
        response => Err(unexpected_response("pane-snapshot", response)),
    }
}

pub(super) fn snapshot_from_response(response: PaneSnapshotResponse) -> Result<PaneSnapshot> {
    let cells = response.cells.into_iter().map(cell_from_wire).collect();
    let cursor = cursor_from_wire(response.cursor);
    let snapshot = PaneSnapshot {
        cols: response.cols,
        rows: response.rows,
        cells,
        cursor,
        revision: response.revision,
    };
    snapshot.validate_shape().map_err(|error| {
        parse_error(format!(
            "pane-snapshot response had malformed row-major cell shape: {error}"
        ))
    })?;
    Ok(snapshot)
}

pub(super) fn cell_from_wire(cell: PaneSnapshotCell) -> PaneCell {
    let glyph = if cell.padding {
        PaneGlyph {
            text: cell.text,
            width: cell.width,
            padding: true,
        }
    } else {
        PaneGlyph::new(cell.text, cell.width)
    };
    PaneCell {
        glyph,
        attributes: PaneAttributes::from_bits(cell.attributes),
        foreground: PaneColor::from_encoded(cell.fg),
        background: PaneColor::from_encoded(cell.bg),
        underline: PaneColor::from_encoded(cell.us),
    }
}

fn cursor_from_wire(cursor: PaneSnapshotCursor) -> PaneCursor {
    PaneCursor::new(cursor.row, cursor.col, cursor.visible, cursor.style)
}
