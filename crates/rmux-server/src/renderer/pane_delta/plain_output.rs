//! Forwarding plain output bytes straight onto a snapshot instead of
//! re-rendering the pane.
use super::*;

impl PaneRenderSnapshot {
    pub(crate) fn can_forward_plain_bytes(&self, bytes: &[u8]) -> bool {
        if !self.has_rectangular_projection() || self.x != 0 || self.cols != self.terminal_cols {
            return false;
        }
        let Some(would_scroll) = self.plain_bytes_would_scroll(bytes) else {
            return false;
        };
        !would_scroll || self.covers_full_terminal_height()
    }

    fn can_apply_plain_bytes_to_snapshot(&self, bytes: &[u8]) -> bool {
        if !self.has_rectangular_projection() {
            return false;
        }
        self.plain_bytes_would_scroll(bytes).is_some()
    }

    fn covers_full_terminal_height(&self) -> bool {
        self.y == 0 && self.rows == self.terminal_rows
    }

    pub(super) fn has_rectangular_projection(&self) -> bool {
        self.source_row_start == 0
            && self.source_width == self.cols
            && self.row_spans.len() == usize::from(self.rows)
            && self.row_spans.iter().enumerate().all(|(row, span)| {
                span.line_index == row
                    && span.source_row == row
                    && span.source_col == 0
                    && span.x == self.x
                    && span.y == self.y.saturating_add(row as u16)
                    && span.cols == self.cols
            })
    }

    fn plain_bytes_would_scroll(&self, bytes: &[u8]) -> Option<bool> {
        let mut row = self.cursor_row.saturating_sub(self.y);
        let mut col = self.cursor_col.saturating_sub(self.x);
        if row >= self.rows || col >= self.cols {
            return None;
        }

        let mut index = 0;
        let mut would_scroll = false;
        while index < bytes.len() {
            match bytes[index] {
                b'\r' => {
                    col = 0;
                    index += 1;
                }
                b'\n' => {
                    if row.saturating_add(1) < self.rows {
                        row = row.saturating_add(1);
                    } else {
                        would_scroll = true;
                    }
                    index += 1;
                }
                b' '..=b'~' => {
                    if col >= self.cols {
                        return None;
                    }
                    col = col.saturating_add(1);
                    index += 1;
                }
                _ => return None,
            }
        }

        Some(would_scroll)
    }

    #[cfg(test)]
    pub(crate) fn positioned_plain_echo_frame(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        if !self.has_rectangular_projection() {
            return None;
        }
        if bytes.is_empty() || !bytes.iter().all(|byte| matches!(*byte, b' '..=b'~')) {
            return None;
        }
        let row = self.cursor_row.checked_sub(self.y)?;
        let col = self.cursor_col.checked_sub(self.x)?;
        if row >= self.rows || col >= self.cols {
            return None;
        }
        let next_col = col.checked_add(bytes.len().try_into().ok()?)?;
        if next_col > self.cols {
            return None;
        }
        let line = self.lines.get(usize::from(row))?;
        if !is_plain_ascii_render_line(line.bytes()) {
            return None;
        }

        let mut frame = Vec::with_capacity(bytes.len().saturating_add(32));
        frame.extend_from_slice(&cursor_position_bytes(self.cursor_row, self.cursor_col));
        frame.extend_from_slice(bytes);
        frame.extend_from_slice(&cursor_position_bytes(self.cursor_row, self.x + next_col));
        Some(frame)
    }

