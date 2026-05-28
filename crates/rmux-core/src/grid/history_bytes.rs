use super::{Grid, GridLine};

const TMUX_GRID_LINE_BYTES: usize = 40;
const TMUX_GRID_CELL_BYTES: usize = 5;
const TMUX_EXTENDED_CELL_BYTES: usize = 23;

impl Grid {
    /// Returns tmux-compatible grid allocation counters for
    /// `#{history_all_bytes}`.
    #[must_use]
    pub fn tmux_history_byte_counts(&self) -> [usize; 6] {
        let line_count = self.history.len() + self.visible.len();
        let cell_count = self
            .history
            .iter()
            .chain(self.visible.iter())
            .map(|line| line.tmux_cell_capacity(self.sx as usize))
            .sum::<usize>();
        let extended_count = self
            .history
            .iter()
            .chain(self.visible.iter())
            .map(GridLine::extended_cell_count)
            .sum::<usize>();

        [
            line_count,
            line_count.saturating_mul(TMUX_GRID_LINE_BYTES),
            cell_count,
            cell_count.saturating_mul(TMUX_GRID_CELL_BYTES),
            extended_count,
            extended_count.saturating_mul(TMUX_EXTENDED_CELL_BYTES),
        ]
    }
}
