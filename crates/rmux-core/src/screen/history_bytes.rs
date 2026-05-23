use super::Screen;

impl Screen {
    /// Returns tmux-compatible grid allocation bytes for `#{history_bytes}`.
    #[must_use]
    pub fn tmux_history_bytes(&self) -> usize {
        let counts = self.grid.tmux_history_byte_counts();
        counts[1]
            .saturating_add(counts[3])
            .saturating_add(counts[5])
    }

    /// Returns tmux-compatible grid allocation counters for `#{history_all_bytes}`.
    #[must_use]
    pub fn tmux_history_all_bytes(&self) -> String {
        let [lines, line_bytes, cells, cell_bytes, extended, extended_bytes] =
            self.grid.tmux_history_byte_counts();
        format!("{lines},{line_bytes},{cells},{cell_bytes},{extended},{extended_bytes}")
    }
}
