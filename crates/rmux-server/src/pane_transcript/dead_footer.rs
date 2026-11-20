use super::PaneTranscript;

impl PaneTranscript {
    /// Draws a preformatted dead-pane footer on the last row. The wrap mark
    /// comes after the text because writing at column zero clears that mark.
    pub(crate) fn append_dead_pane_footer(&mut self, footer: &[u8]) {
        let rows = self.terminal.screen().size().rows.max(1);
        self.append_bytes(format!("\x1b[0m\x1b[r\x1b[{rows};1H\n\x1b[2K").as_bytes());
        self.append_bytes(footer);
        self.terminal.screen_mut().mark_previous_line_wrapped();
    }
}
