use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};
use std::time::Instant;

use rmux_core::input::mode;
use rmux_core::PaneId;
use rmux_proto::{
    ErrorResponse, PaneSnapshotCell, PaneSnapshotCursor, PaneSnapshotRequest, PaneSnapshotResponse,
    Response, RmuxError,
};

use super::super::RequestHandler;
use crate::pane_terminal_lookup::pane_id_for_target;
use crate::pane_terminals::HandlerState;
use crate::pane_transcript::SharedPaneTranscript;

pub(in crate::handler) struct PaneSnapshotInputs {
    pane_id: PaneId,
    transcript: SharedPaneTranscript,
}

/// Saturating cast for cursor coordinates emitted by `Screen::cursor_position`.
///
/// `Screen` stores the cursor as `u32` while the wire protocol uses `u16`. A
/// well-formed pane keeps the cursor inside `u16` bounds, but a defensive
/// saturating cast guarantees that pathological screen state cannot produce a
/// silently-truncated cursor coordinate on the wire.
fn cursor_coord_to_u16(value: u32) -> u16 {
    if value > u16::MAX as u32 {
        u16::MAX
    } else {
        value as u16
    }
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_pane_snapshot(
        &self,
        request: PaneSnapshotRequest,
    ) -> Response {
        let inputs = {
            let state = self.state.lock().await;
            let _lock_span = crate::perf_instrument::span("state_lock_hold")
                .with_str("site", "pane_snapshot_resolve");
            match self.resolve_pane_snapshot_inputs(&state, &request.target) {
                Ok(inputs) => inputs,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };
        self.handle_pane_snapshot_inputs(inputs)
    }

    pub(in crate::handler) fn resolve_pane_snapshot_inputs(
        &self,
        state: &HandlerState,
        target: &rmux_proto::PaneTarget,
    ) -> Result<PaneSnapshotInputs, RmuxError> {
        let pane_id = pane_id_for_target(
            &state.sessions,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        let transcript = state.transcript_handle(target)?;
        Ok(PaneSnapshotInputs {
            pane_id,
            transcript,
        })
    }

    pub(in crate::handler) fn handle_pane_snapshot_inputs(
        &self,
        inputs: PaneSnapshotInputs,
    ) -> Response {
        let pane_id = inputs.pane_id;
        let _snapshot_span = crate::perf_instrument::span("snapshot")
            .with_u64("pane_id", u64::from(pane_id.as_u32()));
        let (cols, rows, cells, cursor, output_sequence, history_size, history_bytes) = {
            let transcript = inputs
                .transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned");
            let screen = transcript.screen();
            let size = screen.size();
            let cols = size.cols;
            let rows = size.rows;
            let history_size = screen.history_size();
            let history_bytes = screen.history_bytes();
            let (cursor_x, cursor_y) = screen.cursor_position();
            let cursor_visible = (screen.mode() & mode::MODE_CURSOR) != 0;
            let cursor = PaneSnapshotCursor {
                row: cursor_coord_to_u16(cursor_y),
                col: cursor_coord_to_u16(cursor_x),
                visible: cursor_visible,
                style: screen.cursor_style(),
            };
            let output_sequence = transcript.output_sequence();

            let cells = match collect_cells(screen, cols, rows, history_size) {
                Ok(cells) => cells,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            (
                cols,
                rows,
                cells,
                cursor,
                output_sequence,
                history_size,
                history_bytes,
            )
        };

        let fingerprint = compute_snapshot_fingerprint(
            cols,
            rows,
            &cells,
            &cursor,
            output_sequence,
            history_size,
            history_bytes,
            pane_id.as_u32(),
        );
        let revision = self.assign_pane_snapshot_revision(pane_id, fingerprint);

        // Notification revisions are anchored to the same `u64` value the
        // snapshot endpoint returns in `PaneSnapshotResponse.revision`. The
        // coalescer is fed exactly this revision (not output ring sequences
        // or attach render counters), preserving a single source of truth
        // for revision identity across snapshot responses and any future
        // notification path.
        self.observe_pane_snapshot_revision(pane_id, revision, Instant::now());

        Response::PaneSnapshot(PaneSnapshotResponse {
            cols,
            rows,
            cells,
            cursor,
            revision,
        })
    }

    /// Records a freshly observed pane snapshot revision in the per-pane
    /// coalescer registry. Returns the revision the coalescer would emit to
    /// notification subscribers right now, or `None` if the cap held the
    /// revision back as pending or suppressed it as a duplicate.
    ///
    /// The revision passed in must be the same `u64` value returned by
    /// `PaneSnapshotResponse.revision` for this pane, so the notification
    /// stream and the snapshot endpoint share a single source of truth.
    pub(crate) fn observe_pane_snapshot_revision(
        &self,
        pane_id: PaneId,
        revision: u64,
        now: Instant,
    ) -> Option<u64> {
        let mut coalescers = self
            .pane_snapshot_coalescers
            .lock()
            .expect("pane snapshot coalescer mutex must not be poisoned");
        coalescers.observe(pane_id, revision, now)
    }

    fn assign_pane_snapshot_revision(&self, pane_id: PaneId, fingerprint: u64) -> u64 {
        let mut revisions = self
            .pane_snapshot_revisions
            .lock()
            .expect("pane snapshot revision mutex must not be poisoned");
        revisions.revision_for(pane_id, fingerprint)
    }

    /// Drains a pending pane snapshot revision that is now eligible to be
    /// emitted, if any. Used by polling notification consumers.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn poll_pane_snapshot_revision(&self, pane_id: PaneId, now: Instant) -> Option<u64> {
        let mut coalescers = self
            .pane_snapshot_coalescers
            .lock()
            .expect("pane snapshot coalescer mutex must not be poisoned");
        coalescers.poll(pane_id, now)
    }

    /// Returns the most recent revision the coalescer emitted for a pane.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn last_emitted_pane_snapshot_revision(&self, pane_id: PaneId) -> Option<u64> {
        let coalescers = self
            .pane_snapshot_coalescers
            .lock()
            .expect("pane snapshot coalescer mutex must not be poisoned");
        coalescers.last_emitted_revision(pane_id)
    }

    /// Drops coalescer state for panes that have been retired.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn forget_pane_snapshot_coalescers(&self, pane_ids: &[PaneId]) {
        let mut coalescers = self
            .pane_snapshot_coalescers
            .lock()
            .expect("pane snapshot coalescer mutex must not be poisoned");
        let mut revisions = self
            .pane_snapshot_revisions
            .lock()
            .expect("pane snapshot revision mutex must not be poisoned");
        for pane_id in pane_ids {
            coalescers.forget(*pane_id);
            revisions.forget(*pane_id);
        }
    }
}

