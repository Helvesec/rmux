use std::collections::VecDeque;
use std::ops::Range;

use rmux_core::input::{mode, OscColourSlot};
use rmux_core::{
    render_dec_modes_for_snapshot, GridRenderOptions, RecoveryRow, RecoveryRowRenderer, Screen,
};
use rmux_proto::{RmuxError, DEFAULT_MAX_DETACHED_FRAME_LENGTH, DEFAULT_MAX_FRAME_LENGTH};

use crate::pane_transcript::PaneTranscript;

// Rows are painted back to back and rely on the terminal's own wrap to place a
// soft-wrapped row's successor, so the paint enables DECAWM before the first
// row. The pane's own mode is restored absolutely once the rows are painted.
const RESET_PREFIX: &[u8] =
    b"\x1b[?2026l\x1b[?1049l\x1b[?6l\x1b[?7h\x1b[r\x1b[0m\x1b]8;;\x1b\\\x1b[?25l\x1b[2J\x1b[3J\x1b[H";
const ALT_SCREEN_PREFIX: &[u8] =
    b"\x1b[?1049h\x1b[?6l\x1b[?7h\x1b[r\x1b[0m\x1b]8;;\x1b\\\x1b[?25l\x1b[2J\x1b[H";
const ALT_SCREEN_NO_CURSOR_PREFIX: &[u8] =
    b"\x1b[?47h\x1b[?6l\x1b[?7h\x1b[r\x1b[0m\x1b]8;;\x1b\\\x1b[?25l\x1b[2J\x1b[H";
const RESET_RENDITION: &[u8] = b"\x1b[0m\x1b]8;;\x1b\\";
const ROW_RESET: &[u8] = b"\x1b[0m\x1b]8;;\x1b\\";
const LINE_BREAK: &[u8] = b"\r\n";

/// A pane-scoped WebShare snapshot shares the two-MiB outbound budget with
/// its opcode and sanitizer state. The sanitizer never expands its input, so
/// 4 KiB leaves bounded framing headroom while retaining the largest common
/// typed viewport.
pub(crate) const MAX_RECOVERY_KEYFRAME_BYTES: usize = 2 * DEFAULT_MAX_FRAME_LENGTH - 4 * 1024;
const MAX_RECOVERY_VIEWPORT_CELLS: usize = 128 * 1024;
const MAX_RECOVERY_COLS: usize = 4096;
const MAX_RECOVERY_ROWS: usize = 2048;
pub(crate) const MAX_RECOVERY_STRING_BYTES: usize = 64 * 1024;
pub(crate) const MAX_RECOVERY_TITLE_STACK_BYTES: usize = 128 * 1024;
pub(crate) const MAX_RECOVERY_HYPERLINK_ENTRY_BYTES: usize = 512;
pub(crate) const MAX_RECOVERY_HYPERLINK_TOTAL_BYTES: usize = 128 * 1024;
const MAX_RECOVERY_DRAFT_HISTORY_BYTES: usize = MAX_RECOVERY_KEYFRAME_BYTES;
// Cell text is capped by rmux-core at 21 bytes. Ninety-six thousand cells
// leave more than two MiB of detached-frame headroom for a recovery keyframe,
// bincode headers and lifecycle companions.
pub(crate) const MAX_RECOVERY_TYPED_SNAPSHOT_CELLS: usize = 96 * 1024;
const MAX_RECOVERY_SURFACE_CELLS: usize = DEFAULT_MAX_DETACHED_FRAME_LENGTH / 32;

/// Owned terminal state copied at an atomic pane boundary.
pub(crate) struct PaneRecoverySeed {
    projection: PaneProjectionSeed,
    keyframe: PaneRecoveryKeyframe,
}

/// Transport-bounded logical pane state shared by structured projections.
pub(crate) struct PaneProjectionSeed {
    screen: Screen,
    dynamic_colors: PaneDynamicColors,
    metadata_complete: bool,
    history_size: usize,
    history_bytes: usize,
    alternate: bool,
    output_sequence: u64,
}

/// Structurally bounded terminal state copied at the atomic output boundary.
///
/// ANSI rendering happens only after the output-state and transcript locks
/// have been released.
pub(crate) struct PaneRecoveryDraft {
    projection: Screen,
    dynamic_colors: PaneDynamicColors,
    metadata_complete: bool,
    pending_bytes: Vec<u8>,
    active_cell_state: Vec<u8>,
    saved_cell_state: Vec<u8>,
    saved_cursor: (u32, u32, bool),
    parser_state: Vec<u8>,
    history_size: usize,
    history_bytes: usize,
    output_sequence: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct PaneDynamicColors {
    pub(crate) foreground: Option<String>,
    pub(crate) background: Option<String>,
    pub(crate) cursor: Option<String>,
}

impl PaneDynamicColors {
    pub(crate) fn capture(transcript: &PaneTranscript) -> Self {
        Self {
            foreground: transcript
                .dynamic_colour(OscColourSlot::Foreground)
                .map(str::to_owned),
            background: transcript
                .dynamic_colour(OscColourSlot::Background)
                .map(str::to_owned),
            cursor: transcript
                .dynamic_colour(OscColourSlot::Cursor)
                .map(str::to_owned),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneRecoveryKeyframe {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) bytes: Vec<u8>,
    pub(crate) alternate: bool,
    pub(crate) history_rows_total: u64,
    pub(crate) history_rows_included: u64,
    pub(crate) metadata_complete: bool,
}

/// One row of the keyframe, with the columns its bytes paint on replay.
#[derive(Debug)]
struct RenderedRow {
    bytes: Vec<u8>,
    wrapped: bool,
    columns: u32,
    leading_columns: u32,
    trailing_reflow_gap: u32,
}

impl RenderedRow {
    fn new(row: RecoveryRow) -> Self {
        Self {
            bytes: row.bytes,
            wrapped: row.wrapped,
            columns: row.span.columns(),
            leading_columns: row.span.leading_columns(),
            trailing_reflow_gap: row.trailing_reflow_gap,
        }
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
    }

    /// Returns the most bytes [`fill_to_width`](Self::fill_to_width) can add to
    /// this row while it is being linked to its neighbours.
    fn reserved_fill_len(&self, cols: u32) -> usize {
        let missing = cols.saturating_sub(self.columns);
        if missing == 0 || !(self.wrapped || self.leading_columns == 0) {
            return 0;
        }
        RESET_RENDITION.len().saturating_add(missing as usize)
    }
}

/// How much scrolled-off scrollback a recovery keyframe is allowed to replay.
///
/// The keyframe always carries the pane's *visible* state (main viewport, plus
/// the saved main viewport when the pane is on the alternate screen). Scrolled-
/// off history is extra, and only some consumers are entitled to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryHistoryPolicy {
    /// Replay the newest scrollback that fits the keyframe budget. Used by the
    /// owner-authenticated SDK raw pane streams, whose contract is the pane's
    /// byte history and which report the achieved coverage to the caller.
    RecentHistory,
    /// Replay the visible state only. Used by the WebShare pane surface: that
    /// link is a read-only *view* handed to third parties, pane scope has no
    /// scrollback protocol at all (`PaneScroll` closes the socket with
    /// `scroll_requires_session`), so rows that left the pane before the share
    /// existed must never reach a viewer's browser.
    VisibleOnly,
}

impl RecoveryHistoryPolicy {
    fn history_budget(self, keyframe_headroom: usize) -> usize {
        match self {
            Self::RecentHistory => keyframe_headroom,
            Self::VisibleOnly => 0,
        }
    }
}

impl PaneRecoveryDraft {
    pub(crate) fn capture(transcript: &PaneTranscript) -> Result<Self, RmuxError> {
        let source = transcript.screen();
        validate_recovery_geometry(source)?;
        if transcript.pending_bytes_ref().len() > MAX_RECOVERY_KEYFRAME_BYTES {
            return Err(RmuxError::FrameTooLarge {
                length: transcript.pending_bytes_ref().len(),
                maximum: MAX_RECOVERY_KEYFRAME_BYTES,
            });
        }

        let (projection, viewport_metadata_complete) = source.clone_recovery_projection_bounded(
            MAX_RECOVERY_STRING_BYTES,
            MAX_RECOVERY_TITLE_STACK_BYTES,
            MAX_RECOVERY_HYPERLINK_ENTRY_BYTES,
            MAX_RECOVERY_HYPERLINK_TOTAL_BYTES,
            MAX_RECOVERY_DRAFT_HISTORY_BYTES,
        );
        let (active_cell_state, active_cell_complete) =
            transcript.active_cell_state_ansi_bounded(MAX_RECOVERY_HYPERLINK_ENTRY_BYTES);
        let (saved_cell_state, saved_cell_complete) =
            transcript.saved_cell_state_ansi_bounded(MAX_RECOVERY_HYPERLINK_ENTRY_BYTES);
        Ok(Self {
            projection,
            dynamic_colors: PaneDynamicColors::capture(transcript),
            metadata_complete: viewport_metadata_complete
                && active_cell_complete
                && saved_cell_complete,
            pending_bytes: transcript.pending_bytes_ref().to_vec(),
            active_cell_state,
            saved_cell_state,
            saved_cursor: transcript.saved_cursor_state(),
            parser_state: transcript.recovery_parser_state_ansi(),
            history_size: source.history_size(),
            history_bytes: source.history_bytes(),
            output_sequence: transcript.output_sequence(),
        })
    }

