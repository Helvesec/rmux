//! Delta between two pane snapshots: full refresh, plain cursor append,
//! single-line scroll or per-row repaint.
use super::*;

impl PaneRenderSnapshot {
    pub(crate) fn diff_to(&self, next: &Self) -> PaneRenderDelta {
        if self.requires_full_refresh(next) {
            return PaneRenderDelta::RequiresFullRefresh;
        }
        if let Some(frame) = self.diff_plain_cursor_append_to(next) {
            return PaneRenderDelta::Incremental(frame);
        }
        if let Some(frame) = self.diff_scroll_up_to(next) {
            return PaneRenderDelta::Incremental(frame);
        }

        let mut frame = Vec::new();
        let changed_rows = self.lines.len().max(next.lines.len());
        for row in 0..changed_rows {
            let previous_line = self.lines.get(row);
            let next_line = next.lines.get(row);
            if previous_line == next_line {
                continue;
            }
            for span in next.row_spans.iter().filter(|span| span.line_index == row) {
                if frame.is_empty() {
                    frame.extend_from_slice(b"\x1b[s\x1b[?25l");
                }
                append_projected_snapshot_span(&mut frame, next, span, next_line);
            }
        }

        if self.scrollbar != next.scrollbar {
            if frame.is_empty() {
                frame.extend_from_slice(b"\x1b[s\x1b[?25l");
            }
            if let Some(scrollbar) = next.scrollbar.as_ref() {
                frame.extend_from_slice(scrollbar.frame().as_slice());
            }
        }

        if !frame.is_empty() {
            frame.extend_from_slice(b"\x1b[0m\x1b[u");
            next.append_final_cursor_state(&mut frame);
        } else if self.cursor_visibility_changed(next) {
            next.append_final_cursor_state(&mut frame);
        } else if self.cursor != next.cursor {
            frame.extend_from_slice(&next.cursor);
        }

        PaneRenderDelta::Incremental(PaneRenderDeltaFrame {
            frame,
            cursor_style: (self.cursor_style != next.cursor_style).then_some(next.cursor_style),
        })
    }

    fn requires_full_refresh(&self, next: &Self) -> bool {
        self.x != next.x
            || self.y != next.y
            || self.rows != next.rows
            || self.cols != next.cols
            || self.source_row_start != next.source_row_start
            || self.source_width != next.source_width
            || self.row_spans != next.row_spans
    }

    fn diff_plain_cursor_append_to(&self, next: &Self) -> Option<PaneRenderDeltaFrame> {
        if !self.has_rectangular_projection()
            || !next.has_rectangular_projection()
            || self.x != 0
            || next.x != 0
            || next.cols != next.terminal_cols
            || self.cursor_style != next.cursor_style
            || self.cursor_row < self.y
            || self.cursor_col < self.x
        {
            return None;
        }

        let row = usize::from(self.cursor_row - self.y);
        let rows = usize::from(next.rows);
        if row >= rows || row >= self.lines.len() || row >= next.lines.len() {
            return None;
        }

        let previous_line = &self.lines[row];
        let next_line = &next.lines[row];
        if !is_plain_ascii_render_line(previous_line.bytes())
            || !is_plain_ascii_render_line(next_line.bytes())
        {
            return None;
        }

        let previous_visible = usize::from(previous_line.columns());
        let next_visible = usize::from(next_line.columns());
        let previous_cursor_col = usize::from(self.cursor_col - self.x);
        if previous_cursor_col != previous_visible || next_visible <= previous_visible {
            return None;
        }
        if next_line.bytes().get(..previous_visible)
            != Some(&previous_line.bytes()[..previous_visible])
        {
            return None;
        }
        if other_rows_changed(&self.lines, &next.lines, row) {
            return None;
        }

        let same_line_cursor = next.cursor_row == self.cursor_row
            && next.cursor_col >= next.x
            && usize::from(next.cursor_col - next.x) == next_visible;
        let next_line_cursor = next.cursor_row == self.cursor_row.saturating_add(1)
            && next.cursor_col == next.x
            && row + 1 < rows;
        if !same_line_cursor && !next_line_cursor {
            return None;
        }

        let mut frame = next_line.bytes()[previous_visible..next_visible].to_vec();
        if next_line_cursor {
            frame.extend_from_slice(b"\r\n");
        }
        if self.cursor_visibility_changed(next) {
            next.append_final_cursor_state(&mut frame);
        }
        Some(PaneRenderDeltaFrame {
            frame,
            cursor_style: None,
        })
    }

    fn diff_scroll_up_to(&self, next: &Self) -> Option<PaneRenderDeltaFrame> {
        if !self.has_rectangular_projection()
            || !next.has_rectangular_projection()
            || self.x != 0
            || next.x != 0
            || next.cols != next.terminal_cols
            || self.rows == 0
            || self.lines.len() != next.lines.len()
            || self.line_revisions.len() != next.line_revisions.len()
        {
            return None;
        }

        let rows = usize::from(next.rows).min(next.lines.len());
        let scroll_rows = (1..rows).find(|scroll_rows| {
            self.line_revisions[*scroll_rows..rows] == next.line_revisions[..rows - *scroll_rows]
                && self.lines[*scroll_rows..rows] == next.lines[..rows - *scroll_rows]
                && self.lines[*scroll_rows..rows]
                    .iter()
                    .any(|line| line.columns() > 0)
        })?;
        if next.lines[rows - scroll_rows..rows]
            .iter()
            .all(|line| line.columns() == 0)
            && self.cursor == next.cursor
        {
            return None;
        }

        let mut frame = Vec::new();
        frame.extend_from_slice(b"\x1b[s\x1b[?25l");
        write!(
            &mut frame,
            "\x1b[{};{}r\x1b[{}S\x1b[r",
            next.y.saturating_add(1),
            next.y.saturating_add(next.rows),
            scroll_rows
        )
        .expect("writing to Vec cannot fail");
        for row in rows - scroll_rows..rows {
            frame.extend_from_slice(
                cursor_position_bytes(next.y.saturating_add(row as u16), next.x).as_slice(),
            );
            frame.extend_from_slice(b"\x1b[0m");
            frame.extend_from_slice(next.lines[row].bytes());
            append_pane_row_tail(
                &mut frame,
                next.y.saturating_add(row as u16),
                next.x,
                next.cols,
                next.terminal_cols,
                next.lines[row].columns(),
            );
        }
        frame.extend_from_slice(b"\x1b[0m\x1b[u");
        next.append_final_cursor_state(&mut frame);

        Some(PaneRenderDeltaFrame {
            frame,
            cursor_style: (self.cursor_style != next.cursor_style).then_some(next.cursor_style),
        })
    }
}

fn other_rows_changed(
    previous: &[Arc<RenderedPaneRow>],
    next: &[Arc<RenderedPaneRow>],
    changed_row: usize,
) -> bool {
    let rows = previous.len().max(next.len());
    for row in 0..rows {
        if row == changed_row {
            continue;
        }
        if previous.get(row) != next.get(row) {
            return true;
        }
    }
    false
}
