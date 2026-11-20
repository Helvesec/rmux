//! How far a line's content reaches, how many cells it uses, and what it
//! costs to keep.

use compact_str::CompactString;

use super::{GridCell, GridCellFlags, GridLine};

impl GridLine {
    pub(in crate::grid) fn rendered_text_len(&self) -> usize {
        if let Some(text) = &self.plain_text {
            return text.trim_end_matches(' ').len();
        }

        let mut total = 0_usize;
        let mut trimmed = 0_usize;
        for cell in &self.cells {
            if cell.flags.contains(GridCellFlags::PADDING) {
                continue;
            }
            let text = cell.text.as_str();
            total = total.saturating_add(text.len());
            let without_trailing_spaces = text.trim_end_matches(' ').len();
            if without_trailing_spaces > 0 {
                trimmed = total.saturating_sub(text.len() - without_trailing_spaces);
            }
        }
        trimmed
    }

    pub(in crate::grid) fn recovery_clone_bytes(&self) -> usize {
        let cell_bytes = self
            .cells
            .len()
            .saturating_mul(std::mem::size_of::<GridCell>());
        let extended_cell_text_bytes = self
            .cells
            .iter()
            .filter(|cell| cell.text.extended.is_some())
            .map(|cell| {
                std::mem::size_of::<CompactString>().saturating_add(cell.text.as_str().len())
            })
            .sum::<usize>();
        let plain_text_bytes = self.plain_text.as_ref().map_or(0, CompactString::len);
        std::mem::size_of::<Self>()
            .saturating_add(cell_bytes)
            .saturating_add(extended_cell_text_bytes)
            .saturating_add(plain_text_bytes)
    }

    /// The logical occupancy of the line: how far written content reaches.
    ///
    /// A cell that only carries an erase background is not written content, so
    /// a whole-screen erase under a non-default background leaves this at zero
    /// exactly as it does under the default one. Transfer to scrollback,
    /// reflow and the resize fast paths ask this question.
    pub(in crate::grid) fn used_end(&self) -> usize {
        if let Some(text) = &self.plain_text {
            return text.len();
        }
        self.cells
            .iter()
            .rposition(|cell| !cell.is_logically_empty())
            .map_or(0, |index| index + 1)
    }

    /// How far materialised cells reach, painted backgrounds included.
    ///
    /// Storage, capture, the compaction caches and the reflow-gap
    /// preconditions ask this question instead: they must keep the cells an
    /// erase painted, which is what makes a coloured blank viewport render as
    /// full-width coloured spaces.
    pub(in crate::grid) fn painted_end(&self) -> usize {
        if let Some(text) = &self.plain_text {
            return text.len();
        }
        self.cells
            .iter()
            .rposition(|cell| !cell.is_blank())
            .map_or(0, |index| index + 1)
    }

    pub(in crate::grid) fn tmux_cell_capacity(&self, line_width: usize) -> usize {
        let used_end = self.painted_end();
        if used_end == 0 {
            return 0;
        }

        let bucket_used = self.extended_cell_count().max(1);
        let quarter = (line_width / 4).max(1);
        let half = (line_width / 2).max(quarter);
        if bucket_used < quarter {
            quarter
        } else if bucket_used < half {
            half
        } else {
            line_width
        }
    }

    pub(in crate::grid) fn extended_cell_count(&self) -> usize {
        if let Some(text) = &self.plain_text {
            return text.len();
        }
        self.cells[..self.painted_end()]
            .iter()
            .filter(|cell| !cell.is_blank() && !cell.is_padding())
            .count()
    }
}
