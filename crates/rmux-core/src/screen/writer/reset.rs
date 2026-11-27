use crate::grid::{GridCell, GridCellFlags};
use crate::input::mode;
use crate::input::{CellState, COLOUR_DEFAULT};
use crate::screen::Screen;

impl Screen {
    pub(super) fn fill_alignment_pattern(&mut self) {
        self.clear_selected_cells();
        self.rupper = 0;
        self.rlower = self.grid.sy().saturating_sub(1);
        let sx = self.grid.sx();
        for y in 0..self.grid.sy() {
            if let Some(line) = self.grid.visible_line_mut(y) {
                for x in 0..sx {
                    if let Some(cell) = line.cell_mut(x) {
                        *cell = GridCell::from_state(
                            'E',
                            1,
                            &CellState::default(),
                            GridCellFlags::default(),
                        );
                    }
                }
                line.set_wrapped(false);
                line.touch();
            }
        }
    }

    pub(super) fn reset_to_initial_state(&mut self) {
        // RIS resets the screen it reaches and never leaves the alternate
        // screen: tmux 3.7b's `screen_write_reset` keeps the saved principal
        // grid, its history and the `?1049h` cursor, so a later `?1049l`
        // restores them. Leaving here would hand that saved grid to the erase
        // below and destroy the shell's screen under a full-screen `reset`.
        self.keyboard.ris();
        self.end_synchronized_output();
        self.clear_selected_cells();
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.pending_wrap = false;
        self.rupper = 0;
        self.rlower = self.grid.sy().saturating_sub(1);
        self.mode = mode::MODE_CURSOR | mode::MODE_WRAP | (self.mode & mode::MODE_CRLF);
        if self.cursor_style != 0 {
            // RIS keeps the selected shape but returns it to steady. Retaining
            // an explicit reset override preserves the raw DECSCUSR value for
            // DTO consumers while producing the measured effective style.
            self.mode |= mode::MODE_CURSOR_BLINKING_SET;
        }
        // `RIS` is a whole-screen erase, so with `scroll-on-clear` on it
        // transfers the used visible rows the way tmux 3.7b's
        // `screen_write_clearscreen` does. The grid only transfers while it
        // keeps history, so the alternate rows are blanked in place. `off`
        // deliberately stays on `clear_visible` rather than the erase path's
        // `clear_visible_without_history`, whose wrapped-boundary break would
        // be a second change.
        if self.scroll_on_clear {
            self.grid.clear_visible_to_history(COLOUR_DEFAULT);
        } else {
            self.grid.clear_visible(COLOUR_DEFAULT);
        }
        self.reset_tabs();
        self.title_stack.clear();
        self.program_status.clear();
        // RIS closes the open link but keeps the hyperlink table, as tmux
        // 3.7b's `screen_write_reset` does: the saved principal grid and the
        // rows `scroll-on-clear` moved to history still reference its entries.
        // Only respawn and `clear-history -H` empty it.
        self.active_hyperlink = 0;
        self.bump_metadata_revision();
    }
}