    pub(crate) fn materialize(self) -> Result<PaneRecoverySeed, RmuxError> {
        self.materialize_with_history(RecoveryHistoryPolicy::RecentHistory)
    }

    /// Materialize a keyframe that replays the pane's visible state only.
    ///
    /// This is the WebShare pane-surface entry point: it must stay the only
    /// materializer reachable from a shared link.
    pub(crate) fn materialize_visible_only(self) -> Result<PaneRecoverySeed, RmuxError> {
        self.materialize_with_history(RecoveryHistoryPolicy::VisibleOnly)
    }

    fn materialize_with_history(
        self,
        history_policy: RecoveryHistoryPolicy,
    ) -> Result<PaneRecoverySeed, RmuxError> {
        let keyframe = {
            let renderer = self.projection.recovery_row_renderer(
                MAX_RECOVERY_HYPERLINK_ENTRY_BYTES,
                MAX_RECOVERY_HYPERLINK_TOTAL_BYTES,
            );
            let rendered = materialize_keyframe(
                &self.projection,
                &renderer,
                &self.pending_bytes,
                &self.active_cell_state,
                &self.saved_cell_state,
                self.saved_cursor,
                &self.parser_state,
                self.history_size,
                self.metadata_complete && renderer.metadata_complete(),
                history_policy,
            );
            match rendered {
                Err(RmuxError::FrameTooLarge { .. }) => {
                    let renderer = self.projection.recovery_row_renderer(0, 0);
                    materialize_keyframe(
                        &self.projection,
                        &renderer,
                        &self.pending_bytes,
                        &self.active_cell_state,
                        &self.saved_cell_state,
                        self.saved_cursor,
                        &self.parser_state,
                        self.history_size,
                        false,
                        history_policy,
                    )
                }
                result => result,
            }
        }?;
        Ok(PaneRecoverySeed {
            projection: self.into_projection(),
            keyframe,
        })
    }

    fn into_projection(self) -> PaneProjectionSeed {
        let alternate = self.projection.is_alternate();
        let (screen, metadata_complete) = self.projection.clone_recovery_viewport_bounded(
            MAX_RECOVERY_STRING_BYTES,
            MAX_RECOVERY_TITLE_STACK_BYTES,
            MAX_RECOVERY_HYPERLINK_ENTRY_BYTES,
            MAX_RECOVERY_HYPERLINK_TOTAL_BYTES,
        );
        PaneProjectionSeed {
            screen,
            dynamic_colors: self.dynamic_colors,
            metadata_complete: self.metadata_complete && metadata_complete,
            history_size: self.history_size,
            history_bytes: self.history_bytes,
            alternate,
            output_sequence: self.output_sequence,
        }
    }
}

impl PaneRecoverySeed {
    #[cfg(test)]
    pub(crate) fn capture(transcript: &PaneTranscript) -> Result<Self, RmuxError> {
        PaneRecoveryDraft::capture(transcript)?.materialize()
    }

    pub(crate) const fn screen(&self) -> &Screen {
        self.projection.screen()
    }

    pub(crate) const fn output_sequence(&self) -> u64 {
        self.projection.output_sequence()
    }

    pub(crate) const fn history_size(&self) -> usize {
        self.projection.history_size()
    }

    pub(crate) const fn history_bytes(&self) -> usize {
        self.projection.history_bytes()
    }

    pub(crate) const fn alternate(&self) -> bool {
        self.projection.alternate()
    }

    pub(crate) fn keyframe(&self) -> PaneRecoveryKeyframe {
        self.keyframe.clone()
    }
}

impl PaneProjectionSeed {
    pub(crate) fn capture(transcript: &PaneTranscript) -> Result<Self, RmuxError> {
        let source = transcript.screen();
        validate_surface_geometry(source)?;
        let alternate = source.is_alternate();
        let (screen, metadata_complete) = source.clone_recovery_viewport_bounded(
            MAX_RECOVERY_STRING_BYTES,
            MAX_RECOVERY_TITLE_STACK_BYTES,
            MAX_RECOVERY_HYPERLINK_ENTRY_BYTES,
            MAX_RECOVERY_HYPERLINK_TOTAL_BYTES,
        );
        Ok(Self {
            screen,
            dynamic_colors: PaneDynamicColors::capture(transcript),
            metadata_complete,
            history_size: source.history_size(),
            history_bytes: source.history_bytes(),
            alternate,
            output_sequence: transcript.output_sequence(),
        })
    }

    pub(crate) const fn screen(&self) -> &Screen {
        &self.screen
    }

    pub(crate) const fn dynamic_colors(&self) -> &PaneDynamicColors {
        &self.dynamic_colors
    }

    pub(crate) const fn output_sequence(&self) -> u64 {
        self.output_sequence
    }

    pub(crate) const fn history_size(&self) -> usize {
        self.history_size
    }

    pub(crate) const fn history_bytes(&self) -> usize {
        self.history_bytes
    }

    pub(crate) const fn alternate(&self) -> bool {
        self.alternate
    }

