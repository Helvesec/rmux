use crate::grid::Grid;
use crate::input::{CellState, COLOUR_DEFAULT};
use crate::screen::{SavedGrid, Screen};

impl Screen {
    /// Enters the alternate screen and remembers `rendition` whether or not
    /// the cursor is saved, as tmux 3.7b `screen_alternate_on` copies its
    /// `saved_cell` on every entry.
    pub(super) fn enter_alternate_screen(&mut self, rendition: &CellState, save_cursor: bool) {
        if !self.alternate_screen_enabled {
            return;
        }
        if self.is_alternate() {
            return;
        }
        self.clear_selected_cells();

        let mut saved_grid = Grid::new(self.grid.size(), 0);
        saved_grid.replace_visible(self.grid.visible_lines());
        self.saved_grid = Some(SavedGrid {
            grid: saved_grid,
            history_enabled: self.grid.history_enabled(),
        });
        if save_cursor {
            self.saved_cursor_x = Some(self.cursor_x);
            self.saved_cursor_y = Some(self.cursor_y);
            self.saved_cursor_pending_wrap = self.pending_wrap;
        }
        self.saved_rendition.clone_from(rendition);

        self.grid.clear_visible(rendition.bg());
        self.grid.set_history_enabled(false);
        self.pending_wrap = false;
        if !save_cursor || !self.preserve_alternate_screen_cursor {
            self.cursor_x = 0;
            self.cursor_y = 0;
        }
    }

    /// Leaves the alternate screen. Returns the rendition remembered on entry
    /// when the saved cursor is restored, which tmux 3.7b
    /// `screen_alternate_off` does for `?1049l` even outside the alternate
    /// screen; `?47l` and `?1047l` restore neither.
    pub(super) fn leave_alternate_screen(&mut self, restore_cursor: bool) -> Option<&CellState> {
        self.clear_selected_cells();
        let saved_cursor = if restore_cursor {
            self.saved_cursor_x
                .zip(self.saved_cursor_y)
                .map(|(x, y)| (x, y, self.saved_cursor_pending_wrap))
        } else {
            None
        };

        let Some(saved) = self.saved_grid.take() else {
            if let Some((x, y, pending_wrap)) = saved_cursor {
                self.restore_cursor_position(x, y, pending_wrap);
            }
            return saved_cursor.map(|_| &self.saved_rendition);
        };

        let current_size = self.grid.size();
        let saved_size = saved.grid.size();
        let alternate_cursor = (self.cursor_x, self.cursor_y, self.pending_wrap);
        self.grid
            .restore_visible_at_size(saved_size, saved.grid.visible_lines(), COLOUR_DEFAULT);
        self.grid.set_history_enabled(saved.history_enabled);
        if let Some((x, y, pending_wrap)) = saved_cursor {
            self.restore_cursor_position(x, y, pending_wrap);
        } else {
            self.restore_cursor_position(
                alternate_cursor.0,
                alternate_cursor.1,
                alternate_cursor.2,
            );
        }
        self.resize(current_size);
        saved_cursor.map(|_| &self.saved_rendition)
    }
}