    pub(crate) fn positioned_plain_output_frame(&mut self, bytes: &[u8]) -> Option<Vec<u8>> {
        if !self.has_rectangular_projection() {
            return None;
        }
        if bytes.is_empty() || self.x == 0 && self.cols == self.terminal_cols {
            return None;
        }
        if !self.can_apply_plain_bytes_to_snapshot(bytes)
            || self
                .lines
                .iter()
                .any(|line| !is_plain_ascii_render_line(line.bytes()))
        {
            return None;
        }

        let mut row = self.cursor_row.checked_sub(self.y)?;
        let mut col = self.cursor_col.checked_sub(self.x)?;
        let mut frame = Vec::with_capacity(bytes.len().saturating_add(64));
        frame.extend_from_slice(b"\x1b[s\x1b[?25l\x1b[0m");
        let mut touched_rows = Vec::new();
        let mut index = 0;
        let original = self.clone();
        while index < bytes.len() {
            match bytes[index] {
                b'\r' => {
                    col = 0;
                    index += 1;
                }
                b'\n' => {
                    if row.saturating_add(1) >= self.rows {
                        return self.restore_positioned_plain_output(original);
                    } else {
                        row = row.saturating_add(1);
                    }
                    index += 1;
                }
                b' '..=b'~' => {
                    let run_end = bytes[index..]
                        .iter()
                        .position(|byte| !matches!(*byte, b' '..=b'~'))
                        .map_or(bytes.len(), |offset| index + offset);
                    let Ok(run_len) = u16::try_from(run_end - index) else {
                        return self.restore_positioned_plain_output(original);
                    };
                    if col.saturating_add(run_len) > self.cols {
                        return self.restore_positioned_plain_output(original);
                    }
                    frame.extend_from_slice(&cursor_position_bytes(
                        self.y.saturating_add(row),
                        self.x.saturating_add(col),
                    ));
                    frame.extend_from_slice(&bytes[index..run_end]);
                    if self
                        .apply_plain_run_to_snapshot(
                            usize::from(row),
                            usize::from(col),
                            &bytes[index..run_end],
                            &mut touched_rows,
                        )
                        .is_none()
                    {
                        return self.restore_positioned_plain_output(original);
                    }
                    col = col.saturating_add(run_len);
                    index = run_end;
                }
                _ => return self.restore_positioned_plain_output(original),
            }
        }

        for row_index in touched_rows {
            if let Some(revision) = self.line_revisions.get_mut(row_index) {
                *revision = revision.saturating_add(1);
            }
        }
        self.cursor_row = self.y.saturating_add(row);
        self.cursor_col = self.x.saturating_add(col);
        replace_cursor_position_bytes(&mut self.cursor, self.cursor_row, self.cursor_col);
        frame.extend_from_slice(b"\x1b[0m\x1b[u");
        self.append_final_cursor_state(&mut frame);
        Some(frame)
    }

    fn restore_positioned_plain_output(&mut self, original: Self) -> Option<Vec<u8>> {
        *self = original;
        None
    }

    pub(crate) fn apply_forwarded_plain_bytes(&mut self, bytes: &[u8]) -> bool {
        if !self.can_apply_plain_bytes_to_snapshot(bytes) {
            return false;
        }

        let mut row = self.cursor_row.saturating_sub(self.y);
        let mut col = self.cursor_col.saturating_sub(self.x);
        let mut touched_rows = Vec::new();
        for byte in bytes {
            match *byte {
                b'\r' => col = 0,
                b'\n' => {
                    if row.saturating_add(1) >= self.rows {
                        self.scroll_snapshot_up_one();
                        row = self.rows.saturating_sub(1);
                    } else {
                        row = row.saturating_add(1);
                    }
                }
                b' '..=b'~' => {
                    if self
                        .apply_plain_run_to_snapshot(
                            usize::from(row),
                            usize::from(col),
                            std::slice::from_ref(byte),
                            &mut touched_rows,
                        )
                        .is_none()
                    {
                        return false;
                    }
                    col = col.saturating_add(1);
                }
                _ => return false,
            }
        }
        for row_index in touched_rows {
            if let Some(revision) = self.line_revisions.get_mut(row_index) {
                *revision = revision.saturating_add(1);
            }
        }

        self.cursor_row = self.y.saturating_add(row);
        self.cursor_col = self.x.saturating_add(col);
        replace_cursor_position_bytes(&mut self.cursor, self.cursor_row, self.cursor_col);
        true
    }

    fn apply_plain_run_to_snapshot(
        &mut self,
        row_index: usize,
        col_index: usize,
        bytes: &[u8],
        touched_rows: &mut Vec<usize>,
    ) -> Option<()> {
        let cols = usize::from(self.cols);
        let end = col_index.checked_add(bytes.len())?;
        if row_index >= self.lines.len() || end > cols {
            return None;
        }
        let line = Arc::make_mut(&mut self.lines[row_index]);
        let line_bytes = line.bytes_mut();
        if line_bytes.len() < end {
            line_bytes.resize(end, b' ');
        }
        line_bytes[col_index..end].copy_from_slice(bytes);
        line.extend_columns_to(u16::try_from(end).ok()?);
        if touched_rows.last().copied() != Some(row_index) && !touched_rows.contains(&row_index) {
            touched_rows.push(row_index);
        }
        Some(())
    }

    fn scroll_snapshot_up_one(&mut self) {
        if self.lines.is_empty() {
            return;
        }
        let _ = self.lines.remove(0);
        self.lines
            .push(Arc::new(RenderedPaneRow::new(Vec::new(), 0)));
        if !self.line_revisions.is_empty() {
            let _ = self.line_revisions.remove(0);
            self.line_revisions
                .push(next_synthetic_line_revision(&self.line_revisions));
        }
    }
}
