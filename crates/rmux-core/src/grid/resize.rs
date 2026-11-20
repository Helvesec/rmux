//! Grid geometry changes and cursor mapping.

use super::history::compacted_history;
use super::reflow::{reflow_alternate_visible_lines, reflow_wrapped_lines_remapping_cursor};
use super::{
    Colour, Grid, GridLine, GridLineFlags, GridLogicalCursor, GridPhysicalCursor, TerminalSize,
    COLOUR_DEFAULT,
};

impl Grid {
    pub(crate) fn resize_visible_width_preserving_cursor(
        &mut self,
        sx: u32,
        bg: Colour,
        visible_y: u32,
        cursor_x: u32,
        pending_wrap: bool,
    ) -> GridPhysicalCursor {
        let source_width = self.sx.max(1);
        let target_width = sx.max(1);
        let visible = reflow_alternate_visible_lines(
            self.visible_lines(),
            target_width,
            bg,
            self.sy as usize,
        );
        let current_history = self.history.len();
        self.sx = target_width;
        self.visible = visible.into();

        // tmux keeps the alternate-screen cursor at its physical coordinate
        // across a width resize. Its grid may retain an x beyond the new edge;
        // RMUX models the same next-write behavior with a bounded edge cursor
        // and pending wrap.
        let physical_x = if pending_wrap { source_width } else { cursor_x };
        let (x, pending_wrap) = if physical_x >= target_width {
            (target_width.saturating_sub(1), true)
        } else {
            (physical_x, false)
        };
        GridPhysicalCursor {
            absolute_y: current_history.saturating_add(
                usize::try_from(visible_y.min(self.sy.saturating_sub(1))).unwrap_or(usize::MAX),
            ),
            x,
            pending_wrap,
        }
    }

    pub(crate) fn restore_visible_at_size(
        &mut self,
        source_size: TerminalSize,
        lines: Vec<GridLine>,
        bg: Colour,
    ) {
        self.sx = u32::from(source_size.cols.max(1));
        self.sy = u32::from(source_size.rows.max(1));
        self.visible = lines.into();
        while self.visible.len() > self.sy as usize {
            let _ = self.visible.pop_back();
        }
        while self.visible.len() < self.sy as usize {
            self.visible.push_back(GridLine::blank_with_bg(self.sx, bg));
        }
        for line in &mut self.visible {
            line.resize_width_preserving_wrap(self.sx, bg);
        }
    }

    pub(crate) fn logical_cursor(
        &self,
        visible_y: u32,
        cursor_x: u32,
        pending_wrap: bool,
    ) -> GridLogicalCursor {
        let total_lines = self.total_line_count();
        if total_lines == 0 {
            return GridLogicalCursor {
                logical_start_y: 0,
                offset: 0,
            };
        }

        let absolute_y = self
            .history
            .len()
            .saturating_add(visible_y as usize)
            .min(total_lines.saturating_sub(1));
        let logical_start_y = self.logical_start_y(absolute_y);
        let mut offset = 0_usize;
        for line_y in logical_start_y..absolute_y {
            if let Some(line) = self.absolute_line(line_y) {
                offset = offset.saturating_add(line.reflow_logical_width());
            }
        }
        let physical_cursor_column = if pending_wrap {
            self.sx as usize
        } else {
            cursor_x.min(self.sx.saturating_sub(1)) as usize
        };
        let cursor_column = self
            .absolute_line(absolute_y)
            .map_or(physical_cursor_column, |line| {
                line.reflow_logical_column(physical_cursor_column)
            });
        offset = offset.saturating_add(cursor_column);

        GridLogicalCursor {
            logical_start_y,
            offset,
        }
    }

    pub(crate) fn resize_width_remapping_cursor(
        &mut self,
        sx: u32,
        bg: Colour,
        cursor: GridLogicalCursor,
    ) -> GridPhysicalCursor {
        let sx = sx.max(1);
        if sx == self.sx {
            return self.locate_cursor_from_logical(cursor);
        }

        if self.can_resize_width_without_reflow(sx) {
            for line in &mut self.history {
                line.resize_width_preserving_wrap(sx, bg);
            }
            for line in &mut self.visible {
                line.resize_width_preserving_wrap(sx, bg);
            }
            self.sx = sx;
            self.recompute_history_content_bytes();
            return self.locate_cursor_from_logical(cursor);
        }

        let visible_rows = self.sy as usize;
        let lines = self
            .history
            .iter()
            .chain(self.visible.iter())
            .cloned()
            .collect::<Vec<_>>();
        let (mut reflowed, reflow_cursor) =
            reflow_wrapped_lines_remapping_cursor(lines, sx, bg, cursor);
        while reflowed.len() < visible_rows {
            reflowed.push(GridLine::blank_with_bg(sx, bg));
        }

        let default_history_rows = reflowed.len().saturating_sub(visible_rows);
        let mut mapped_cursor = reflow_cursor.unwrap_or(GridPhysicalCursor {
            absolute_y: reflowed.len().saturating_sub(1),
            x: sx.saturating_sub(1),
            pending_wrap: false,
        });
        let trailing_empty_rows = reflowed
            .iter()
            .rev()
            .take_while(|line| {
                line.used_end() == 0
                    && !line.flags.contains(GridLineFlags::WRAPPED)
                    && !line.flags.contains(GridLineFlags::START_PROMPT)
                    && !line.flags.contains(GridLineFlags::START_OUTPUT)
            })
            .count();
        let cursor_shift = default_history_rows.saturating_sub(mapped_cursor.absolute_y);
        let history_rows =
            default_history_rows.saturating_sub(cursor_shift.min(trailing_empty_rows));
        if mapped_cursor.absolute_y < history_rows {
            mapped_cursor.absolute_y = history_rows;
            mapped_cursor.x = 0;
            mapped_cursor.pending_wrap = false;
        }
        let mut remaining = reflowed.split_off(history_rows);
        remaining.truncate(visible_rows);
        while remaining.len() < visible_rows {
            remaining.push(GridLine::blank_with_bg(sx, bg));
        }
        let mut visible = remaining;
        for line in &mut visible {
            line.resize_width_preserving_wrap(sx, bg);
        }
        self.history = compacted_history(reflowed);
        self.recompute_history_content_bytes();
        self.reflow_history_capacity = if self.history.len() > self.hlimit {
            self.history.len()
        } else {
            0
        };
        self.visible = visible.into();
        self.hscrolled = if self.hlimit == 0 {
            0
        } else {
            self.history.len()
        };
        self.sx = sx;
        mapped_cursor
    }

