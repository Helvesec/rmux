use crate::grid::{GridCell, GridCellFlags};
use crate::input::COLOUR_DEFAULT;

use super::Screen;

impl Screen {
    /// Returns the start of the final tmux `tty_draw_line` empty run.
    ///
    /// This tracks the tty-draw predicate over physical cells so callers can
    /// replace only a trailing empty run without losing wide-cell boundaries.
    #[must_use]
    pub fn visible_line_tmux_empty_tail_start(&self, row: usize, cols: usize) -> Option<u16> {
        let line = self
            .grid
            .visible_line(u32::try_from(row).unwrap_or(u32::MAX))?;
        let mut column = 0_usize;
        let mut last_bg = COLOUR_DEFAULT;
        let mut empty_run_start = cols;
        let mut in_empty_run = false;
        while column < cols {
            let cell = line
                .cell(u32::try_from(column).unwrap_or(u32::MAX))
                .unwrap_or_else(GridCell::default);
            let remaining = cols.saturating_sub(column);
            let cell_width = usize::from(cell.width().max(1));
            let empty_width = if cell_width > remaining {
                remaining
            } else if cell.is_padding() {
                1
            } else if cell.bg() == last_bg && cell.attr() == 0 && cell.link() == 0 {
                if cell.flags().contains(GridCellFlags::CLEARED) {
                    1
                } else if cell.flags().contains(GridCellFlags::TAB) {
                    cell_width
                } else if cell.text() == " " {
                    1
                } else {
                    0
                }
            } else {
                0
            };
            let next_is_empty = empty_width != 0;
            if next_is_empty != in_empty_run {
                empty_run_start = column;
                in_empty_run = next_is_empty;
            }

            last_bg = cell.bg();
            let consumed_columns = if next_is_empty {
                empty_width
            } else {
                cell_width
            };
            column = column.saturating_add(consumed_columns);
        }

        let tail_start = if in_empty_run { empty_run_start } else { cols };
        Some(u16::try_from(tail_start).unwrap_or(u16::MAX))
    }
}

#[cfg(test)]
mod tests {
    use rmux_proto::TerminalSize;

    use crate::{input::InputParser, Screen};

    fn tail_start(bytes: &[u8]) -> u16 {
        let size = TerminalSize { cols: 20, rows: 1 };
        let mut screen = Screen::new(size, 100);
        let mut parser = InputParser::new();
        parser.parse(bytes, &mut screen);
        screen
            .visible_line_tmux_empty_tail_start(0, usize::from(size.cols))
            .expect("visible row")
    }

    #[test]
    fn tmux_empty_tail_uses_space_background_attribute_and_link_predicate() {
        assert_eq!(tail_start(b"A       "), 1);
        assert_eq!(tail_start(b"\x1b[44mA       \x1b[K\x1b[0m"), 1);
        assert_eq!(tail_start(b"A\x1b[31m       "), 1);
        assert_eq!(tail_start(b"A\x1b[1m       "), 8);
        assert_eq!(
            tail_start(b"A\x1b]8;;https://example.test\x1b\\       \x1b]8;;\x1b\\"),
            8
        );
    }

    #[test]
    fn tmux_empty_tail_retains_the_full_width_of_a_wide_cell() {
        assert_eq!(tail_start("表       ".as_bytes()), 2);
    }
}
