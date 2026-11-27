//! Remap row-granularity shell marks with the content of a reflowed group.

use super::{GridCell, GridLine, GridLineFlags};

#[derive(Default)]
pub(super) struct ReflowShellMarks {
    offset: usize,
    marks: Vec<(usize, GridLineFlags)>,
}

impl ReflowShellMarks {
    pub(super) fn observe(&mut self, line: &GridLine) {
        if line.flags.contains(GridLineFlags::START_PROMPT)
            || line.flags.contains(GridLineFlags::START_OUTPUT)
        {
            self.marks.push((self.offset, line.flags));
        }
        self.offset = self.offset.saturating_add(line.reflow_logical_width());
    }

    pub(super) fn account_for_clamped_cells(&mut self, cells: &[GridCell], width: u32) {
        let mut cells = cells.iter().peekable();
        let mut source_offset = 0_usize;
        let mut removed_columns = 0_usize;
        for (offset, _) in &mut self.marks {
            while let Some(cell) = cells.peek() {
                let source_width = usize::from(cell.width().max(1));
                if source_offset.saturating_add(source_width) > *offset {
                    break;
                }
                source_offset = source_offset.saturating_add(source_width);
                // Reflow renders a glyph wider than the viewport in one cell.
                // Convert source-column anchors to that same output space.
                if source_width > width.max(1) as usize {
                    removed_columns = removed_columns.saturating_add(source_width - 1);
                }
                let _ = cells.next();
            }
            *offset = offset.saturating_sub(removed_columns);
        }
    }

    pub(super) fn apply(&mut self, lines: &mut [GridLine]) {
        let mut row = 0;
        let mut end = lines.first().map_or(0, GridLine::reflow_logical_width);
        for (offset, flags) in self.marks.drain(..) {
            while offset >= end && row + 1 < lines.len() {
                row += 1;
                end = end.saturating_add(lines[row].reflow_logical_width());
            }
            if let Some(line) = lines.get_mut(row) {
                if flags.contains(GridLineFlags::START_PROMPT) {
                    line.mark_shell_line(false);
                }
                if flags.contains(GridLineFlags::START_OUTPUT) {
                    line.mark_shell_line(true);
                }
            }
        }
        self.offset = 0;
    }
}
