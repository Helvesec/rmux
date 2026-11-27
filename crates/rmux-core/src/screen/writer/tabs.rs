use crate::screen::Screen;

pub(super) fn cursor_backward_tab_steps(n: u32, current: u32) -> u32 {
    n.max(1).min(current.saturating_add(1))
}

impl Screen {
    pub(super) fn move_to_next_tab_stop(&mut self) {
        self.clear_pending_wrap();
        let start = self.cursor_column();
        let next = ((start + 1) as usize..self.tabs.len())
            .find(|index| self.tabs[*index])
            .map(|index| index as u32)
            .unwrap_or_else(|| self.grid.sx().saturating_sub(1));
        self.cursor_x = next;
    }

    pub(super) fn move_back_tab_stops(&mut self, n: u32) {
        self.clear_pending_wrap();
        let mut current = self.cursor_column();
        for _ in 0..cursor_backward_tab_steps(n, current) {
            if current == 0 {
                break;
            }
            let previous = (0..current as usize)
                .rev()
                .find(|index| self.tabs[*index])
                .map(|index| index as u32)
                .unwrap_or(0);
            current = previous;
        }
        self.cursor_x = current;
    }

    pub(super) fn set_tab_stop_at_cursor(&mut self) {
        let column = self.cursor_column() as usize;
        if let Some(tab) = self.tabs.get_mut(column) {
            *tab = true;
        }
    }

    pub(super) fn clear_tab_stop_at_cursor(&mut self) {
        let column = self.cursor_column() as usize;
        if let Some(tab) = self.tabs.get_mut(column) {
            *tab = false;
        }
    }
}
