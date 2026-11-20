use super::{GridCellFlags, GridLine, GridLineFlags};

impl GridLine {
    /// Where a wrapped row joins the next one when reflowed, as tmux's
    /// `cellused`: a compact ASCII row after its written cells, written
    /// trailing spaces included; a cell row at its full width, which keeps
    /// the blanks DCH and ICH leave in a wrapped row as data.
    pub(in crate::grid) fn wrapped_end(&self) -> usize {
        match &self.plain_text {
            Some(text) => usize::from(self.plain_text_data_end).max(text.len()),
            None => self.width() as usize,
        }
    }

    pub(in crate::grid) fn mark_reflow_gap(&mut self, start: u32) {
        self.materialize_for_cell_mutation();
        for cell in self.cells.iter_mut().skip(start as usize) {
            debug_assert!(cell.is_blank());
            let mut flags = cell.flags();
            flags.insert(GridCellFlags::REFLOW_GAP);
            cell.set_flags(flags);
        }
    }

    /// Marks only the unused physical suffix skipped before a wide pre-wrap.
    pub(crate) fn mark_unused_suffix_as_reflow_gap(&mut self, start: u32) {
        // The text cache trims trailing spaces, but materialization restores
        // them as explicit, non-blank cells through plain_text_data_end.
        let materialized_end = if let Some(text) = &self.plain_text {
            text.len()
                .max(usize::from(self.plain_text_data_end).min(self.width as usize))
        } else {
            self.painted_end()
        };
        if materialized_end <= start as usize {
            self.mark_reflow_gap(start);
        }
    }

    pub(super) fn restore_trailing_reflow_gaps(&mut self, count: usize) {
        self.materialize_reflow_gaps();
        for cell in self.cells.iter_mut().rev().take(count) {
            debug_assert!(cell.is_blank());
            let mut flags = cell.flags();
            flags.insert(GridCellFlags::REFLOW_GAP);
            cell.set_flags(flags);
        }
    }

    pub(super) fn materialize_reflow_gaps(&mut self) {
        for cell in &mut self.cells {
            let mut flags = cell.flags();
            if flags.contains(GridCellFlags::REFLOW_GAP) {
                flags.remove(GridCellFlags::REFLOW_GAP);
                cell.set_flags(flags);
            }
        }
    }

    /// Returns the unused columns a wide glyph left at the end of the line
    /// when it did not fit and pre-wrapped onto the following row.
    pub(in crate::grid) fn trailing_reflow_gap(&self) -> u32 {
        let width = self.width as usize;
        let gap = self
            .cells
            .iter()
            .take(width)
            .rev()
            .take_while(|cell| cell.is_reflow_gap())
            .count();
        u32::try_from(gap).unwrap_or(u32::MAX)
    }

    pub(in crate::grid) fn reflow_logical_width(&self) -> usize {
        // Only a wrapped row joins its successor at the written extent.
        // An unwrapped row still spans its physical columns, whether its
        // cells are compact ASCII or materialized.
        let end = if self.flags.contains(GridLineFlags::WRAPPED) {
            self.wrapped_end()
        } else {
            self.width as usize
        };
        self.reflow_logical_column(end)
    }

    pub(in crate::grid) fn reflow_logical_column(&self, physical_column: usize) -> usize {
        let physical_column = physical_column.min(self.width as usize);
        physical_column.saturating_sub(
            self.cells
                .iter()
                .take(physical_column)
                .filter(|cell| cell.is_reflow_gap())
                .count(),
        )
    }
}
