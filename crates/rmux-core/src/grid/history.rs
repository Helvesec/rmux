//! Scrollback collection, capacity and cached byte accounting.

use super::{cell, Grid, GridLine, VecDeque};

const HISTORY_STAMP_REFRESH_LINES: u16 = 256;

impl Grid {
    pub(super) fn effective_history_capacity(&self) -> usize {
        self.hlimit.max(self.reflow_history_capacity)
    }

    pub(super) fn push_history(&mut self, line: GridLine) {
        if !self.history_enabled {
            return;
        }
        // Normal terminal output consumes any unused resize-restoration
        // budget. Keep only the overflow rows that are still in history before
        // inserting the new row.
        if self.reflow_history_capacity > 0 {
            self.reflow_history_capacity = if self.history.len() > self.hlimit {
                self.history.len()
            } else {
                0
            };
        }
        // Collection that is on always has room for the row it is transferring:
        // tmux evicts the oldest row and appends the new one, so a pane moved to
        // zero keeps the last row a clear or a scroll just handed over
        // (grid-view.c:66-90,106-121 with grid.c:432-444). This minimum belongs
        // to normal collection only; `effective_history_capacity` keeps its own
        // zero and the height helper keeps its policy.
        let capacity = self.effective_history_capacity().max(1);
        // tmux grid_collect_history(all=0) collects the oldest ten percent
        // before normal scroll/clear insertion. Resize has a separate policy.
        self.insert_history_row(line, capacity, (self.hlimit / 10).max(1));
    }

    pub(super) fn push_history_preserving_reflow_capacity(&mut self, line: GridLine) {
        if self.hlimit == 0 {
            return;
        }
        // Height shrink redistributes rows already stored by the grid. Like
        // tmux screen_resize_y, retain every row even above hlimit; growth can
        // pull them back. Leave the reflow budget and normal collection policy
        // unchanged: neither is a capacity limit for this transfer.
        let history_capacity = self.history.len().saturating_add(1);
        self.insert_history_row(line, history_capacity, 1);
    }

    fn insert_history_row(
        &mut self,
        mut line: GridLine,
        history_capacity: usize,
        collection_batch: usize,
    ) {
        if history_capacity == 0 {
            return;
        }

        line.stamp_for_history_at(self.next_history_stamp());
        line.compact_for_history();
        if self.history.len() >= history_capacity {
            for _ in 0..collection_batch.min(self.history.len()) {
                let _ = self.pop_history_front();
            }
            self.hscrolled = self.hscrolled.min(self.history.len());
        }
        self.history_content_bytes = self
            .history_content_bytes
            .saturating_add(history_line_content_bytes(&line));
        self.history.push_back(line);
        self.hscrolled = (self.hscrolled + 1).min(self.history.len());
    }

    pub(super) fn pop_history_front(&mut self) -> Option<GridLine> {
        let line = self.history.pop_front()?;
        self.history_content_bytes = self
            .history_content_bytes
            .saturating_sub(history_line_content_bytes(&line));
        Some(line)
    }

    pub(super) fn pop_history_back(&mut self) -> Option<GridLine> {
        let line = self.history.pop_back()?;
        self.history_content_bytes = self
            .history_content_bytes
            .saturating_sub(history_line_content_bytes(&line));
        Some(line)
    }

    pub(super) fn recompute_history_content_bytes(&mut self) {
        self.history_content_bytes = self.history.iter().map(history_line_content_bytes).sum();
    }

    fn next_history_stamp(&mut self) -> i64 {
        if self.history_stamp_remaining == 0 {
            self.history_stamp = cell::current_unix_timestamp();
            self.history_stamp_remaining = HISTORY_STAMP_REFRESH_LINES;
        }
        self.history_stamp_remaining = self.history_stamp_remaining.saturating_sub(1);
        self.history_stamp
    }
}

pub(super) fn history_line_content_bytes(line: &GridLine) -> usize {
    line.rendered_text_len().saturating_add(1)
}

pub(super) fn compacted_history(lines: Vec<GridLine>) -> VecDeque<GridLine> {
    lines
        .into_iter()
        .map(|mut line| {
            line.compact_for_history();
            line
        })
        .collect()
}