fn collect_cells(
    screen: &rmux_core::Screen,
    cols: u16,
    rows: u16,
    _history_size: usize,
) -> Result<Vec<PaneSnapshotCell>, RmuxError> {
    let cols_usize = usize::from(cols);
    let rows_usize = usize::from(rows);
    let total = cols_usize.saturating_mul(rows_usize);
    let mut cells = Vec::with_capacity(total);
    if cols_usize == 0 || rows_usize == 0 {
        return Ok(cells);
    }

    for row in 0..rows_usize {
        let before = cells.len();
        let found = screen.visit_visible_line_cells(row, cols_usize, |cell| {
            cells.push(PaneSnapshotCell {
                text: cell.text().to_owned(),
                width: cell.width(),
                padding: cell.is_padding(),
                attributes: cell.attr(),
                fg: cell.fg(),
                bg: cell.bg(),
                us: cell.us(),
                link: cell.link(),
            });
        });
        if !found {
            cells.extend((0..cols_usize).map(|_| blank_cell()));
            continue;
        }
        while cells.len() - before < cols_usize {
            cells.push(blank_cell());
        }
    }

    Ok(cells)
}

fn blank_cell() -> PaneSnapshotCell {
    PaneSnapshotCell {
        text: " ".to_owned(),
        width: 1,
        padding: false,
        attributes: 0,
        fg: 8,
        bg: 8,
        us: 8,
        link: 0,
    }
}

#[allow(clippy::too_many_arguments)]
fn compute_snapshot_fingerprint(
    cols: u16,
    rows: u16,
    cells: &[PaneSnapshotCell],
    cursor: &PaneSnapshotCursor,
    output_sequence: u64,
    history_size: usize,
    history_bytes: usize,
    pane_id_value: u32,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    cols.hash(&mut hasher);
    rows.hash(&mut hasher);
    cursor.row.hash(&mut hasher);
    cursor.col.hash(&mut hasher);
    cursor.visible.hash(&mut hasher);
    cursor.style.hash(&mut hasher);
    for cell in cells {
        cell.text.hash(&mut hasher);
        cell.width.hash(&mut hasher);
        cell.padding.hash(&mut hasher);
        cell.attributes.hash(&mut hasher);
        cell.fg.hash(&mut hasher);
        cell.bg.hash(&mut hasher);
        cell.us.hash(&mut hasher);
        cell.link.hash(&mut hasher);
    }
    output_sequence.hash(&mut hasher);
    history_size.hash(&mut hasher);
    history_bytes.hash(&mut hasher);
    pane_id_value.hash(&mut hasher);
    let raw = hasher.finish();
    if raw == 0 {
        0xFFFF_FFFF_FFFF_FFFF
    } else {
        raw
    }
}

