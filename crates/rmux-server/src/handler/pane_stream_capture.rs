use std::collections::BTreeSet;
use std::sync::Arc;

use rmux_core::input::mode;
use rmux_core::PaneId;
use rmux_proto::{
    PaneRawRebase, PaneRawRebaseReason, PaneRecoveryCoverage, PaneSnapshotCursor,
    PaneSnapshotResponse, PaneSurfaceDynamicColors, PaneSurfaceFrame, PaneSurfaceHyperlink,
    PaneSurfaceSnapshot, RmuxError,
};

use crate::pane_io::{PaneBoundary, PaneInvalidationReason, PaneOutputReceiver};
use crate::pane_recovery::{PaneDynamicColors, PaneRecoveryDraft, PaneRecoverySeed};

use super::super::pane_support::{
    collect_cells, compute_snapshot_fingerprint, cursor_coord_to_u16,
};
use super::super::RequestHandler;
use super::types::{PaneStreamSource, PaneSurfaceFingerprint};
use crate::pane_recovery::MAX_RECOVERY_TYPED_SNAPSHOT_CELLS;

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

pub(in crate::handler) fn capture_source(
    source: &PaneStreamSource,
) -> Result<CapturedPaneBoundary, RmuxError> {
    capture_source_with_materializer(source, PaneRecoveryDraft::materialize)
}

fn capture_source_with_materializer(
    source: &PaneStreamSource,
    materialize: impl FnOnce(PaneRecoveryDraft) -> Result<PaneRecoverySeed, RmuxError>,
) -> Result<CapturedPaneBoundary, RmuxError> {
    let (boundary, draft, receiver) = source.output.capture_with_observer(|| {
        let transcript = source
            .transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        PaneRecoveryDraft::capture(&transcript)
    });
    Ok(CapturedPaneBoundary {
        boundary,
        seed: materialize(draft?)?,
        receiver,
    })
}

pub(in crate::handler) fn capture_surface_source(
    source: &PaneStreamSource,
    previous: &PaneSurfaceFingerprint,
    force: bool,
) -> Result<CapturedSurfaceBoundary, RmuxError> {
    let (boundary, captured, receiver) = source.output.capture_with_observer(|| {
        let transcript = source
            .transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dynamic_colors = PaneDynamicColors::capture(&transcript);
        let fingerprint = PaneSurfaceFingerprint::capture(transcript.screen(), &dynamic_colors);
        let seed =
            (force || &fingerprint != previous).then(|| PaneRecoveryDraft::capture(&transcript));
        (fingerprint, seed)
    });
    let seed = captured
        .1
        .transpose()?
        .map(PaneRecoveryDraft::materialize)
        .transpose()?;
    Ok(CapturedSurfaceBoundary {
        boundary,
        fingerprint: captured.0,
        seed,
        receiver,
    })
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
        coverage: PaneRecoveryCoverage {
            history_rows_total: keyframe.history_rows_total,
            history_rows_included: keyframe.history_rows_included,
            metadata_complete: keyframe.metadata_complete,
        },
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
    validate_recovery_snapshot_geometry(size.cols, size.rows)?;
    let history_size = seed.history_size();
    let history_bytes = seed.history_bytes();
    let cells = collect_cells(screen, size.cols, size.rows, history_size)?;
    let (hyperlinks, hyperlinks_complete) = collect_surface_hyperlinks(screen, &cells);
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
            hyperlinks,
            cursor,
            title: screen.title().to_owned(),
            path: screen.path().to_owned(),
            dynamic_colors: PaneSurfaceDynamicColors {
                foreground: seed.dynamic_colors().foreground.clone(),
                background: seed.dynamic_colors().background.clone(),
                cursor: seed.dynamic_colors().cursor.clone(),
            },
            metadata_complete: seed.metadata_complete() && hyperlinks_complete,
            mode_bits: screen.mode(),
            alternate: seed.alternate(),
            scroll_top,
            scroll_bottom,
            history_size: saturating_u64(history_size),
            history_bytes: saturating_u64(history_bytes),
            revision: grid_revision,
        },
    }))
}

