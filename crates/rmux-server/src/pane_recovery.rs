use std::collections::VecDeque;
use std::ops::Range;

use rmux_core::input::mode;
use rmux_core::{render_dec_modes_for_snapshot, GridRenderOptions, RecoveryRowRenderer, Screen};
use rmux_proto::{RmuxError, DEFAULT_MAX_FRAME_LENGTH};

use crate::pane_transcript::PaneTranscript;

const RESET_PREFIX: &[u8] =
    b"\x1b[?2026l\x1b[?1049l\x1b[?6l\x1b[r\x1b[0m\x1b]8;;\x1b\\\x1b[?25l\x1b[2J\x1b[3J\x1b[H";
const ALT_SCREEN_PREFIX: &[u8] =
    b"\x1b[?1049h\x1b[?6l\x1b[r\x1b[0m\x1b]8;;\x1b\\\x1b[?25l\x1b[2J\x1b[H";
const ALT_SCREEN_NO_CURSOR_PREFIX: &[u8] =
    b"\x1b[?47h\x1b[?6l\x1b[r\x1b[0m\x1b]8;;\x1b\\\x1b[?25l\x1b[2J\x1b[H";
const RESET_RENDITION: &[u8] = b"\x1b[0m\x1b]8;;\x1b\\";
const ROW_RESET: &[u8] = b"\x1b[0m\x1b]8;;\x1b\\";

/// The WebShare queue is capped at two detached frames. Keep enough headroom
/// for its opcode, sanitizer state and the companion session-view frame.
pub(crate) const MAX_RECOVERY_KEYFRAME_BYTES: usize = 2 * DEFAULT_MAX_FRAME_LENGTH - 128 * 1024;
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

/// Owned terminal state copied at an atomic pane boundary.
pub(crate) struct PaneRecoverySeed {
    screen: Screen,
    keyframe: PaneRecoveryKeyframe,
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

#[derive(Debug)]
struct RenderedRow {
    bytes: Vec<u8>,
    wrapped: bool,
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
        let keyframe = {
            let renderer = self.projection.recovery_row_renderer(
                MAX_RECOVERY_HYPERLINK_ENTRY_BYTES,
                MAX_RECOVERY_HYPERLINK_TOTAL_BYTES,
            );
            materialize_keyframe(
                &self.projection,
                &renderer,
                &self.pending_bytes,
                &self.active_cell_state,
                &self.saved_cell_state,
                self.saved_cursor,
                &self.parser_state,
                self.history_size,
                self.metadata_complete && renderer.metadata_complete(),
            )
        }?;
        let (screen, _) = self.projection.clone_recovery_viewport_bounded(
            MAX_RECOVERY_STRING_BYTES,
            MAX_RECOVERY_TITLE_STACK_BYTES,
            MAX_RECOVERY_HYPERLINK_ENTRY_BYTES,
            MAX_RECOVERY_HYPERLINK_TOTAL_BYTES,
        );
        Ok(PaneRecoverySeed {
            screen,
            keyframe,
            history_size: self.history_size,
            history_bytes: self.history_bytes,
            alternate: self.projection.is_alternate(),
            output_sequence: self.output_sequence,
        })
    }
}

impl PaneRecoverySeed {
    #[cfg(test)]
    pub(crate) fn capture(transcript: &PaneTranscript) -> Result<Self, RmuxError> {
        PaneRecoveryDraft::capture(transcript)?.materialize()
    }

    pub(crate) const fn screen(&self) -> &Screen {
        &self.screen
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
        self.keyframe.metadata_complete
    }