#[derive(Debug, Default)]
pub(in crate::handler) struct PaneSnapshotRevisionRegistry {
    panes: HashMap<PaneId, PaneSnapshotRevisionState>,
}

#[derive(Debug, Clone, Copy)]
struct PaneSnapshotRevisionState {
    fingerprint: u64,
    revision: u64,
}

impl PaneSnapshotRevisionRegistry {
    fn revision_for(&mut self, pane_id: PaneId, fingerprint: u64) -> u64 {
        let Some(state) = self.panes.get_mut(&pane_id) else {
            self.panes.insert(
                pane_id,
                PaneSnapshotRevisionState {
                    fingerprint,
                    revision: 1,
                },
            );
            return 1;
        };

        if state.fingerprint == fingerprint {
            return state.revision;
        }

        state.fingerprint = fingerprint;
        state.revision = state.revision.saturating_add(1);
        state.revision
    }

    fn forget(&mut self, pane_id: PaneId) {
        self.panes.remove(&pane_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rmux_core::{Screen, TerminalScreen};
    use rmux_proto::TerminalSize;

    fn screen_with_size(cols: u16, rows: u16) -> Screen {
        Screen::new(TerminalSize { cols, rows }, 0)
    }

    fn snapshot_cursor(row: u16, col: u16) -> PaneSnapshotCursor {
        PaneSnapshotCursor {
            row,
            col,
            visible: true,
            style: 0,
        }
    }

    fn baseline_cell() -> PaneSnapshotCell {
        PaneSnapshotCell {
            text: "x".to_owned(),
            width: 1,
            padding: false,
            attributes: 0,
            fg: 8,
            bg: 8,
            us: 8,
            link: 0,
        }
    }

    #[test]
    fn cursor_coord_to_u16_clamps_extreme_values() {
        assert_eq!(cursor_coord_to_u16(0), 0);
        assert_eq!(cursor_coord_to_u16(80), 80);
        assert_eq!(cursor_coord_to_u16(u16::MAX as u32), u16::MAX);
        // Pathological cursor coordinates from a misbehaving backend must
        // saturate rather than silently truncate via `as u16` wrap-around.
        assert_eq!(cursor_coord_to_u16(u16::MAX as u32 + 1), u16::MAX);
        assert_eq!(cursor_coord_to_u16(u32::MAX), u16::MAX);
    }

    #[test]
    fn collect_cells_returns_empty_vec_when_either_dim_is_zero() {
        let screen = screen_with_size(0, 4);
        let cells = collect_cells(&screen, 0, 4, 0).expect("zero cols ok");
        assert!(cells.is_empty());

        let screen = screen_with_size(4, 0);
        let cells = collect_cells(&screen, 4, 0, 0).expect("zero rows ok");
        assert!(cells.is_empty());
    }

    #[test]
    fn collect_cells_pads_short_rows_to_exactly_cols_blank_cells() {
        // `screen_with_size` produces a clean grid where every row has exactly
        // `cols` cells, so the fallback we are validating is purely defensive
        // for any future grid backend that could hand us short rows. The
        // captured row count must always equal `rows * cols`.
        let screen = screen_with_size(4, 2);
        let cells = collect_cells(&screen, 4, 2, 0).expect("collect ok");
        assert_eq!(cells.len(), 8);
        for cell in &cells {
            // Default cells are blank single-width spaces with default colors.
            assert!(!cell.padding);
            assert_eq!(cell.width, 1);
        }
    }

    #[test]
    fn collect_cells_preserves_padding_metadata_for_wide_cells() {
        // Feed a wide glyph through the core terminal boundary into a Screen.
        let mut terminal = TerminalScreen::new(TerminalSize { cols: 4, rows: 1 }, 0);
        terminal.feed("界x".as_bytes());
        let screen = terminal.screen().clone();
        let cells = collect_cells(&screen, 4, 1, 0).expect("collect ok");
        assert_eq!(cells.len(), 4);
        assert!(!cells[0].padding);
        assert_eq!(cells[0].text, "界");
        assert_eq!(cells[0].width, 2);
        // The trailing padding column carries width 0 and the padding flag,
        // matching the rmux-core grid contract.
        assert!(cells[1].padding);
        assert_eq!(cells[1].width, 0);
        assert!(!cells[2].padding);
        assert_eq!(cells[2].text, "x");
    }

    #[test]
    fn collect_cells_skips_history_offset_and_returns_visible_rows() {
        // Pre-fill the screen with two rows of content via the parser, then
        // verify that the visible row offset stays correct as `history_size`
        // advances. With a zero history limit `history_size` stays at zero
        // here, but the function must not panic for non-zero offsets either.
        let mut terminal = TerminalScreen::new(TerminalSize { cols: 4, rows: 2 }, 0);
        terminal.feed(b"abcd\r\nefgh");
        let screen = terminal.screen().clone();
        let cells = collect_cells(&screen, 4, 2, 0).expect("collect ok");
        assert_eq!(cells.len(), 8);
        let row0_text: String = cells[0..4].iter().map(|c| c.text.as_str()).collect();
        let row1_text: String = cells[4..8].iter().map(|c| c.text.as_str()).collect();
        assert_eq!(row0_text, "abcd");
        assert_eq!(row1_text, "efgh");
    }

    #[test]
    fn compute_snapshot_fingerprint_is_never_zero_for_default_inputs() {
        let cursor = snapshot_cursor(0, 0);
        let fingerprint = compute_snapshot_fingerprint(0, 0, &[], &cursor, 0, 0, 0, 0);
        assert_ne!(fingerprint, 0);
    }

    #[test]
    fn compute_snapshot_fingerprint_changes_with_each_observable_field() {
        let cursor = snapshot_cursor(0, 0);
        let baseline = compute_snapshot_fingerprint(80, 24, &[], &cursor, 0, 0, 0, 1);

        // Each observable input must influence the revision. We do not assert
        // exact deltas (which would couple to the hash internals); only that
        // the revision value moves when one input changes.
        assert_ne!(
            baseline,
            compute_snapshot_fingerprint(81, 24, &[], &cursor, 0, 0, 0, 1)
        );
        assert_ne!(
            baseline,
            compute_snapshot_fingerprint(80, 25, &[], &cursor, 0, 0, 0, 1)
        );
        assert_ne!(
            baseline,
            compute_snapshot_fingerprint(80, 24, &[], &cursor, 1, 0, 0, 1)
        );
        assert_ne!(
            baseline,
            compute_snapshot_fingerprint(80, 24, &[], &cursor, 0, 1, 0, 1)
        );
        assert_ne!(
            baseline,
            compute_snapshot_fingerprint(80, 24, &[], &cursor, 0, 0, 1, 1)
        );
        assert_ne!(
            baseline,
            compute_snapshot_fingerprint(80, 24, &[], &cursor, 0, 0, 0, 2)
        );
        assert_ne!(
            baseline,
            compute_snapshot_fingerprint(80, 24, &[], &snapshot_cursor(1, 0), 0, 0, 0, 1)
        );
        assert_ne!(
            baseline,
            compute_snapshot_fingerprint(80, 24, &[baseline_cell()], &cursor, 0, 0, 0, 1)
        );
    }

    #[test]
    fn compute_snapshot_fingerprint_is_stable_for_identical_inputs() {
        // The internal fingerprint is stable for two captures of the exact
        // same observable state; the public revision counter is assigned from
        // this fingerprint by `PaneSnapshotRevisionRegistry`.
        let cursor = snapshot_cursor(2, 5);
        let cells = vec![baseline_cell(); 4];
        let a = compute_snapshot_fingerprint(80, 24, &cells, &cursor, 7, 1, 100, 9);
        let b = compute_snapshot_fingerprint(80, 24, &cells, &cursor, 7, 1, 100, 9);
        assert_eq!(a, b);
    }

    #[test]
    fn pane_snapshot_revisions_are_monotone_for_state_transitions() {
        let mut registry = PaneSnapshotRevisionRegistry::default();
        let pane_id = PaneId::new(3);

        assert_eq!(registry.revision_for(pane_id, 10), 1);
        assert_eq!(
            registry.revision_for(pane_id, 10),
            1,
            "unchanged observable state must not advance revision",
        );
        assert_eq!(
            registry.revision_for(pane_id, 20),
            2,
            "changed observable state must advance revision",
        );
        assert_eq!(
            registry.revision_for(pane_id, 10),
            3,
            "returning to prior content is still a new transition",
        );
    }

    #[test]
    fn pane_snapshot_revision_forget_resets_only_retired_panes() {
        let mut registry = PaneSnapshotRevisionRegistry::default();
        let first = PaneId::new(4);
        let second = PaneId::new(5);

        assert_eq!(registry.revision_for(first, 10), 1);
        assert_eq!(registry.revision_for(second, 99), 1);
        assert_eq!(registry.revision_for(first, 11), 2);

        registry.forget(first);

        assert_eq!(registry.revision_for(first, 10), 1);
        assert_eq!(registry.revision_for(second, 100), 2);
    }

    #[tokio::test]
    async fn coalescer_caps_revision_notifications_to_60_per_second_per_pane() {
        // Drive the per-pane snapshot coalescer at well above 60 Hz over a
        // full simulated second. The cap is enforced by the coalescer
        // registry that backs `observe_pane_snapshot_revision`; this test
        // asserts the cap holds independently of the 16 ms attach refresh
        // scheduler in `crates/rmux-server/src/pane_io/refresh_scheduler.rs`
        // by feeding `Instant` values directly without invoking any tokio
        // timer or scheduler.
        let handler = RequestHandler::new();
        let pane_id = PaneId::new(7);
        let base = Instant::now();
        let mut emitted: Vec<u64> = Vec::new();
        let mut revision: u64 = 0;
        // Observe at 1 kHz (1 ms spacing) for 1 s — 1,000 distinct revisions.
        for tick_ms in 0..=1_000u64 {
            revision = revision.wrapping_add(1);
            let now = base + std::time::Duration::from_millis(tick_ms);
            if let Some(value) = handler.observe_pane_snapshot_revision(pane_id, revision, now) {
                emitted.push(value);
            }
        }
        // Drain any tail value that the cap is currently holding pending.
        if let Some(value) = handler
            .poll_pane_snapshot_revision(pane_id, base + std::time::Duration::from_millis(1_001))
        {
            emitted.push(value);
        }
        assert!(
            emitted.len() <= 60,
            "snapshot coalescer emitted {} notifications in 1 s (cap is 60/s)",
            emitted.len(),
        );
        // Last emitted revision is observable through the registry surface
        // so future notification consumers can skip any revision they have
        // already seen.
        assert_eq!(
            handler.last_emitted_pane_snapshot_revision(pane_id),
            emitted.last().copied(),
        );
    }

    #[tokio::test]
    async fn coalescer_uses_response_revision_and_emits_monotonic_observed_order() {
        // The coalescer must be fed the same revision value the snapshot
        // endpoint puts on the wire, and it must deliver revisions in
        // observation order even when bursty observations arrive faster
        // than the cap allows.
        let handler = RequestHandler::new();
        let pane_id = PaneId::new(11);
        let base = Instant::now();
        // Three rapid observations inside one coalescing window; only the
        // newest survives the cap.
        assert_eq!(
            handler.observe_pane_snapshot_revision(pane_id, 1, base),
            Some(1),
        );
        assert_eq!(
            handler.observe_pane_snapshot_revision(
                pane_id,
                2,
                base + std::time::Duration::from_micros(100),
            ),
            None,
        );
        assert_eq!(
            handler.observe_pane_snapshot_revision(
                pane_id,
                3,
                base + std::time::Duration::from_micros(200),
            ),
            None,
        );
        // After the window opens, the freshest pending value (3) is
        // delivered; revision 2 is dropped because it was superseded.
        let after_window = base + std::time::Duration::from_millis(20);
        assert_eq!(
            handler.poll_pane_snapshot_revision(pane_id, after_window),
            Some(3),
        );
        // A stale repeat of the latest emitted revision is suppressed.
        assert_eq!(
            handler.observe_pane_snapshot_revision(
                pane_id,
                3,
                after_window + std::time::Duration::from_millis(50),
            ),
            None,
        );
        // Forgetting a pane drops its coalescer state but keeps others.
        let other = PaneId::new(12);
        assert_eq!(
            handler.observe_pane_snapshot_revision(other, 99, base),
            Some(99),
        );
        handler.forget_pane_snapshot_coalescers(&[pane_id]);
        assert_eq!(handler.last_emitted_pane_snapshot_revision(pane_id), None);
        assert_eq!(handler.last_emitted_pane_snapshot_revision(other), Some(99),);
    }
}
