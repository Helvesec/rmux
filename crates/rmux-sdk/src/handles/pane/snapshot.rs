use crate::handles::session::unexpected_response;
use crate::{
    Pane, PaneAttributes, PaneCell, PaneColor, PaneCursor, PaneGlyph, PaneId, PaneSnapshot, Result,
};
use rmux_proto::{
    PaneSnapshotCell, PaneSnapshotCursor, PaneSnapshotRefRequest, PaneSnapshotRequest,
    PaneSnapshotResponse, PaneTargetRef, Request, Response, CAPABILITY_SDK_PANE_BY_ID,
};

use super::target::{is_already_closed_error, parse_error};

pub(super) async fn pane_snapshot(pane: &Pane) -> Result<PaneSnapshot> {
    // Resolve the handle to a concrete daemon pane identity first. A genuinely
    // absent slot (stale or never-existed) resolves to `None` and keeps the
    // documented default (revision 0) snapshot contract.
    let Some(pane_id) = pane.id().await? else {
        return Ok(PaneSnapshot::default());
    };

    // The pane was listed at the start of this call, but the daemon can still
    // close it between the existence check and the snapshot endpoint round
    // trip. Treat the already-closed protocol errors emitted in that window as
    // a "vanished mid-snapshot" signal and degrade to a default snapshot,
    // while genuine transport or protocol errors still propagate.
    match request_pane_snapshot(pane, pane_id).await {
        Ok(response) => snapshot_from_response(response),
        Err(error) if is_already_closed_error(&error, pane.target()) => Ok(PaneSnapshot::default()),
        Err(error) => Err(error),
    }
}

async fn request_pane_snapshot(pane: &Pane, pane_id: PaneId) -> Result<PaneSnapshotResponse> {
    let response = if pane.stable_id.is_some() {
        crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await?;
        pane.transport()
            .request(Request::PaneSnapshotRef(PaneSnapshotRefRequest {
                target: pane.proto_target_ref(),
            }))
            .await?
    } else {
        request_slot_pane_snapshot(pane, pane_id).await?
    };

    match response {
        Response::PaneSnapshot(response) => Ok(response),
        response => Err(unexpected_response("pane-snapshot", response)),
    }
}

// A slot (index) handle reads through the stable pane id its listing resolved to,
// whenever the daemon advertises by-id addressing. The by-slot snapshot endpoint
// resolves `PaneTarget.pane_index` as a raw pane index, so a live pane whose
// visible index differs from its raw index (any non-zero pane-base-index) would
// resolve to the blank stale-slot default instead of real content.
// The by-id endpoint maps the pane id back to its raw index inside the daemon, so
// the index handle observes the same live pane as the discovery/by-id path.
// Daemons that predate by-id snapshotting keep the legacy slot endpoint, so their
// behavior is unchanged.
async fn request_slot_pane_snapshot(pane: &Pane, pane_id: PaneId) -> Result<Response> {
    match crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await {
        Ok(()) => Ok(pane
            .transport()
            .request(Request::PaneSnapshotRef(PaneSnapshotRefRequest {
                target: PaneTargetRef::by_id(pane.target().session_name.clone(), pane_id),
            }))
            .await?),
        Err(error) if crate::capabilities::is_unavailable(&error, CAPABILITY_SDK_PANE_BY_ID) => {
            Ok(pane
                .transport()
                .request(Request::PaneSnapshot(PaneSnapshotRequest {
                    target: pane.target().into(),
                }))
                .await?)
        }
        Err(error) => Err(error),
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
