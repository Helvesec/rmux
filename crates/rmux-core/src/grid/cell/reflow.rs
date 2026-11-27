use super::{GridCell, GridCellFlags, GridLine, GridLineFlags, HiddenColumns};
use crate::input::Colour;

impl GridLine {
    /// Re-widths an alternate-screen row without reflow, as tmux 3.7b's
    /// `screen_resize` does while a saved grid exists: narrowing hides the
    /// columns past the new edge rather than dropping or rewrapping them, and
    /// widening shows hidden columns again at their own positions.
    pub(in crate::grid) fn resize_width_hiding_columns(&mut self, width: u32, bg: Colour) {
        let old_width = self.width;
        let mut hidden = self
            .hidden_columns
            .take()
            .map_or_else(Vec::new, |columns| columns.cells);
        if width < old_width {
            let mut cut = (width..old_width)
                .map(|x| self.cell(x).unwrap_or_default())
                .collect::<Vec<_>>();
            cut.append(&mut hidden);
            hidden = cut;
        }
        self.resize_width_preserving_wrap(width, bg);
        if width > old_width && !hidden.is_empty() {
            let shown = hidden.len().min((width - old_width) as usize);
            self.materialize_for_cell_mutation();
            let start = old_width as usize;
            for (target, cell) in self.cells.iter_mut().skip(start).zip(hidden.drain(..shown)) {
                *target = cell;
            }
            self.touch();
        }
        while hidden
            .last()
            .is_some_and(|cell| *cell == GridCell::default())
        {
            hidden.pop();
        }
        if !hidden.is_empty() {
            self.hidden_columns = Some(Box::new(HiddenColumns { cells: hidden }));
        }
    }

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
