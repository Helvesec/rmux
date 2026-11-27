use super::Screen;

impl Screen {
    /// Joins the preceding visible row to the cursor row without moving the
    /// cursor or changing its pending wrap or saved state.
    pub fn mark_previous_line_wrapped(&mut self) {
        let Some(previous_y) = self.cursor_y.checked_sub(1) else {
            return;
        };
        if let Some(previous) = self.grid.visible_line_mut(previous_y) {
            previous.set_wrapped(true);
        }
    }
}