    fn can_resize_width_without_reflow(&self, sx: u32) -> bool {
        self.history.iter().chain(self.visible.iter()).all(|line| {
            !line.flags.contains(GridLineFlags::WRAPPED) && line.used_end() <= sx as usize
        })
    }

    pub(crate) fn resize_height(&mut self, sy: u32, cursor_y: &mut u32, bg: Colour) {
        let sy = sy.max(1);
        let oldy = self.sy;

        if sy < oldy {
            let mut needed = oldy - sy;

            let available_bottom = oldy.saturating_sub(1).saturating_sub(*cursor_y);
            let remove_bottom = available_bottom.min(needed);
            for _ in 0..remove_bottom {
                let _ = self.visible.pop_back();
            }
            needed -= remove_bottom;

            if self.history_enabled {
                for _ in 0..needed {
                    let Some(line) = self.visible.pop_front() else {
                        break;
                    };
                    self.push_history_preserving_reflow_capacity(line);
                }
            } else {
                let remove_top = (*cursor_y).min(needed);
                for _ in 0..remove_top {
                    let _ = self.visible.pop_front();
                }
                *cursor_y = cursor_y.saturating_sub(remove_top);
            }
        } else if sy > oldy {
            let mut needed = sy - oldy;
            let pull = self.hscrolled.min(needed as usize).min(self.history.len()) as u32;
            if self.history_enabled && pull > 0 {
                let mut restored = Vec::with_capacity(pull as usize);
                for _ in 0..pull {
                    if let Some(line) = self.pop_history_back() {
                        restored.push(line);
                    }
                }
                restored.reverse();
                for mut line in restored.into_iter().rev() {
                    line.resize_width_preserving_wrap(self.sx, bg);
                    self.visible.push_front(line);
                }
                *cursor_y = cursor_y.saturating_add(pull).min(sy.saturating_sub(1));
                self.hscrolled -= pull as usize;
                needed -= pull;
            }

            for _ in 0..needed {
                self.visible.push_back(GridLine::blank_with_bg(self.sx, bg));
            }
        }

        self.sy = sy;
        while self.visible.len() > self.sy as usize {
            let _ = self.visible.pop_back();
        }
        while self.visible.len() < self.sy as usize {
            self.visible.push_back(GridLine::blank_with_bg(self.sx, bg));
        }
        for line in &mut self.visible {
            line.resize_width_preserving_wrap(self.sx, bg);
        }
        *cursor_y = (*cursor_y).min(self.sy.saturating_sub(1));
    }

    fn total_line_count(&self) -> usize {
        self.history.len().saturating_add(self.visible.len())
    }

    pub(super) fn logical_start_y(&self, absolute_y: usize) -> usize {
        let mut start = absolute_y.min(self.total_line_count().saturating_sub(1));
        while start > 0
            && self
                .absolute_line(start - 1)
                .is_some_and(|line| line.flags.contains(GridLineFlags::WRAPPED))
        {
            start -= 1;
        }
        start
    }

    fn locate_cursor_from_logical(&self, cursor: GridLogicalCursor) -> GridPhysicalCursor {
        let total_lines = self.total_line_count();
        if total_lines == 0 {
            return GridPhysicalCursor {
                absolute_y: 0,
                x: 0,
                pending_wrap: false,
            };
        }

        let start = cursor.logical_start_y.min(total_lines.saturating_sub(1));
        let mut lines = Vec::new();
        for absolute_y in start..total_lines {
            let Some(line) = self.absolute_line(absolute_y) else {
                break;
            };
            lines.push(line.clone());
            if !line.flags.contains(GridLineFlags::WRAPPED) {
                break;
            }
        }

        if lines.len() == 1 {
            let used_end = lines[0].used_end();
            let at_new_edge =
                used_end > 0 && cursor.offset == used_end && used_end == self.sx as usize;
            return GridPhysicalCursor {
                absolute_y: start,
                x: if at_new_edge {
                    self.sx.saturating_sub(1)
                } else {
                    u32::try_from(cursor.offset)
                        .unwrap_or(u32::MAX)
                        .min(self.sx.saturating_sub(1))
                },
                pending_wrap: at_new_edge,
            };
        }

        let (_, relative) = reflow_wrapped_lines_remapping_cursor(
            lines,
            self.sx,
            COLOUR_DEFAULT,
            GridLogicalCursor {
                logical_start_y: 0,
                offset: cursor.offset,
            },
        );
        let relative = relative.unwrap_or(GridPhysicalCursor {
            absolute_y: 0,
            x: 0,
            pending_wrap: false,
        });
        GridPhysicalCursor {
            absolute_y: start.saturating_add(relative.absolute_y),
            x: relative.x,
            pending_wrap: relative.pending_wrap,
        }
    }
}