    pub(crate) fn keyframe(&self) -> PaneRecoveryKeyframe {
        self.keyframe.clone()
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
) -> Result<PaneRecoveryKeyframe, RmuxError> {
    let size = source.size();
    let alternate = source.is_alternate();
    let captured_history_rows = source.history_size();
    let active_visible = render_active_rows(
        renderer,
        captured_history_rows..captured_history_rows.saturating_add(usize::from(size.rows)),
    )?;

    let mut prefix = Vec::new();
    prefix.extend_from_slice(RESET_PREFIX);
    append_title_state(&mut prefix, source);
    append_osc_text(&mut prefix, 7, source.path());
    prefix.extend_from_slice(parser_state);

    let mut mandatory_before_active = Vec::new();
    if alternate {
        let saved_visible = render_saved_rows(renderer, 0..usize::from(size.rows))?;
        append_rows(&mut mandatory_before_active, &saved_visible);
        if let Some((x, y, pending_wrap)) = source.alternate_saved_cursor() {
            append_cursor_state(
                &mut mandatory_before_active,
                source,
                x,
                y,
                pending_wrap,
                Some(&saved_visible),
                None,
            );
        }
        mandatory_before_active.extend_from_slice(if source.alternate_saved_cursor().is_some() {
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

    let mandatory_rows_len = encoded_rows_len(&active_visible);
    let mandatory_len = prefix
        .len()
        .saturating_add(mandatory_before_active.len())
        .saturating_add(mandatory_rows_len)
        .saturating_add(suffix.len());
    if mandatory_len > MAX_RECOVERY_KEYFRAME_BYTES {
        return Err(RmuxError::FrameTooLarge {
            length: mandatory_len,
            maximum: MAX_RECOVERY_KEYFRAME_BYTES,
        });
    }

    let history_budget = MAX_RECOVERY_KEYFRAME_BYTES - mandatory_len;
    let history = recent_history_suffix(renderer, captured_history_rows, history_budget)?;
    let history_boundary_len = if history.back().is_some_and(|row| !row.wrapped) {
        2
    } else {
        0
    };
    let history_len = encoded_rows_len_deque(&history).saturating_add(history_boundary_len);
    if history_len > history_budget {
        return Err(RmuxError::Server(
            "recovery history accounting exceeded its byte budget".to_owned(),
        ));
    }

    let mut bytes = Vec::with_capacity(mandatory_len.saturating_add(history_len));
    bytes.extend_from_slice(&prefix);
    append_rows_deque(&mut bytes, &history);
    if !history.is_empty() && history.back().is_some_and(|row| !row.wrapped) {
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(&mandatory_before_active);
    append_rows(&mut bytes, &active_visible);
    bytes.extend_from_slice(&suffix);
    debug_assert!(bytes.len() <= MAX_RECOVERY_KEYFRAME_BYTES);

    Ok(PaneRecoveryKeyframe {
        cols: size.cols,
        rows: size.rows,
        bytes,
        alternate,
        history_rows_total: u64::try_from(history_rows_total).unwrap_or(u64::MAX),
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

fn render_active_rows(
    renderer: &RecoveryRowRenderer<'_>,
    range: Range<usize>,
) -> Result<Vec<RenderedRow>, RmuxError> {
    render_rows(range, |row| renderer.active_row(row, capture_options()))
}

fn render_saved_rows(
    renderer: &RecoveryRowRenderer<'_>,
    range: Range<usize>,
) -> Result<Vec<RenderedRow>, RmuxError> {
    render_rows(range, |row| renderer.saved_row(row, capture_options()))
}

fn render_rows(
    range: Range<usize>,
    mut render: impl FnMut(usize) -> Option<(Vec<u8>, bool)>,
) -> Result<Vec<RenderedRow>, RmuxError> {
    let mut rows = Vec::with_capacity(range.len());
    let mut encoded_len = 0_usize;
    for row in range {
        let Some((bytes, wrapped)) = render(row) else {
            return Err(RmuxError::Server(format!(
                "terminal row {row} disappeared during recovery capture"
            )));
        };
        let separator = if rows
            .last()
            .is_some_and(|previous: &RenderedRow| !previous.wrapped)
        {
            2
        } else {
            0
        };
        encoded_len = encoded_len
            .saturating_add(separator)
            .saturating_add(ROW_RESET.len())
            .saturating_add(bytes.len());
        if encoded_len > MAX_RECOVERY_KEYFRAME_BYTES {
            return Err(RmuxError::FrameTooLarge {
                length: encoded_len,
                maximum: MAX_RECOVERY_KEYFRAME_BYTES,
            });
        }
        rows.push(RenderedRow { bytes, wrapped });
    }
    Ok(rows)
}

fn recent_history_suffix(
    renderer: &RecoveryRowRenderer<'_>,
    history_rows: usize,
    budget: usize,
) -> Result<VecDeque<RenderedRow>, RmuxError> {
    let mut retained = VecDeque::new();
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
        let Some((group, group_len)) =
            render_history_group_bounded(renderer, start..end, remaining)?
        else {
            break;
        };
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
) -> Result<Option<(Vec<RenderedRow>, usize)>, RmuxError> {
    let mut group = Vec::new();
    let mut encoded_len = 0_usize;
    for absolute_y in range {
        let Some((bytes, wrapped)) = renderer.active_row(absolute_y, capture_options()) else {
            return Err(RmuxError::Server(
                "terminal history changed during recovery capture".to_owned(),
            ));
        };
        let separator = if group
            .last()
            .is_some_and(|previous: &RenderedRow| !previous.wrapped)
        {
            2
        } else {
            0
        };
        let next_len = encoded_len
            .saturating_add(separator)
            .saturating_add(ROW_RESET.len())
            .saturating_add(bytes.len());
        if next_len > budget {
            return Ok(None);
        }
        encoded_len = next_len;
        group.push(RenderedRow { bytes, wrapped });
    }
    if group.last().is_some_and(|row| !row.wrapped) {
        encoded_len = encoded_len.saturating_add(2);
    }
    if encoded_len > budget {
        return Ok(None);
    }
    Ok(Some((group, encoded_len)))
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

fn append_rows(out: &mut Vec<u8>, rows: &[RenderedRow]) {
    for (index, row) in rows.iter().enumerate() {
        if index > 0 && !rows[index - 1].wrapped {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(ROW_RESET);
        out.extend_from_slice(&row.bytes);
    }
}

fn append_rows_deque(out: &mut Vec<u8>, rows: &VecDeque<RenderedRow>) {
    let mut previous_wrapped = None;
    for row in rows {
        if previous_wrapped == Some(false) {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(ROW_RESET);
        out.extend_from_slice(&row.bytes);
        previous_wrapped = Some(row.wrapped);
    }
}

fn encoded_rows_len(rows: &[RenderedRow]) -> usize {
    rows.iter()
        .enumerate()
        .fold(0_usize, |length, (index, row)| {
            length
                .saturating_add(if index > 0 && !rows[index - 1].wrapped {
                    2
                } else {
                    0
                })
                .saturating_add(ROW_RESET.len())
                .saturating_add(row.bytes.len())
        })
}

fn encoded_rows_len_deque(rows: &VecDeque<RenderedRow>) -> usize {
    let mut previous_wrapped = None;
    rows.iter().fold(0_usize, |length, row| {
        let separator = if previous_wrapped == Some(false) {
            2
        } else {
            0
        };
        previous_wrapped = Some(row.wrapped);
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
        let mut transcript = PaneTranscript::new(100, SIZE);
        transcript.append_bytes(initial);
        let keyframe = PaneRecoverySeed::capture(&transcript)
            .expect("capture recovery state")
            .keyframe();

        let mut actual = TerminalScreen::new(SIZE, 100);
        actual.feed(&keyframe.bytes);
        actual.feed(tail);

        let mut expected = TerminalScreen::new(SIZE, 100);
        expected.feed(initial);
        expected.feed(tail);
        (actual, expected)
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