fn collect_surface_hyperlinks(
    screen: &rmux_core::Screen,
    cells: &[rmux_proto::PaneSnapshotCell],
) -> (Vec<PaneSurfaceHyperlink>, bool) {
    let ids = cells
        .iter()
        .filter_map(|cell| (cell.link != 0).then_some(cell.link))
        .collect::<BTreeSet<_>>();
    let mut complete = true;
    let hyperlinks = ids
        .into_iter()
        .filter_map(|id| {
            let Some(uri) = screen.hyperlink_uri(id) else {
                complete = false;
                return None;
            };
            Some(PaneSurfaceHyperlink {
                id,
                uri: uri.to_owned(),
            })
        })
        .collect();
    (hyperlinks, complete)
}

fn materialize_typed_snapshot(
    handler: &RequestHandler,
    pane_id: PaneId,
    seed: &PaneRecoverySeed,
) -> Result<PaneSnapshotResponse, RmuxError> {
    let screen = seed.screen();
    let size = screen.size();
    validate_recovery_snapshot_geometry(size.cols, size.rows)?;
    let history_size = seed.history_size();
    let history_bytes = seed.history_bytes();
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

fn validate_recovery_snapshot_geometry(cols: u16, rows: u16) -> Result<(), RmuxError> {
    let cells = usize::from(cols)
        .checked_mul(usize::from(rows))
        .ok_or_else(|| RmuxError::Server("recovery snapshot dimensions overflow".to_owned()))?;
    if cells > MAX_RECOVERY_TYPED_SNAPSHOT_CELLS {
        return Err(RmuxError::Server(format!(
            "recovery snapshot grid has {cells} cells, exceeding the {MAX_RECOVERY_TYPED_SNAPSHOT_CELLS}-cell transport cap"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use rmux_core::events::{OutputCursorItem, PaneOutputSubscriptionKey};
    use rmux_core::PaneId;
    use rmux_proto::{PaneTarget, SessionName, TerminalSize};

    use super::{capture_source_with_materializer, PaneRecoveryDraft, PaneStreamSource};
    use crate::pane_io;
    use crate::pane_transcript::PaneTranscript;

    #[test]
    fn recovery_materialization_does_not_block_output_publication() {
        let session = SessionName::new("capture-lock-test").expect("valid session name");
        let output = pane_io::pane_output_channel();
        let transcript = PaneTranscript::shared(64, TerminalSize { cols: 80, rows: 24 });
        let source = PaneStreamSource {
            target: PaneTarget::new(session.clone(), 0),
            key: PaneOutputSubscriptionKey::new(session, PaneId::new(1)),
            output: output.clone(),
            transcript: transcript.clone(),
            generation: 0,
        };

        let (materializer_entered_tx, materializer_entered_rx) = mpsc::sync_channel(0);
        let (release_materializer_tx, release_materializer_rx) = mpsc::sync_channel(0);
        let capture = thread::spawn(move || {
            capture_source_with_materializer(&source, |draft: PaneRecoveryDraft| {
                materializer_entered_tx
                    .send(())
                    .expect("test must observe materializer entry");
                release_materializer_rx
                    .recv()
                    .expect("test must release materializer");
                draft.materialize()
            })
        });

        materializer_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("draft capture must reach out-of-lock materialization");

        let (published_tx, published_rx) = mpsc::sync_channel(1);
        let publisher = thread::spawn(move || {
            pane_io::publish_pane_bytes_for_test(&transcript, &output, b"after".to_vec());
            published_tx
                .send(())
                .expect("publication completion must be observable");
        });

        if let Err(error) = published_rx.recv_timeout(Duration::from_secs(2)) {
            let _ = release_materializer_tx.send(());
            let _ = publisher.join();
            let _ = capture.join();
            panic!("output publication remained blocked during materialization: {error}");
        }
        release_materializer_tx
            .send(())
            .expect("materializer must still be waiting");

        publisher.join().expect("publisher thread must not panic");
        let mut captured = capture
            .join()
            .expect("capture thread must not panic")
            .expect("capture must succeed");
        assert_eq!(captured.boundary.next_output_sequence, 0);
        assert_eq!(captured.seed.output_sequence(), 0);

        let Some(OutputCursorItem::Event(event)) = captured.receiver.try_recv() else {
            panic!("receiver must observe output published after its capture boundary");
        };
        assert_eq!(event.sequence(), 0);
        assert_eq!(event.bytes(), b"after");
        assert!(
            captured.receiver.try_recv().is_none(),
            "captured receiver must not duplicate the post-boundary event"
        );
    }
}
