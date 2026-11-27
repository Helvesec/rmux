//! Bounded capture of active, saved and recent history rows for a recovery
//! keyframe.

use std::collections::VecDeque;
use std::ops::Range;

use rmux_core::{RecoveryRow, RecoveryRowRenderer};
use rmux_proto::RmuxError;

use super::rows::{link_row_pair, link_rows, RenderedRow};
use super::{capture_options, LINE_BREAK, MAX_RECOVERY_KEYFRAME_BYTES, ROW_RESET};

pub(super) fn render_active_rows(
    renderer: &RecoveryRowRenderer<'_>,
    range: Range<usize>,
    cols: u32,
) -> Result<Vec<RenderedRow>, RmuxError> {
    render_rows(range, cols, |row| {
        renderer.active_row(row, capture_options())
    })
}

pub(super) fn render_saved_rows(
    renderer: &RecoveryRowRenderer<'_>,
    range: Range<usize>,
    cols: u32,
) -> Result<Vec<RenderedRow>, RmuxError> {
    render_rows(range, cols, |row| {
        renderer.saved_row(row, capture_options())
    })
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

pub(super) fn recent_history_suffix(
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
            let Some(previous_wrapped) = renderer.recovery_history_row_wrapped(start - 1) else {
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
