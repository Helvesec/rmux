use super::Screen;

impl Screen {
    pub(crate) fn previous_cell_x(&self, y: u32, x: u32) -> u32 {
        let Some(candidate) = x.checked_sub(1) else {
            return 0;
        };
        let Some(line) = self.grid.visible_line(y) else {
            return candidate;
        };
        if line.is_padding_cell(candidate) {
            return line.owning_cell_x(candidate).unwrap_or(candidate);
        }
        candidate
    }

    pub(crate) fn next_cell_x(&self, y: u32, x: u32) -> u32 {
        let max_x = self.grid.sx().saturating_sub(1);
        if x >= max_x {
            return max_x;
        }

        let Some(line) = self.grid.visible_line(y) else {
            return x.saturating_add(1).min(max_x);
        };
        let owner_x = line.owning_cell_x(x).unwrap_or(x);
        let width = line
            .cell(owner_x)
            .map_or(1, |cell| u32::from(cell.width().max(1)));

        owner_x.saturating_add(width).min(max_x)
    }
}