    pub(crate) const fn metadata_complete(&self) -> bool {
        self.metadata_complete
    }
}

#[allow(clippy::too_many_arguments)]
fn materialize_keyframe(
    source: &Screen,
    renderer: &RecoveryRowRenderer<'_>,
    pending_bytes: &[u8],
    active_cell_state: &[u8],
    saved_cell_state: &[u8],
    saved_cursor: (u32, u32, bool),
    parser_state: &[u8],
    history_rows_total: usize,
    metadata_complete: bool,
    history_policy: RecoveryHistoryPolicy,
) -> Result<PaneRecoveryKeyframe, RmuxError> {
    let size = source.size();
    let cols = u32::from(size.cols);
    let alternate = source.is_alternate();
    let active_history_rows = source.history_size();
    let recovery_history_rows = renderer.recovery_history_size();
    let mut active_visible = render_active_rows(
        renderer,
        active_history_rows..active_history_rows.saturating_add(usize::from(size.rows)),
        cols,
    )?;
    link_rows(&mut active_visible, cols);

    let mut saved_visible = if alternate {
        let mut rows = render_saved_rows(renderer, 0..usize::from(size.rows), cols)?;
        link_rows(&mut rows, cols);
        Some(rows)
    } else {
        None
    };

    let mut prefix = Vec::new();
    prefix.extend_from_slice(RESET_PREFIX);
    append_title_state(&mut prefix, source);
    append_osc_text(&mut prefix, 7, source.path());
    prefix.extend_from_slice(parser_state);

    // The saved viewport is repainted on the main screen before the alternate
    // screen is entered, so its trailing cursor and mode bytes follow the rows.
    let mut alternate_tail = Vec::new();
    if let Some(saved_visible) = saved_visible.as_ref() {
        if let Some((x, y, pending_wrap)) = source.alternate_saved_cursor() {
            append_cursor_state(
                &mut alternate_tail,
                source,
                x,
                y,
                pending_wrap,
                Some(saved_visible),
                None,
            );
        }
        alternate_tail.extend_from_slice(if source.alternate_saved_cursor().is_some() {
            ALT_SCREEN_PREFIX
        } else {
            ALT_SCREEN_NO_CURSOR_PREFIX
        });
    }

    let mut suffix = Vec::new();
    append_scroll_region(&mut suffix, source);
    render_dec_modes_for_snapshot(source.mode(), source.cursor_style(), &mut suffix);
    append_tab_stops(&mut suffix, source);
    append_saved_decsc(&mut suffix, source, saved_cursor, saved_cell_state);
    append_active_cursor_state(&mut suffix, source, &active_visible, active_cell_state);
    suffix.extend_from_slice(pending_bytes);

    // Linking the history onto the first repainted row may still have to fill
    // that row, so its worst case is reserved before the history is sized.
    let first_repainted = saved_visible
        .as_ref()
        .map_or(active_visible.first(), |rows| rows.first());
    let mandatory_len = prefix
        .len()
        .saturating_add(
            saved_visible
                .as_ref()
                .map_or(0, |rows| encoded_rows_len(rows)),
        )
        .saturating_add(alternate_tail.len())
        .saturating_add(encoded_rows_len(&active_visible))
        .saturating_add(suffix.len())
        .saturating_add(first_repainted.map_or(0, |row| row.reserved_fill_len(cols)));
    if mandatory_len > MAX_RECOVERY_KEYFRAME_BYTES {
        return Err(RmuxError::FrameTooLarge {
            length: mandatory_len,
            maximum: MAX_RECOVERY_KEYFRAME_BYTES,
        });
    }

    // Both fixes land here. The share-viewer policy bounds how much history a
    // spectator may receive; the wrap fix needs `cols` and links the boundary
    // row pair so a soft-wrapped row is not glued onto the next one.
    let history_budget = history_policy.history_budget(MAX_RECOVERY_KEYFRAME_BYTES - mandatory_len);
    let mut history = recent_history_suffix(renderer, recovery_history_rows, history_budget, cols)?;
    let first_repainted = saved_visible
        .as_mut()
        .map_or(active_visible.first_mut(), |rows| rows.first_mut());
    let history_boundary = link_row_pair(history.back_mut(), first_repainted, cols);
    let history_len = encoded_rows_len_deque(&history).saturating_add(history_boundary.len());
    if history_len > history_budget {
        return Err(RmuxError::Server(
            "recovery history accounting exceeded its byte budget".to_owned(),
        ));
    }

    let mut bytes = Vec::with_capacity(mandatory_len.saturating_add(history_len));
    bytes.extend_from_slice(&prefix);
    append_rows_deque(&mut bytes, &history);
    bytes.extend_from_slice(history_boundary);
    if let Some(saved_visible) = saved_visible.as_ref() {
        append_rows(&mut bytes, saved_visible);
    }
    bytes.extend_from_slice(&alternate_tail);
    append_rows(&mut bytes, &active_visible);
    bytes.extend_from_slice(&suffix);
    debug_assert!(bytes.len() <= MAX_RECOVERY_KEYFRAME_BYTES);

    Ok(PaneRecoveryKeyframe {
        cols: size.cols,
        rows: size.rows,
        bytes,
        alternate,
        history_rows_total: u64::try_from(adjust_history_total(
            history_rows_total,
            active_history_rows,
            recovery_history_rows,
        ))
        .unwrap_or(u64::MAX),
        history_rows_included: u64::try_from(history.len()).unwrap_or(u64::MAX),
        metadata_complete,
    })
}

pub(crate) fn validate_recovery_geometry(screen: &Screen) -> Result<(), RmuxError> {
    let size = screen.size();
    let cols = usize::from(size.cols);
    let rows = usize::from(size.rows);
    let cells = cols
        .checked_mul(rows)
        .ok_or_else(|| RmuxError::Server("recovery viewport dimensions overflow".to_owned()))?;
    if cols == 0
        || rows == 0
        || cols > MAX_RECOVERY_COLS
        || rows > MAX_RECOVERY_ROWS
        || cells > MAX_RECOVERY_VIEWPORT_CELLS
    {
        return Err(RmuxError::Server(format!(
            "recovery viewport {cols}x{rows} exceeds the supported geometry cap ({MAX_RECOVERY_COLS} columns, {MAX_RECOVERY_ROWS} rows, {MAX_RECOVERY_VIEWPORT_CELLS} cells)"
        )));
    }
    Ok(())
}

fn validate_surface_geometry(screen: &Screen) -> Result<(), RmuxError> {
    let size = screen.size();
    let cells = usize::from(size.cols)
        .checked_mul(usize::from(size.rows))
        .ok_or_else(|| RmuxError::Server("surface dimensions overflow".to_owned()))?;
    if size.cols == 0 || size.rows == 0 || cells > MAX_RECOVERY_SURFACE_CELLS {
        return Err(RmuxError::Server(format!(
            "surface grid has {cells} cells, exceeding the {MAX_RECOVERY_SURFACE_CELLS}-cell transport cap"
        )));
    }
    Ok(())
}

fn render_active_rows(
    renderer: &RecoveryRowRenderer<'_>,
    range: Range<usize>,
    cols: u32,
) -> Result<Vec<RenderedRow>, RmuxError> {
    render_rows(range, cols, |row| {
        renderer.active_row(row, capture_options())
    })
}

fn render_saved_rows(
    renderer: &RecoveryRowRenderer<'_>,
    range: Range<usize>,
    cols: u32,
) -> Result<Vec<RenderedRow>, RmuxError> {
    render_rows(range, cols, |row| {
        renderer.saved_row(row, capture_options())
    })
}

/// Places every row of a repainted run at its own terminal line.
///
/// Rows are painted back to back, so the replaying terminal decides where a
/// row ends up: a soft-wrapped row hands the next row's first cell to the next
/// line only when that cell no longer fits in the columns the row leaves free.
/// [`link_row_pair`] restores that condition wherever the grid no longer
/// satisfies it, which happens whenever a wrapped row was shortened after it
/// wrapped (DCH, or a wide glyph that pre-wrapped and left a gap).
fn link_rows(rows: &mut [RenderedRow], cols: u32) {
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
fn link_row_pair(
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

fn render_rows(
    range: Range<usize>,
    cols: u32,
    mut render: impl FnMut(usize) -> Option<RecoveryRow>,
) -> Result<Vec<RenderedRow>, RmuxError> {
    let mut rows = Vec::with_capacity(range.len());
    let mut encoded_len = 0_usize;
    for row in range {
        let Some(rendered) = render(row) else {
            return Err(RmuxError::Server(format!(
                "terminal row {row} disappeared during recovery capture"
            )));
        };
        let rendered = RenderedRow::new(rendered);
        encoded_len = encoded_len
            .saturating_add(LINE_BREAK.len())
            .saturating_add(ROW_RESET.len())
            .saturating_add(rendered.bytes.len())
            .saturating_add(rendered.reserved_fill_len(cols));
        if encoded_len > MAX_RECOVERY_KEYFRAME_BYTES {
            return Err(RmuxError::FrameTooLarge {
                length: encoded_len,
                maximum: MAX_RECOVERY_KEYFRAME_BYTES,
            });
        }
        rows.push(rendered);
    }
    Ok(rows)
}

fn recent_history_suffix(
    renderer: &RecoveryRowRenderer<'_>,
    history_rows: usize,
    budget: usize,
    cols: u32,
) -> Result<VecDeque<RenderedRow>, RmuxError> {
    let mut retained: VecDeque<RenderedRow> = VecDeque::new();
    let mut retained_len = 0_usize;
    let mut end = history_rows;
    while end > 0 {
        let remaining = budget.saturating_sub(retained_len);
        let max_group_rows = remaining / ROW_RESET.len();
        if max_group_rows == 0 {
            break;
        }
        let mut start = end - 1;
        let mut group_rows = 1_usize;
        while start > 0 {
            let Some(previous_wrapped) = renderer.active_row_wrapped(start - 1) else {
                return Err(RmuxError::Server(
                    "terminal history changed during recovery capture".to_owned(),
                ));
            };
            if !previous_wrapped {
                break;
            }
            if group_rows == max_group_rows {
                // Every rendered row needs at least ROW_RESET. Once a single
                // logical line has more rows than can fit, no complete suffix
                // beginning with that newest line is representable.
                return Ok(retained);
            }
            start -= 1;
            group_rows += 1;
        }
        let Some((mut group, group_len)) =
            render_history_group_bounded(renderer, start..end, remaining, cols)?
        else {
            break;
        };
        link_rows(&mut group, cols);
        let _ = link_row_pair(group.last_mut(), retained.front_mut(), cols);
        for row in group.into_iter().rev() {
            retained.push_front(row);
        }
        retained_len = retained_len.saturating_add(group_len);
        end = start;
    }
    Ok(retained)
}

fn render_history_group_bounded(
    renderer: &RecoveryRowRenderer<'_>,
    range: Range<usize>,
    budget: usize,
    cols: u32,
) -> Result<Option<(Vec<RenderedRow>, usize)>, RmuxError> {
    let mut group = Vec::new();
    let mut encoded_len = 0_usize;
    for absolute_y in range {
        let Some(rendered) = renderer.recovery_history_row(absolute_y, capture_options()) else {
            return Err(RmuxError::Server(
                "terminal history changed during recovery capture".to_owned(),
            ));
        };
        let rendered = RenderedRow::new(rendered);
        let next_len = encoded_len
            .saturating_add(LINE_BREAK.len())
            .saturating_add(ROW_RESET.len())
            .saturating_add(rendered.bytes.len())
            .saturating_add(rendered.reserved_fill_len(cols));
        if next_len > budget {
            return Ok(None);
        }
        encoded_len = next_len;
        group.push(rendered);
    }
    encoded_len = encoded_len.saturating_add(LINE_BREAK.len());
    if encoded_len > budget {
        return Ok(None);
    }
    Ok(Some((group, encoded_len)))
}

fn adjust_history_total(total: usize, captured: usize, recovered: usize) -> usize {
    if recovered >= captured {
        total.saturating_add(recovered - captured)
    } else {
        total.saturating_sub(captured - recovered)
    }
}

fn append_title_state(out: &mut Vec<u8>, screen: &Screen) {
    for _ in 0..Screen::title_stack_limit() {
        out.extend_from_slice(b"\x1b[23;2t");
    }
    for title in screen.title_stack() {
        append_osc_text(out, 2, title);
        out.extend_from_slice(b"\x1b[22;2t");
    }
    append_osc_text(out, 2, screen.title());
}

fn append_osc_text(out: &mut Vec<u8>, command: u8, value: &str) {
    out.extend_from_slice(b"\x1b]");
    out.extend_from_slice(command.to_string().as_bytes());
    out.push(b';');
    for character in value.chars().filter(|character| !character.is_control()) {
        let mut encoded = [0_u8; 4];
        out.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
    }
    out.extend_from_slice(b"\x1b\\");
}

fn append_scroll_region(out: &mut Vec<u8>, screen: &Screen) {
    let (top, bottom) = screen.scroll_region();
    let default_bottom = u32::from(screen.size().rows.max(1)).saturating_sub(1);
    if top != 0 || bottom != default_bottom {
        out.extend_from_slice(
            format!(
                "\x1b[{};{}r",
                top.saturating_add(1),
                bottom.saturating_add(1)
            )
            .as_bytes(),
        );
    }
}

fn append_tab_stops(out: &mut Vec<u8>, screen: &Screen) {
    out.extend_from_slice(b"\x1b[3g");
    for (column, enabled) in screen.tab_stops().iter().copied().enumerate() {
        if enabled {
            append_cup(out, column.saturating_add(1) as u32, 1);
            out.extend_from_slice(b"\x1bH");
        }
    }
}

fn append_saved_decsc(
    out: &mut Vec<u8>,
    screen: &Screen,
    saved_cursor: (u32, u32, bool),
    saved_cell_state: &[u8],
) {
    let (x, y, origin) = saved_cursor;
    out.extend_from_slice(b"\x1b[?6l");
    out.extend_from_slice(RESET_RENDITION);
    out.extend_from_slice(saved_cell_state);
    if origin {
        out.extend_from_slice(b"\x1b[?6h");
        let row = y.saturating_sub(screen.scroll_region().0).saturating_add(1);
        append_cup(out, x.saturating_add(1), row);
    } else {
        append_cup(out, x.saturating_add(1), y.saturating_add(1));
    }
    out.extend_from_slice(b"\x1b7");
    out.extend_from_slice(b"\x1b[?6l");
}

fn append_active_cursor_state(
    out: &mut Vec<u8>,
    screen: &Screen,
    rows: &[RenderedRow],
    active_cell_state: &[u8],
) {
    let (x, y) = screen.cursor_position();
    if screen.mode() & mode::MODE_ORIGIN != 0 {
        out.extend_from_slice(b"\x1b[?6h");
    } else {
        out.extend_from_slice(b"\x1b[?6l");
    }
    out.extend_from_slice(RESET_RENDITION);
    append_cursor_state(
        out,
        screen,
        x,
        y,
        screen.pending_wrap(),
        Some(rows),
        (screen.mode() & mode::MODE_ORIGIN != 0).then(|| screen.scroll_region().0),
    );
    out.extend_from_slice(RESET_RENDITION);
    out.extend_from_slice(active_cell_state);
    out.extend_from_slice(if screen.mode() & mode::MODE_CURSOR != 0 {
        b"\x1b[?25h"
    } else {
        b"\x1b[?25l"
    });
}

fn append_cursor_state(
    out: &mut Vec<u8>,
    screen: &Screen,
    x: u32,
    y: u32,
    pending_wrap: bool,
    lines: Option<&[RenderedRow]>,
    origin_top: Option<u32>,
) {
    let cursor_row = y.saturating_sub(origin_top.unwrap_or(0)).saturating_add(1);
    if pending_wrap {
        if let Some(line) = lines.and_then(|lines| lines.get(y as usize)) {
            append_cup(out, 1, cursor_row);
            out.extend_from_slice(RESET_RENDITION);
            out.extend_from_slice(&line.bytes);
            return;
        }
    }
    let size = screen.size();
    append_cup(
        out,
        x.min(u32::from(size.cols.saturating_sub(1)))
            .saturating_add(1),
        cursor_row,
    );
}

fn append_cup(out: &mut Vec<u8>, col: u32, row: u32) {
    out.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
}

/// Separator emitted between two rows that the terminal will not wrap itself.
///
/// Rows are linked by [`link_rows`] before they are appended, so a wrapped row
/// is always followed by a row the terminal wraps onto: only an unwrapped row
/// needs this separator.
fn row_separator(previous: Option<&RenderedRow>) -> &'static [u8] {
    match previous {
        Some(previous) if previous.wrapped => b"",
        Some(_) => LINE_BREAK,
        None => b"",
    }
}

fn append_rows(out: &mut Vec<u8>, rows: &[RenderedRow]) {
    for (index, row) in rows.iter().enumerate() {
        out.extend_from_slice(row_separator(
            index.checked_sub(1).map(|previous| &rows[previous]),
        ));
        out.extend_from_slice(ROW_RESET);
        out.extend_from_slice(&row.bytes);
    }
}

fn append_rows_deque(out: &mut Vec<u8>, rows: &VecDeque<RenderedRow>) {
    let mut previous = None;
    for row in rows {
        out.extend_from_slice(row_separator(previous));
        out.extend_from_slice(ROW_RESET);
        out.extend_from_slice(&row.bytes);
        previous = Some(row);
    }
}

fn encoded_rows_len(rows: &[RenderedRow]) -> usize {
    rows.iter()
        .enumerate()
        .fold(0_usize, |length, (index, row)| {
            length
                .saturating_add(
                    row_separator(index.checked_sub(1).map(|previous| &rows[previous])).len(),
                )
                .saturating_add(ROW_RESET.len())
                .saturating_add(row.bytes.len())
        })
}

fn encoded_rows_len_deque(rows: &VecDeque<RenderedRow>) -> usize {
    let mut previous = None;
    rows.iter().fold(0_usize, |length, row| {
        let separator = row_separator(previous).len();
        previous = Some(row);
        length
            .saturating_add(separator)
            .saturating_add(ROW_RESET.len())
            .saturating_add(row.bytes.len())
    })
}

fn capture_options() -> GridRenderOptions {
    GridRenderOptions {
        with_sequences: true,
        include_empty_cells: false,
        trim_spaces: false,
        ..GridRenderOptions::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_transcript::PaneTranscript;
    use rmux_core::{ScreenCaptureRange, TerminalScreen};
    use rmux_proto::TerminalSize;
    use std::io::Write;
    use std::process::{Command, Stdio};

    const SIZE: TerminalSize = TerminalSize { cols: 16, rows: 6 };

    fn snapshot_ansi_lines(screen: &Screen) -> Vec<Vec<u8>> {
        screen
            .capture_transcript_lines_independent(ScreenCaptureRange::default(), capture_options())
    }

    fn snapshot_ansi_rows(screen: &Screen, include_history: bool) -> Vec<(Vec<u8>, bool)> {
        screen.capture_transcript_rows_independent(
            if include_history {
                ScreenCaptureRange {
                    start: None,
                    end: None,
                    start_is_absolute: true,
                    end_is_absolute: true,
                }
            } else {
                ScreenCaptureRange::default()
            },
            capture_options(),
        )
    }

    fn recovered(initial: &[u8], tail: &[u8]) -> (TerminalScreen, TerminalScreen) {
        recovered_after(b"", initial, tail)
    }

    /// Replays a keyframe into a terminal that already carries `stale` state.
    fn recovered_after(
        stale: &[u8],
        initial: &[u8],
        tail: &[u8],
    ) -> (TerminalScreen, TerminalScreen) {
        let mut transcript = PaneTranscript::new(100, SIZE);
        transcript.append_bytes(initial);
        let keyframe = PaneRecoverySeed::capture(&transcript)
            .expect("capture recovery state")
            .keyframe();

        let mut actual = TerminalScreen::new(SIZE, 100);
        actual.feed(stale);
        actual.feed(&keyframe.bytes);
        actual.feed(tail);

        let mut expected = TerminalScreen::new(SIZE, 100);
        expected.feed(initial);
        expected.feed(tail);
        (actual, expected)
    }

    /// Renders every row over the full terminal width.
    ///
    /// A soft-wrapped row owns all of its columns, so the trailing cells of a
    /// wrapped row are interior to its logical line. This view compares the
    /// painted surface without asserting whether a blank column is a cleared
    /// cell or a written space, which an ANSI replay cannot distinguish.
    fn painted_rows(screen: &Screen) -> Vec<(Vec<u8>, bool)> {
        screen.capture_transcript_rows_independent(
            ScreenCaptureRange {
                start: None,
                end: None,
                start_is_absolute: true,
                end_is_absolute: true,
            },
            GridRenderOptions {
                include_empty_cells: true,
                ..capture_options()
            },
        )
    }

    fn assert_painted_surface_equal(actual: &TerminalScreen, expected: &TerminalScreen) {
        assert_eq!(
            painted_rows(actual.screen()),
            painted_rows(expected.screen())
        );
        assert_eq!(
            actual.screen().cursor_position(),
            expected.screen().cursor_position()
        );
    }

    /// Asserts that a recovered screen reflows exactly like the source screen,
    /// which is where a lost soft-wrap flag or a misplaced row becomes visible.
    ///
    /// Reflow output is compared as painted columns: a blank column inside a
    /// soft-wrapped logical line is a written space on one side and a cleared
    /// cell on the other, because a replayed row is stored as compacted ASCII
    /// while a row the terminal edited is stored as cells. The two paint the
    /// same screen and both trim to the same `capture-pane` output.
    fn assert_reflow_equal(initial: &[u8], tail: &[u8]) {
        for cols in [4_u16, 6, 8, 10, 12, 15, 20, 24, 40] {
            let (mut actual, mut expected) = recovered(initial, tail);
            let resized = TerminalSize {
                cols,
                rows: SIZE.rows,
            };
            actual.resize(resized);
            expected.resize(resized);
            assert_eq!(
                painted_rows(actual.screen()),
                painted_rows(expected.screen()),
                "reflow to {cols} columns diverged"
            );
            assert_eq!(
                actual.screen().cursor_position(),
                expected.screen().cursor_position(),
                "reflow to {cols} columns moved the cursor"
            );
        }
    }

    fn assert_visible_equal(actual: &TerminalScreen, expected: &TerminalScreen) {
        assert_eq!(
            snapshot_ansi_lines(actual.screen()),
            snapshot_ansi_lines(expected.screen())
        );
        assert_eq!(
            actual.screen().cursor_position(),
            expected.screen().cursor_position()
        );
        assert_eq!(actual.screen().mode(), expected.screen().mode());
        assert_eq!(
            actual.screen().scroll_region(),
            expected.screen().scroll_region()
        );
        assert_eq!(actual.screen().title(), expected.screen().title());
        assert_eq!(
            actual.screen().title_stack(),
            expected.screen().title_stack()
        );
        assert_eq!(actual.screen().path(), expected.screen().path());
        assert_eq!(actual.pending_bytes(), expected.pending_bytes());
    }

    fn assert_complete_equal(actual: &TerminalScreen, expected: &TerminalScreen) {
        assert_visible_equal(actual, expected);
        assert_eq!(
            snapshot_ansi_rows(actual.screen(), true),
            snapshot_ansi_rows(expected.screen(), true)
        );
        assert_eq!(
            actual.screen().history_size(),
            expected.screen().history_size()
        );
    }

    #[test]
    fn keyframe_preserves_pending_wrap_before_continuation() {
        let (actual, expected) = recovered(b"0123456789abcdef", b"X");
        assert_visible_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_custom_tabs() {
        let (actual, expected) = recovered(b"\x1b[3g\x1b[1;4H\x1bH\x1b[H", b"\tX");
        assert_visible_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_decsc_state() {
        let initial = b"\x1b[31m\x1b[2;3H\x1b7\x1b[0m\x1b[5;10H";
        let (actual, expected) = recovered(initial, b"\x1b8X");
        assert_visible_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_restores_default_decsc_rendition_absolutely() {
        let initial =
            b"\x1b[2;3H\x1b7\x1b[31m\x1b]8;id=active;https://example.test\x1b\\\x1b[5;10H";
        let (actual, expected) = recovered(initial, b"\x1b8X");
        assert_visible_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_incomplete_parser_state() {
        let (actual, expected) = recovered(b"base\x1b[38;2;1", b"2;34;56mX");
        assert_visible_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_main_and_alternate_buffers() {
        let initial = b"main\x1b[?1049h\x1b[2J\x1b[Halt";
        let (actual, expected) = recovered(initial, b"\x1b[?1049lX");
        assert_visible_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_saved_main_after_resize_in_alternate_screen() {
        let initial = b"main-0\r\nmain-1\r\nmain-2\r\nmain-3\r\nmain-4\x1b[?1049h\x1b[2J\x1b[Halt";
        for resized in [
            TerminalSize { cols: 20, rows: 9 },
            TerminalSize { cols: 12, rows: 4 },
        ] {
            let mut transcript = PaneTranscript::new(100, SIZE);
            transcript.append_bytes(initial);
            transcript.resize(resized);
            transcript.append_bytes(b"\x1b[2;2Htail");
            let keyframe = PaneRecoverySeed::capture(&transcript)
                .expect("capture resized alternate-screen recovery state")
                .keyframe();

            let mut actual = TerminalScreen::new(resized, 100);
            actual.feed(&keyframe.bytes);
            actual.feed(b"\x1b[?1049lX");

            let mut expected = TerminalScreen::new(SIZE, 100);
            expected.feed(initial);
            expected.resize(resized);
            expected.feed(b"\x1b[2;2Htail");
            expected.feed(b"\x1b[?1049lX");

            assert_complete_equal(&actual, &expected);
            assert_eq!(
                keyframe.history_rows_total,
                u64::try_from(expected.screen().history_size()).unwrap_or(u64::MAX)
            );

            let visible = PaneRecoveryDraft::capture(&transcript)
                .expect("capture visible-only resized alternate-screen state")
                .materialize_visible_only()
                .expect("materialize visible-only resized alternate-screen state")
                .keyframe();
            let mut visible_actual = TerminalScreen::new(resized, 100);
            visible_actual.feed(&visible.bytes);
            visible_actual.feed(b"\x1b[?1049lX");
            assert_visible_equal(&visible_actual, &expected);
            assert_eq!(visible_actual.screen().history_size(), 0);
            assert_eq!(
                visible.history_rows_total,
                u64::try_from(expected.screen().history_size()).unwrap_or(u64::MAX)
            );
        }
    }

    #[test]
    fn keyframe_preserves_main_scrollback_while_alternate_is_active() {
        let initial = b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven\r\neight\x1b[?1049h\x1b[2J\x1b[Halt";
        let (actual, expected) = recovered(initial, b"\x1b[?1049l\r\nTAIL");
        assert_complete_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_replaces_stale_main_and_alternate_buffers() {
        let initial = b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven\r\neight\x1b[?1049h\x1b[2J\x1b[Halt";
        let mut transcript = PaneTranscript::new(100, SIZE);
        transcript.append_bytes(initial);
        let keyframe = PaneRecoverySeed::capture(&transcript)
            .expect("capture recovery state")
            .keyframe();

        let mut actual = TerminalScreen::new(SIZE, 100);
        actual.feed(
            b"stale-one\r\nstale-two\r\nstale-three\r\nstale-four\r\nstale-five\r\nstale-six\r\nstale-seven\x1b[?1049hstale-alt",
        );
        actual.feed(&keyframe.bytes);
        actual.feed(b"\x1b[?1049l\r\nTAIL");

        let mut expected = TerminalScreen::new(SIZE, 100);
        expected.feed(initial);
        expected.feed(b"\x1b[?1049l\r\nTAIL");
        assert_complete_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_saved_main_pending_wrap_while_alternate_is_active() {
        let initial = b"0123456789abcdef\x1b[?1049h\x1b[2J\x1b[Hdifferent-alt";
        let (actual, expected) = recovered(initial, b"\x1b[?1049lX");
        assert_visible_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_active_alternate_pending_wrap() {
        let initial = b"main\x1b[?1049h0123456789abcdef";
        let (actual, expected) = recovered(initial, b"X\x1b[?1049l");
        assert_complete_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_preserves_scrollback_and_wrapped_rows() {
        let initial = b"abcdefghABCDEFGH\r\nline-2\r\nline-3\r\nline-4";
        let (mut actual, mut expected) = recovered(initial, b"\r\nsuffix");
        assert_complete_equal(&actual, &expected);

        let resized = TerminalSize { cols: 10, rows: 4 };
        actual.resize(resized);
        expected.resize(resized);
        assert_complete_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_keeps_a_shortened_wrapped_row_from_absorbing_the_next_row() {
        // DCH on a soft-wrapped row leaves the row shorter than the terminal
        // while it stays wrapped, so a replay that relies on autowrap alone
        // paints the next row's cells onto it.
        for initial in [
            b"0123456789abcdefGHIJ\x1b[1;1H\x1b[3P".as_slice(),
            b"0123456789abcdefXYZ\x1b[1;1H\x1b[4P".as_slice(),
            b"0123456789abcdefXYZ\x1b[1;1H\x1b[1P".as_slice(),
            b"0123456789abcdefXYZ\x1b[1;1H\x1b[16P".as_slice(),
            b"0123456789abcdefXYZ\x1b[1;9H\x1b[3P".as_slice(),
        ] {
            let (actual, expected) = recovered(initial, b"");
            assert_complete_equal(&actual, &expected);
            assert_reflow_equal(initial, b"");
        }

        // A styled row cannot be stored as compacted ASCII, so the blanks the
        // fill repaints stay written spaces. They paint the same screen and
        // reflow the same way, but they are not cleared cells.
        let styled = b"\x1b[44m0123456789abcdef\x1b[0mXYZ\x1b[1;1H\x1b[3P";
        let (actual, expected) = recovered(styled, b"");
        assert_painted_surface_equal(&actual, &expected);
        assert_reflow_equal(styled, b"");
    }

    #[test]
    fn keyframe_keeps_a_shortened_wrapped_row_from_absorbing_the_continuation_it_scrolled_into() {
        // The same row, once in scrollback and once at the scrollback boundary,
        // exercises the history deque and the history-to-viewport link.
        let scrolled = b"0123456789abcdefGHIJ\x1b[1;1H\x1b[3P\x1b[6;1Hbottom\r\ntail";
        let (actual, expected) = recovered(scrolled, b"");
        assert_eq!(actual.screen().history_size(), 1);
        assert_complete_equal(&actual, &expected);
        assert_reflow_equal(scrolled, b"");

        let deep = b"0123456789abcdefGHIJ\x1b[1;1H\x1b[3P\x1b[6;1Hb\r\nc\r\nd\r\ne";
        let (actual, expected) = recovered(deep, b"");
        assert!(actual.screen().history_size() >= 3);
        assert_complete_equal(&actual, &expected);
        assert_reflow_equal(deep, b"");
    }

    #[test]
    fn keyframe_keeps_a_shortened_wrapped_saved_row_from_absorbing_the_next_row() {
        // The saved main screen is repainted the same way before the alternate
        // screen is entered.
        let initial = b"0123456789abcdefGHIJ\x1b[1;1H\x1b[3P\x1b[?1049h\x1b[2J\x1b[Halt";
        let (actual, expected) = recovered(initial, b"\x1b[?1049l");
        assert_complete_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_reproduces_a_wide_glyph_pre_wrap_gap_without_filling_it() {
        // A wide glyph that does not fit leaves one unused column and wraps.
        // The replay must let the glyph pre-wrap again instead of filling the
        // gap, or the reflowed logical line gains a space that never existed.
        let initial = "界界界界界界界a界tail".as_bytes();
        let (actual, expected) = recovered(initial, b"");
        assert_complete_equal(&actual, &expected);
        assert_reflow_equal(initial, b"");

        let scrolled = "界界界界界界界a界tail\u{1b}[6;1Hb\r\nc".as_bytes();
        let (actual, expected) = recovered(scrolled, b"");
        assert!(actual.screen().history_size() >= 1);
        assert_complete_equal(&actual, &expected);
        assert_reflow_equal(scrolled, b"");
    }

    #[test]
    fn keyframe_fills_a_shortened_wrapped_row_before_a_wide_continuation() {
        // One free column that is *not* a pre-wrap gap must still be filled:
        // letting the wide glyph pre-wrap into it would drop a blank the row
        // still holds, which reflow then rewraps one column early.
        for initial in [
            "0123456789abcdef界tail\u{1b}[1;1H\u{1b}[1P".as_bytes(),
            "0123456789abcdef界tail\u{1b}[1;1H\u{1b}[3P".as_bytes(),
        ] {
            let (actual, expected) = recovered(initial, b"");
            assert_complete_equal(&actual, &expected);
            assert_reflow_equal(initial, b"");
            assert_painted_surface_equal(&actual, &expected);
        }
    }

    #[test]
    fn keyframe_keeps_a_wrapped_row_whose_continuation_was_erased() {
        // `EL` on the continuation row leaves a wrapped row followed by a row
        // that paints nothing: the pending wrap can never resolve, so every
        // later row would climb one line.
        let initial = b"0123456789abcdefXYZ\x1b[2;1H\x1b[K\x1b[3;1Hthird";
        let (actual, expected) = recovered(initial, b"");
        assert_complete_equal(&actual, &expected);
        assert_reflow_equal(initial, b"");
    }

    #[test]
    fn keyframe_paints_wrapped_rows_when_the_client_disabled_autowrap() {
        // Rows are placed by the replaying terminal's own wrap, so the paint
        // cannot inherit a client that left DECAWM off.
        let initial = b"0123456789abcdefGHIJ\x1b[1;1H\x1b[3P";
        let (actual, expected) = recovered_after(b"\x1b[?7lstale", initial, b"");
        assert_complete_equal(&actual, &expected);

        // A pane that itself disabled autowrap still gets its mode restored.
        let disabled = b"\x1b[?7l0123456789abcdefGHIJ";
        let (actual, expected) = recovered_after(b"\x1b[?7lstale", disabled, b"");
        assert_complete_equal(&actual, &expected);
        assert_eq!(actual.screen().mode() & mode::MODE_WRAP, 0);
    }

    #[test]
    fn keyframe_keeps_a_bounded_recent_history_suffix_in_alternate_screen() {
        let size = TerminalSize { cols: 64, rows: 4 };
        let mut transcript = PaneTranscript::new(50_000, size);
        let mut output = Vec::with_capacity(3 * 1024 * 1024);
        for row in 0..45_000 {
            writeln!(
                output,
                "row-{row:05}-abcdefghijklmnopqrstuvwxyz-0123456789\r"
            )
            .expect("write history row");
        }
        output.extend_from_slice(b"\x1b[?1049hALT");
        transcript.append_bytes(&output);
        let expected_history_size = transcript.screen().history_size();
        let expected_history_bytes = transcript.screen().history_bytes();

        let seed = PaneRecoverySeed::capture(&transcript).expect("capture bounded recovery state");
        assert_eq!(seed.screen().history_size(), 0);
        assert_eq!(seed.history_size(), expected_history_size);
        assert_eq!(seed.history_bytes(), expected_history_bytes);
        assert!(seed.screen().is_alternate());
        let keyframe = seed.keyframe();
        assert!(keyframe.alternate);
        assert!(keyframe.bytes.len() <= MAX_RECOVERY_KEYFRAME_BYTES);
        assert_eq!(
            keyframe.history_rows_total,
            u64::try_from(expected_history_size).unwrap_or(u64::MAX)
        );
        assert!(keyframe.history_rows_total > keyframe.history_rows_included);
        assert!(keyframe.history_rows_included > 0);

        let mut recovered = TerminalScreen::new(size, 50_000);
        recovered.feed(&keyframe.bytes);
        recovered.feed(b"\x1b[?1049l");
        assert_eq!(
            u64::try_from(recovered.screen().history_size()).unwrap_or(u64::MAX),
            keyframe.history_rows_included
        );
    }

    fn keyframes_for_both_history_policies(
        initial: &[u8],
    ) -> (PaneRecoveryKeyframe, PaneRecoveryKeyframe) {
        let mut transcript = PaneTranscript::new(100, SIZE);
        transcript.append_bytes(initial);
        let recent = PaneRecoveryDraft::capture(&transcript)
            .expect("capture recovery state")
            .materialize()
            .expect("materialize recent-history keyframe")
            .keyframe();
        let visible = PaneRecoveryDraft::capture(&transcript)
            .expect("capture recovery state")
            .materialize_visible_only()
            .expect("materialize visible-only keyframe")
            .keyframe();
        (recent, visible)
    }

    fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn visible_only_keyframe_drops_scrollback_but_replays_the_viewport() {
        let initial = b"secret-token\r\nrow-0\r\nrow-1\r\nrow-2\r\nrow-3\r\nrow-4\r\nrow-5\r\nrow-6\r\nrow-7\r\nrow-8\r\nrow-9\r\n";
        let (recent, visible) = keyframes_for_both_history_policies(initial);

        // The owner-authenticated SDK contract is unchanged: it still replays
        // the newest scrollback that fits.
        assert!(recent.history_rows_included > 0);
        assert!(contains_subslice(&recent.bytes, b"secret-token"));

        // The shared-link surface reports the same true total and replays none
        // of it.
        assert_eq!(visible.history_rows_total, recent.history_rows_total);
        assert!(visible.history_rows_total > 0);
        assert_eq!(visible.history_rows_included, 0);
        assert!(!contains_subslice(&visible.bytes, b"secret-token"));
        assert!(!contains_subslice(&visible.bytes, b"row-0"));

        // ... and the visible viewport still replays exactly, with no
        // scrollback reconstructed in the viewer's emulator.
        let mut actual = TerminalScreen::new(SIZE, 100);
        actual.feed(&visible.bytes);
        let mut expected = TerminalScreen::new(SIZE, 100);
        expected.feed(initial);
        assert_visible_equal(&actual, &expected);
        assert_eq!(actual.screen().history_size(), 0);
    }

    #[test]
    fn visible_only_keyframe_keeps_the_saved_main_viewport_under_alternate_screen() {
        let initial =
            b"secret-token\r\nmain-0\r\nmain-1\r\nmain-2\r\nmain-3\r\nmain-4\r\nmain-5\r\nmain-6\x1b[?1049h\x1b[2J\x1b[Halt-body";
        let (_, visible) = keyframes_for_both_history_policies(initial);

        assert!(visible.alternate);
        assert_eq!(visible.history_rows_included, 0);
        assert!(!contains_subslice(&visible.bytes, b"secret-token"));

        // Leaving the alternate screen must still restore the pane's visible
        // main viewport, which is what the keyframe exists to fix.
        let mut actual = TerminalScreen::new(SIZE, 100);
        actual.feed(&visible.bytes);
        actual.feed(b"\x1b[?1049l");
        let mut expected = TerminalScreen::new(SIZE, 100);
        expected.feed(initial);
        expected.feed(b"\x1b[?1049l");
        assert_visible_equal(&actual, &expected);
        assert_eq!(actual.screen().history_size(), 0);
    }

    #[test]
    fn visible_only_keyframe_stays_viewport_sized_against_a_full_scrollback() {
        let size = TerminalSize { cols: 80, rows: 24 };
        let mut transcript = PaneTranscript::new(50_000, size);
        let mut output = Vec::with_capacity(4 * 1024 * 1024);
        for row in 0..40_000 {
            writeln!(
                output,
                "secret-row-{row:05}-abcdefghijklmnopqrstuvwxyz-0123456789\r"
            )
            .expect("write history row");
        }
        // `clear`: the viewport is blank, every secret row lives in scrollback.
        output.extend_from_slice(b"\x1b[H\x1b[2J");
        transcript.append_bytes(&output);

        let visible = PaneRecoveryDraft::capture(&transcript)
            .expect("capture recovery state")
            .materialize_visible_only()
            .expect("materialize visible-only keyframe")
            .keyframe();

        assert!(visible.history_rows_total >= 40_000);
        assert_eq!(visible.history_rows_included, 0);
        assert!(!contains_subslice(&visible.bytes, b"secret-row-"));

        // Geometry-independent invariant: replaying the keyframe reconstructs
        // no scrollback at all in the viewer's emulator, so nothing the viewer
        // can scroll back to exists.
        let mut recovered = TerminalScreen::new(size, 50_000);
        recovered.feed(&visible.bytes);
        assert_eq!(recovered.screen().history_size(), 0);
    }

    #[test]
    fn keyframe_never_splits_one_oversized_wrapped_history_group() {
        let size = TerminalSize { cols: 16, rows: 4 };
        let mut transcript = PaneTranscript::new(100_000, size);
        transcript.append_bytes(&vec![b'x'; 2 * 1024 * 1024]);

        let keyframe = PaneRecoverySeed::capture(&transcript)
            .expect("capture bounded wrapped recovery")
            .keyframe();

        assert!(keyframe.history_rows_total > 0);
        assert_eq!(keyframe.history_rows_included, 0);
        assert!(keyframe.bytes.len() <= MAX_RECOVERY_KEYFRAME_BYTES);
    }

    #[test]
    fn keyframe_preserves_wide_cells_and_wrap_boundaries() {
        let initial = "界界界界界界界界界\r\n尾".repeat(8);
        let (actual, expected) = recovered(initial.as_bytes(), "続".as_bytes());
        assert_complete_equal(&actual, &expected);
    }

    #[test]
    fn keyframe_reports_bounded_title_path_and_hyperlink_metadata() {
        let oversized = "x".repeat(MAX_RECOVERY_STRING_BYTES + 1);
        let oversized_link = "https://example.test/".to_owned()
            + &"y".repeat(MAX_RECOVERY_HYPERLINK_ENTRY_BYTES + 1);
        let mut transcript = PaneTranscript::new(100, SIZE);
        transcript.append_bytes(format!("\x1b]2;{oversized}\x1b\\\x1b[22;2t").as_bytes());
        transcript.append_bytes(format!("\x1b]7;file:///{oversized}\x1b\\").as_bytes());
        transcript
            .append_bytes(format!("\x1b]8;;{oversized_link}\x1b\\X\x1b]8;;\x1b\\").as_bytes());

        let seed =
            PaneRecoverySeed::capture(&transcript).expect("capture bounded recovery metadata");
        assert!(seed.screen().title().len() <= MAX_RECOVERY_STRING_BYTES);
        assert!(seed.screen().path().len() <= MAX_RECOVERY_STRING_BYTES);
        let keyframe = seed.keyframe();
        assert!(!keyframe.metadata_complete);
        assert!(keyframe.bytes.len() <= MAX_RECOVERY_KEYFRAME_BYTES);
    }

    #[test]
    fn keyframe_drops_amplified_hyperlinks_before_failing_the_stream() {
        let size = TerminalSize {
            cols: MAX_RECOVERY_COLS as u16,
            rows: 1,
        };
        let left = "https://left.test/".to_owned() + &"a".repeat(450);
        let right = "https://right.test/".to_owned() + &"b".repeat(450);
        let mut output = Vec::with_capacity(MAX_RECOVERY_COLS * 512);
        for column in 0..MAX_RECOVERY_COLS {
            let (id, uri) = if column % 2 == 0 {
                ("left", &left)
            } else {
                ("right", &right)
            };
            write!(output, "\x1b]8;id={id};{uri}\x1b\\x").expect("write linked cell");
        }
        output.extend_from_slice(b"\x1b]8;;\x1b\\");

        let mut transcript = PaneTranscript::new(0, size);
        transcript.append_bytes(&output);
        let keyframe = PaneRecoverySeed::capture(&transcript)
            .expect("hyperlink amplification must degrade instead of failing")
            .keyframe();

        assert!(!keyframe.metadata_complete);
        assert!(keyframe.bytes.len() <= MAX_RECOVERY_KEYFRAME_BYTES);
        assert!(!keyframe
            .bytes
            .windows(b"https://left.test/".len())
            .any(|window| window == b"https://left.test/"));

        let mut recovered = TerminalScreen::new(size, 0);
        recovered.feed(&keyframe.bytes);
        let plain = recovered
            .screen()
            .capture_transcript(ScreenCaptureRange::default(), GridRenderOptions::default());
        assert_eq!(
            plain.iter().filter(|byte| **byte == b'x').count(),
            MAX_RECOVERY_COLS
        );
    }

    #[test]
    fn recovery_geometry_is_rejected_before_viewport_clone_or_render() {
        let transcript = PaneTranscript::new(
            0,
            TerminalSize {
                cols: (MAX_RECOVERY_COLS + 1) as u16,
                rows: 1,
            },
        );
        assert!(matches!(
            PaneRecoverySeed::capture(&transcript),
            Err(RmuxError::Server(message)) if message.contains("geometry cap")
        ));
    }

    #[test]
    fn maximum_typed_geometry_with_maximum_cell_text_remains_recoverable() {
        let size = TerminalSize {
            cols: 384,
            rows: 256,
        };
        let cell = "a\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}";
        assert_eq!(cell.len(), 21);
        let cells = usize::from(size.cols) * usize::from(size.rows);
        assert_eq!(cells, MAX_RECOVERY_TYPED_SNAPSHOT_CELLS);

        let mut transcript = PaneTranscript::new(0, size);
        transcript.append_bytes(cell.repeat(cells).as_bytes());

        let keyframe = PaneRecoverySeed::capture(&transcript)
            .expect("accepted typed recovery geometry must fit its keyframe")
            .keyframe();
        assert!(keyframe.bytes.len() <= MAX_RECOVERY_KEYFRAME_BYTES);
    }

    #[test]
    fn keyframe_fits_web_and_detached_rpc_single_frame_caps() {
        use rmux_proto::{
            PaneRawRebase, PaneRawRebaseReason, PaneRecoveryCoverage,
            DEFAULT_MAX_DETACHED_FRAME_LENGTH,
        };

        let mut transcript = PaneTranscript::new(50_000, SIZE);
        let row = b"bounded-recovery-row\r\n";
        for _ in 0..50_000 {
            transcript.append_bytes(row);
        }
        let keyframe = PaneRecoverySeed::capture(&transcript)
            .expect("capture transport-bounded recovery")
            .keyframe();
        assert!(keyframe.bytes.len() < 2 * DEFAULT_MAX_FRAME_LENGTH);

        let rebase = PaneRawRebase {
            epoch: 1,
            generation: 1,
            invalidation_revision: 0,
            next_sequence: 0,
            cols: keyframe.cols,
            rows: keyframe.rows,
            keyframe: keyframe.bytes,
            alternate: keyframe.alternate,
            coverage: PaneRecoveryCoverage {
                history_rows_total: keyframe.history_rows_total,
                history_rows_included: keyframe.history_rows_included,
                metadata_complete: keyframe.metadata_complete,
            },
            snapshot: None,
            reason: PaneRawRebaseReason::Initial,
        };
        let encoded = bincode::serialized_size(&rebase).expect("serialize bounded rebase");
        assert!(encoded < DEFAULT_MAX_DETACHED_FRAME_LENGTH as u64);
    }

    #[test]
    fn maximum_typed_snapshot_and_keyframe_fit_the_detached_rpc_cap_together() {
        use rmux_proto::{
            PaneRawRebase, PaneRawRebaseReason, PaneRecoveryCoverage, PaneSnapshotCell,
            PaneSnapshotCursor, PaneSnapshotResponse, DEFAULT_MAX_DETACHED_FRAME_LENGTH,
        };

        let cell = PaneSnapshotCell {
            text: "x".repeat(21),
            width: 1,
            padding: false,
            attributes: u16::MAX,
            fg: i32::MIN,
            bg: i32::MAX,
            us: i32::MIN,
            link: u32::MAX,
        };
        let snapshot = PaneSnapshotResponse {
            cols: 384,
            rows: 256,
            cells: vec![cell; MAX_RECOVERY_TYPED_SNAPSHOT_CELLS],
            cursor: PaneSnapshotCursor {
                row: 255,
                col: 383,
                visible: true,
                style: u32::MAX,
            },
            revision: u64::MAX,
        };
        let rebase = PaneRawRebase {
            epoch: u64::MAX,
            generation: u64::MAX,
            invalidation_revision: u64::MAX,
            next_sequence: u64::MAX,
            cols: 384,
            rows: 256,
            keyframe: vec![b'x'; MAX_RECOVERY_KEYFRAME_BYTES],
            alternate: true,
            coverage: PaneRecoveryCoverage {
                history_rows_total: u64::MAX,
                history_rows_included: u64::MAX,
                metadata_complete: false,
            },
            snapshot: Some(snapshot),
            reason: PaneRawRebaseReason::GenerationChanged,
        };

        let encoded = bincode::serialized_size(&rebase).expect("measure worst-case rebase");
        assert!(encoded < DEFAULT_MAX_DETACHED_FRAME_LENGTH as u64);
    }

    #[test]
    fn keyframe_preserves_unicode_titles_without_treating_utf8_as_c1() {
        let (actual, expected) = recovered("\u{1b}]2;hé-界\u{7}".as_bytes(), b"");
        assert_eq!(actual.screen().title(), expected.screen().title());
        assert_eq!(actual.screen().title(), "hé-界");
    }

    #[test]
    fn keyframe_preserves_path_and_title_stack_for_continuation() {
        let initial = b"\x1b]7;file:///srv/build\x1b\\\x1b]2;first\x1b\\\x1b[22;2t\x1b]2;second\x1b\\\x1b[22;2t\x1b]2;current\x1b\\";
        let (actual, expected) = recovered(initial, b"\x1b[23;2t");
        assert_visible_equal(&actual, &expected);
        assert_eq!(actual.screen().path(), "file:///srv/build");
        assert_eq!(actual.screen().title(), "second");
    }

    #[test]
    fn keyframe_replaces_stale_title_stack_before_continuation() {
        let initial =
            b"\x1b]2;first\x1b\\\x1b[22;2t\x1b]2;second\x1b\\\x1b[22;2t\x1b]2;current\x1b\\";
        let mut transcript = PaneTranscript::new(100, SIZE);
        transcript.append_bytes(initial);
        let keyframe = PaneRecoverySeed::capture(&transcript)
            .expect("capture recovery state")
            .keyframe();

        let mut actual = TerminalScreen::new(SIZE, 100);
        actual.feed(
            b"\x1b]2;stale-a\x1b\\\x1b[22;2t\x1b]2;stale-b\x1b\\\x1b[22;2t\x1b]2;stale-current\x1b\\",
        );
        actual.feed(&keyframe.bytes);
        actual.feed(b"\x1b[23;2t");

        let mut expected = TerminalScreen::new(SIZE, 100);
        expected.feed(initial);
        expected.feed(b"\x1b[23;2t");
        assert_visible_equal(&actual, &expected);
        assert_eq!(actual.screen().title(), "second");
    }

    #[test]
    fn keyframe_preserves_dynamic_colour_query_state() {
        let initial = b"\x1b]10;#112233\x1b\\\x1b]11;rgb:44/55/66\x07\x1b]12;#778899\x1b\\";
        let (mut actual, mut expected) = recovered(initial, b"");
        let queries = b"\x1b]10;?\x1b\\\x1b]11;?\x07\x1b]12;?\x1b\\";
        actual.feed(queries);
        expected.feed(queries);
        let expected_replies = expected.take_replies();
        assert!(
            !expected_replies.is_empty(),
            "the continuation must exercise stored colour state"
        );
        assert_eq!(actual.take_replies(), expected_replies);
    }

    #[test]
    fn keyframe_clears_stale_dynamic_colour_query_state() {
        let transcript = PaneTranscript::new(100, SIZE);
        let keyframe = PaneRecoverySeed::capture(&transcript)
            .expect("capture recovery state")
            .keyframe();
        let mut actual = TerminalScreen::new(SIZE, 100);
        actual.feed(b"\x1b]10;#112233\x1b\\");
        actual.feed(&keyframe.bytes);
        actual.feed(b"\x1b]10;?\x1b\\");
        assert!(actual.take_replies().is_empty());
    }

    #[test]
    #[ignore = "release gate installs and runs the pinned xterm.js oracle"]
    fn keyframes_converge_in_independent_xterm_oracle() {
        let vectors = [
            (
                "scrollback-wrap-title",
                TerminalSize { cols: 12, rows: 4 },
                b"\x1b]2;build\x07\x1b[31mred-wide-\xe7\x95\x8c\r\nline-2\r\nline-3\r\nline-4\r\nline-5"
                    .as_slice(),
                b"\r\nTAIL".as_slice(),
            ),
            (
                "alternate-saved-main",
                TerminalSize { cols: 16, rows: 5 },
                b"main-one\r\nmain-two\x1b[3;4H\x1b[?1049h\x1b[32mALT\x1b[5;8H"
                    .as_slice(),
                b"\x1b[?1049lZ".as_slice(),
            ),
            (
                "alternate-saved-main-pending-wrap",
                TerminalSize { cols: 16, rows: 5 },
                b"0123456789abcdef\x1b[?1049h\x1b[2J\x1b[Hdifferent-alt".as_slice(),
                b"\x1b[?1049lX".as_slice(),
            ),
            (
                "alternate-saved-main-scrollback",
                TerminalSize { cols: 16, rows: 5 },
                b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven\r\neight\x1b[?1049h\x1b[2J\x1b[Halt"
                    .as_slice(),
                b"\x1b[?1049l\r\nTAIL".as_slice(),
            ),
            (
                "alternate-active-pending-wrap",
                TerminalSize { cols: 16, rows: 5 },
                b"main\x1b[?1049h0123456789abcdef".as_slice(),
                b"X\x1b[?1049l".as_slice(),
            ),
            (
                "tabs-decsc-parser",
                TerminalSize { cols: 16, rows: 6 },
                b"\x1b[3g\x1b[1;5H\x1bH\x1b[31m\x1b[2;3H\x1b7\x1b[0m\x1b[5;10H\x1b[38;2;1"
                    .as_slice(),
                b"2;34;56mX\x1b8\tY".as_slice(),
            ),
            (
                "styled-trailing-blanks",
                TerminalSize { cols: 12, rows: 4 },
                b"\x1b[44mA    \x1b[0m\x1b[2;1Hplain\x1b[2;12H\x1b[41m \x1b[0m"
                    .as_slice(),
                b"\x1b[3;1Htail".as_slice(),
            ),
            (
                "pending-wrap-wide-unicode-title",
                TerminalSize { cols: 12, rows: 4 },
                "0123456789ab\u{1b}]2;hé-界\u{7}".as_bytes(),
                "界".as_bytes(),
            ),
            (
                "origin-region-cursor-style",
                TerminalSize { cols: 16, rows: 6 },
                b"top\r\nmiddle\r\nbottom\x1b[2;5r\x1b[?6h\x1b[3 q\x1b[2;4H\x1b[7mX"
                    .as_slice(),
                b"\x1b[0mY".as_slice(),
            ),
            (
                "decsc-default-rendition-is-absolute",
                TerminalSize { cols: 16, rows: 6 },
                b"\x1b[2;3H\x1b7\x1b[31m\x1b]8;id=active;https://example.test\x1b\\\x1b[5;10H"
                    .as_slice(),
                b"\x1b8X".as_slice(),
            ),
            (
                "post-rep-wide-wrap-rebase",
                TerminalSize { cols: 6, rows: 3 },
                "a界\u{1b}[1b".as_bytes(),
                b"ZW".as_slice(),
            ),
            (
                "dch-shortened-wrapped-row",
                TerminalSize { cols: 16, rows: 6 },
                b"0123456789abcdefGHIJ\x1b[1;1H\x1b[3P".as_slice(),
                b"".as_slice(),
            ),
            (
                "dch-shortened-wrapped-row-scrolled",
                TerminalSize { cols: 16, rows: 6 },
                b"0123456789abcdefGHIJ\x1b[1;1H\x1b[3P\x1b[6;1Hbottom\r\ntail".as_slice(),
                b"\r\nafter".as_slice(),
            ),
            // A wrapped row whose continuation was erased is deliberately not
            // an oracle vector: rmux keeps the row's soft-wrap flag where tmux
            // 3.7b and xterm.js drop it (see
            // .rmux-audit/h-recovery-keyframe/oracle-probe.txt), so the vector
            // would measure that grid divergence instead of the keyframe. The
            // keyframe's own behaviour is pinned by
            // `keyframe_keeps_a_wrapped_row_whose_continuation_was_erased`.
            (
                "wide-glyph-pre-wrap-gap",
                TerminalSize { cols: 16, rows: 6 },
                "界界界界界界界a界tail".as_bytes(),
                "\u{1b}[3;1H続".as_bytes(),
            ),
            (
                "dch-shortened-wrapped-row-before-wide-glyph",
                TerminalSize { cols: 16, rows: 6 },
                "0123456789abcdef界tail\u{1b}[1;1H\u{1b}[1P".as_bytes(),
                b"".as_slice(),
            ),
        ];
        let vectors = vectors
            .into_iter()
            .map(|(name, size, initial, tail)| {
                let mut transcript = PaneTranscript::new(100, size);
                transcript.append_bytes(initial);
                let keyframe = PaneRecoverySeed::capture(&transcript)
            .expect("capture recovery state")
            .keyframe();
                serde_json::json!({
                    "name": name,
                    "cols": size.cols,
                    "rows": size.rows,
                    "scrollback": 100,
                    "actualPrefix": b"\x1b]2;stale\x1b\\\x1b[31mstale-main\r\nstale-history\x1b[?1049hstale-alt".as_slice(),
                    "initial": initial,
                    "keyframe": keyframe.bytes,
                    "tail": tail,
                })
            })
            .collect::<Vec<_>>();
        let oracle_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/xterm-oracle");
        let mut child = Command::new("node")
            .arg("recovery-oracle.mjs")
            .current_dir(&oracle_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start pinned xterm.js recovery oracle");
        child
            .stdin
            .as_mut()
            .expect("oracle stdin")
            .write_all(
                serde_json::to_vec(&serde_json::json!({ "vectors": vectors }))
                    .expect("encode oracle vectors")
                    .as_slice(),
            )
            .expect("write oracle vectors");
        let output = child.wait_with_output().expect("wait for xterm.js oracle");
        assert!(
            output.status.success(),
            "xterm.js recovery oracle failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
