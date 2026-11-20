use super::GridLine;

impl GridLine {
    pub(crate) fn recovery_written_columns(&self) -> Option<u32> {
        self.plain_text
            .as_ref()
            .map(|text| u32::from(self.plain_text_data_end).max(text.len() as u32))
    }

    /// Remove only default-space replay padding, never glyphs, styles or gaps.
    pub(crate) fn restore_recovery_written_columns(&mut self, columns: u32) {
        let Some(text) = self.plain_text.as_ref() else {
            return;
        };
        if columns < text.len() as u32
            || columns > self.width
            || columns > u32::from(self.plain_text_data_end)
        {
            return;
        }
        self.cells.clear();
        self.plain_text_data_end = columns as u16;
        self.touch();
    }
}
