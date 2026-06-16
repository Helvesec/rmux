use rmux_core::Screen;
use rmux_proto::RmuxError;

use super::args::parse_single_argument;
use super::text::{
    classify_word_char, line_char, normalize_positions, owner_positions, CopyRange, WordClass,
};
use super::types::{
    ClearPolicy, CopyModeCommandOutcome, CopyPosition, SelectionMode, SelectionState,
};
use super::CopyModeState;

impl CopyModeState {
    pub(super) fn other_end(&mut self) -> Result<CopyModeCommandOutcome, RmuxError> {
        if self.view_mode {
            return Ok(
                self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::EmacsOnly)
            );
        }
        let Some(selection) = self.selection_snapshot() else {
            return Ok(
                self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::EmacsOnly)
            );
        };
        let current = self.cursor;
        self.cursor = selection.anchor;
        if let Some(existing) = &mut self.selection {
            existing.anchor = current;
            existing.end = self.cursor;
            existing.active = true;
        }
        self.ensure_cursor_visible();
        Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::EmacsOnly))
    }

    pub(super) fn selection_mode(
        &mut self,
        args: &[String],
    ) -> Result<CopyModeCommandOutcome, RmuxError> {
        let value = parse_single_argument("selection-mode", args)?;
        let mode = SelectionMode::parse(&value)
            .ok_or_else(|| RmuxError::Server(format!("invalid selection mode: {value}")))?;
        if let Some(selection) = &mut self.selection {
            selection.mode = mode;
            selection.end = self.cursor;
            selection.active = true;
        } else {
            self.selection = Some(SelectionState {
                anchor: self.cursor,
                end: self.cursor,
                mode,
                active: true,
            });
        }
        Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Never))
    }

    pub(super) fn begin_selection(&mut self) {
        self.selection = Some(SelectionState {
            anchor: self.cursor,
            end: self.cursor,
            mode: SelectionMode::Char,
            active: true,
        });
    }

    pub(super) fn mark_selection_in_viewport(
        &self,
        viewport: &mut Screen,
        selection: SelectionState,
    ) {
        let rows = usize::from(self.rows().max(1));
        let viewport_top = self.top_line;
        let viewport_bottom = viewport_top.saturating_add(rows);
        let (start, end) = normalize_positions(selection.anchor, selection.end);
        let rect_min_x = start.x.min(end.x);
        let rect_max_x = start.x.max(end.x);

        for y in start.y..=end.y {
            if y < viewport_top || y >= viewport_bottom {
                continue;
            }

            let (range_start, range_end) = match selection.mode {
                SelectionMode::Line => (0, self.line_end_x(y)),
                SelectionMode::Char | SelectionMode::Word if self.rectangle => {
                    (rect_min_x, rect_max_x)
                }
                SelectionMode::Char | SelectionMode::Word => {
                    let range_start = if y == start.y { start.x } else { 0 };
                    let range_end = if y == end.y {
                        end.x
                    } else {
                        self.line_end_x(y)
                    };
                    (range_start, range_end)
                }
            };

            viewport.mark_selected_row_range((y - viewport_top) as u32, range_start, range_end);
        }
    }

    pub(super) fn select_line(&mut self) {
        let start_y = self.logical_line_start_y(self.cursor.y);
        let end_y = self.logical_line_end_y(self.cursor.y);
        let x = self.line_end_x(end_y);
        self.cursor = CopyPosition { x, y: end_y };
        self.selection = Some(SelectionState {
            anchor: CopyPosition { x: 0, y: start_y },
            end: CopyPosition { x, y: end_y },
            mode: SelectionMode::Line,
            active: true,
        });
        self.ensure_cursor_visible();
    }

    pub(super) fn select_word(&mut self) {
        let range = self.word_selection_range(self.cursor);
        self.selection = Some(SelectionState {
            anchor: range.start,
            end: range.end,
            mode: SelectionMode::Word,
            active: true,
        });
        self.cursor = range.end;
        self.ensure_cursor_visible();
    }

    pub(super) fn selection_snapshot(&self) -> Option<SelectionState> {
        let mut selection = self.selection.clone()?;
        if selection.active {
            selection.end = self.cursor;
        }
        Some(selection)
    }

    pub(super) fn sync_selection_with_cursor(&mut self) {
        if let Some(selection) = &mut self.selection {
            if selection.active {
                selection.end = self.cursor;
            }
        }
    }

    pub(super) fn word_selection_range(&self, position: CopyPosition) -> CopyRange {
        let line = self.line(position.y);
        let positions = owner_positions(&line);
        if positions.is_empty() {
            return CopyRange {
                start: position,
                end: position,
            };
        }
        let mut owner = line.owning_cell_x(position.x).unwrap_or(position.x);
        let mut class = line_char(&line, owner)
            .map(|ch| classify_word_char(ch, &self.word_separators, false))
            .unwrap_or(WordClass::Space);
        if class != WordClass::Word {
            if let Some(next_word) = positions.iter().copied().filter(|x| *x > owner).find(|x| {
                line_char(&line, *x).map(|ch| classify_word_char(ch, &self.word_separators, false))
                    == Some(WordClass::Word)
            }) {
                owner = next_word;
                class = WordClass::Word;
            }
        }
        let mut start = owner;
        let mut end = owner;
        for candidate in positions.iter().copied().rev().filter(|x| *x < owner) {
            let candidate_class = line_char(&line, candidate)
                .map(|ch| classify_word_char(ch, &self.word_separators, false))
                .unwrap_or(WordClass::Space);
            if candidate_class != class {
                break;
            }
            start = candidate;
        }
        for candidate in positions.iter().copied().filter(|x| *x > owner) {
            let candidate_class = line_char(&line, candidate)
                .map(|ch| classify_word_char(ch, &self.word_separators, false))
                .unwrap_or(WordClass::Space);
            if candidate_class != class {
                break;
            }
            end = candidate;
        }
        CopyRange {
            start: CopyPosition {
                x: start,
                y: position.y,
            },
            end: CopyPosition {
                x: end,
                y: position.y,
            },
        }
    }
}
