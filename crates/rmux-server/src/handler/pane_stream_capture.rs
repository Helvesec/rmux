use std::sync::Arc;

use rmux_core::input::mode;
use rmux_core::PaneId;
use rmux_proto::{
    PaneRawRebase, PaneRawRebaseReason, PaneSnapshotCursor, PaneSnapshotResponse, PaneSurfaceFrame,
    PaneSurfaceSnapshot, RmuxError,
};

use crate::pane_io::{PaneBoundary, PaneInvalidationReason, PaneOutputReceiver};
use crate::pane_recovery::PaneRecoverySeed;

use super::super::pane_support::{
    collect_cells, compute_snapshot_fingerprint, cursor_coord_to_u16,
};
use super::super::RequestHandler;
use super::types::{PaneStreamSource, PaneSurfaceFingerprint};

pub(in crate::handler) struct CapturedPaneBoundary {
    pub(in crate::handler) boundary: PaneBoundary,
    pub(in crate::handler) seed: PaneRecoverySeed,
    pub(in crate::handler) receiver: PaneOutputReceiver,
}

pub(in crate::handler) struct CapturedSurfaceBoundary {
    pub(in crate::handler) boundary: PaneBoundary,
    pub(in crate::handler) fingerprint: PaneSurfaceFingerprint,
    pub(in crate::handler) seed: Option<PaneRecoverySeed>,
    pub(in crate::handler) receiver: PaneOutputReceiver,
}

pub(in crate::handler) fn capture_source(source: &PaneStreamSource) -> CapturedPaneBoundary {
    let (boundary, seed, receiver) = source.output.capture_with_observer(|| {
        let transcript = source
            .transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        PaneRecoverySeed::capture(&transcript)
    });
    CapturedPaneBoundary {
        boundary,
        seed,
        receiver,
    }
}

pub(in crate::handler) fn capture_surface_source(
    source: &PaneStreamSource,
    previous: &PaneSurfaceFingerprint,
    force: bool,
) -> CapturedSurfaceBoundary {
    let (boundary, captured, receiver) = source.output.capture_with_observer(|| {
        let transcript = source
            .transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fingerprint = PaneSurfaceFingerprint::capture(transcript.screen());
        let seed =
            (force || &fingerprint != previous).then(|| PaneRecoverySeed::capture(&transcript));
        (fingerprint, seed)
    });
    CapturedSurfaceBoundary {
        boundary,
        fingerprint: captured.0,
        seed: captured.1,
        receiver,
    }
}

pub(in crate::handler) fn materialize_raw_rebase(
    handler: &RequestHandler,
    pane_id: PaneId,
    epoch: u64,
    reason: PaneRawRebaseReason,
    include_snapshot: bool,
    captured: &CapturedPaneBoundary,
) -> Result<PaneRawRebase, RmuxError> {
    let keyframe = captured.seed.keyframe();
    let snapshot = include_snapshot
        .then(|| materialize_typed_snapshot(handler, pane_id, &captured.seed))
        .transpose()?;
    Ok(PaneRawRebase {
        epoch,
        generation: captured.boundary.generation,
        invalidation_revision: captured.boundary.invalidation_revision,
        next_sequence: captured.boundary.next_output_sequence,
        cols: keyframe.cols,
        rows: keyframe.rows,
        keyframe: keyframe.bytes,
        alternate: keyframe.alternate,
        snapshot,
        reason,
    })
}

pub(in crate::handler) fn materialize_surface_frame(
    handler: &RequestHandler,
    pane_id: PaneId,
    epoch: u64,
    surface_revision: u64,
    next_output_sequence: u64,
    seed: &PaneRecoverySeed,
) -> Result<Arc<PaneSurfaceFrame>, RmuxError> {
    let screen = seed.screen();
    let size = screen.size();
    let history_size = screen.history_size();
    let history_bytes = screen.history_bytes();
    let cells = collect_cells(screen, size.cols, size.rows, history_size)?;
    let (cursor_x, cursor_y) = screen.cursor_position();
    let (scroll_top, scroll_bottom) = screen.scroll_region();
    let cursor = PaneSnapshotCursor {
        row: cursor_coord_to_u16(cursor_y),
        col: cursor_coord_to_u16(cursor_x),
        visible: screen.mode() & mode::MODE_CURSOR != 0,
        style: screen.cursor_style(),
    };
    let fingerprint = compute_snapshot_fingerprint(
        size.cols,
        size.rows,
        &cells,
        &cursor,
        seed.output_sequence(),
        history_size,
        history_bytes,
        pane_id.as_u32(),
    );
    let grid_revision = handler.assign_pane_snapshot_revision(pane_id, fingerprint);
    Ok(Arc::new(PaneSurfaceFrame {
        epoch,
        revision: surface_revision,
        next_output_sequence,
        snapshot: PaneSurfaceSnapshot {
            cols: size.cols,
            rows: size.rows,
            cells,
            cursor,
            title: screen.title().to_owned(),
            path: screen.path().to_owned(),
            mode_bits: screen.mode(),
            alternate: screen.is_alternate(),
            scroll_top,
            scroll_bottom,
            history_size: saturating_u64(history_size),
            history_bytes: saturating_u64(history_bytes),
            revision: grid_revision,
        },
    }))
}

fn materialize_typed_snapshot(
    handler: &RequestHandler,
    pane_id: PaneId,
    seed: &PaneRecoverySeed,
) -> Result<PaneSnapshotResponse, RmuxError> {
    let screen = seed.screen();
    let size = screen.size();
    let history_size = screen.history_size();
    let history_bytes = screen.history_bytes();
    let cells = collect_cells(screen, size.cols, size.rows, history_size)?;
    let (cursor_x, cursor_y) = screen.cursor_position();
    let cursor = PaneSnapshotCursor {
        row: cursor_coord_to_u16(cursor_y),
        col: cursor_coord_to_u16(cursor_x),
        visible: screen.mode() & mode::MODE_CURSOR != 0,
        style: screen.cursor_style(),
    };
    let fingerprint = compute_snapshot_fingerprint(
        size.cols,
        size.rows,
        &cells,
        &cursor,
        seed.output_sequence(),
        history_size,
        history_bytes,
        pane_id.as_u32(),
    );
    let revision = handler.assign_pane_snapshot_revision(pane_id, fingerprint);
    Ok(PaneSnapshotResponse {
        cols: size.cols,
        rows: size.rows,
        cells,
        cursor,
        revision,
    })
}

pub(in crate::handler) const fn raw_reason(reason: PaneInvalidationReason) -> PaneRawRebaseReason {
    match reason {
        PaneInvalidationReason::Initial => PaneRawRebaseReason::Initial,
        PaneInvalidationReason::Resize => PaneRawRebaseReason::Resize,
        PaneInvalidationReason::ClearHistory => PaneRawRebaseReason::ClearHistory,
        PaneInvalidationReason::ParserStateExpired => PaneRawRebaseReason::ParserStateExpired,
        PaneInvalidationReason::TerminalReset => PaneRawRebaseReason::TerminalReset,
        PaneInvalidationReason::TranscriptMutation => PaneRawRebaseReason::TranscriptMutation,
        PaneInvalidationReason::GenerationChanged => PaneRawRebaseReason::GenerationChanged,
    }
}

fn saturating_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
