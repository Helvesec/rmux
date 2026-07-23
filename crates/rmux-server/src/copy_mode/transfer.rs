use rmux_proto::RmuxError;

use super::args::{
    ensure_max_positional, ensure_no_extra_args, parse_flagged_args, parse_positionals,
};
use super::text::{normalize_positions, owner_positions};
use super::types::{
    ClearPolicy, CopyBufferTarget, CopyModeCommandOutcome, CopyModePipeCommand, CopyModeTransfer,
    CopyPosition, ModeKeys, SelectionMode,
};
use super::CopyModeState;

impl CopyModeState {
    pub(super) fn transfer_selection(
        &mut self,
        args: &[String],
        append: bool,
        cancel: bool,
        clear_selection: bool,
    ) -> Result<CopyModeCommandOutcome, RmuxError> {
        let data = self.current_selection_bytes();
        let outcome = CopyModeCommandOutcome {
            cancel,
            transfer: Some(CopyModeTransfer {
                data,
                buffer_target: Some(if append {
                    CopyBufferTarget::Top
                } else {
                    CopyBufferTarget::New(None)
                }),
                append,
                pipe_command: None,
            }),
        };
        if clear_selection {
            self.selection = None;
        }
        ensure_no_extra_args("append-selection", args)?;
        Ok(self.finish_policy(outcome, ClearPolicy::Always))
    }

    pub(super) fn transfer_copy_selection(
        &mut self,
        args: &[String],
        cancel: bool,
        clear: ClearPolicy,
    ) -> Result<CopyModeCommandOutcome, RmuxError> {
        let parsed = parse_flagged_args(args, "CP")?;
        ensure_max_positional("copy-selection", &parsed.positionals, 1)?;
        let data = self.current_selection_bytes();
        let buffer_target = if parsed.flags.contains(&'P') {
            None
        } else {
            Some(CopyBufferTarget::New(
                parsed
                    .positionals
                    .first()
                    .cloned()
                    .filter(|value| !value.is_empty()),
            ))
        };
        let outcome = CopyModeCommandOutcome {
            cancel,
            transfer: Some(CopyModeTransfer {
                data,
                buffer_target,
                append: false,
                pipe_command: None,
            }),
        };
        if clear != ClearPolicy::Never {
            self.selection = None;
        }
        Ok(self.finish_policy(outcome, clear))
    }

    pub(super) fn transfer_copy_pipe(
        &mut self,
        args: &[String],
        cancel: bool,
        clear: ClearPolicy,
    ) -> Result<CopyModeCommandOutcome, RmuxError> {
        let parsed = parse_flagged_args(args, "CP")?;
        ensure_max_positional("copy-pipe", &parsed.positionals, 2)?;
        let data = self.current_selection_bytes();
        let buffer_target = if parsed.flags.contains(&'P') {
            None
        } else {
            Some(CopyBufferTarget::New(
                parsed
                    .positionals
                    .get(1)
                    .cloned()
                    .filter(|value| !value.is_empty()),
            ))
        };
        let outcome = CopyModeCommandOutcome {
            cancel,
            transfer: Some(CopyModeTransfer {
                data,
                buffer_target,
                append: false,
                pipe_command: Some(copy_pipe_command(&parsed.positionals)),
            }),
        };
        if clear != ClearPolicy::Never {
            self.selection = None;
        }
        Ok(self.finish_policy(outcome, clear))
    }

    pub(super) fn transfer_pipe(
        &mut self,
        args: &[String],
        cancel: bool,
        clear: ClearPolicy,
    ) -> Result<CopyModeCommandOutcome, RmuxError> {
        let positionals = parse_positionals(args)?;
        ensure_max_positional("pipe", &positionals, 1)?;
        let data = self.current_selection_bytes();
        let outcome = CopyModeCommandOutcome {
            cancel,
            transfer: Some(CopyModeTransfer {
                data,
                buffer_target: None,
                append: false,
                pipe_command: explicit_pipe_command(&positionals),
            }),
        };
        if clear != ClearPolicy::Never {
            self.selection = None;
        }
        Ok(self.finish_policy(outcome, clear))
    }

    pub(super) fn transfer_line(
        &mut self,
        args: &[String],
        pipe: bool,
        cancel: bool,
        count: usize,
    ) -> Result<CopyModeCommandOutcome, RmuxError> {
        let parsed = parse_flagged_args(args, "CP")?;
        ensure_max_positional("copy-line", &parsed.positionals, if pipe { 2 } else { 1 })?;
        let data = self.current_line_transfer_bytes(count);
        let buffer_target = if pipe {
            if parsed.flags.contains(&'P') {
                None
            } else {
                Some(CopyBufferTarget::New(
                    parsed
                        .positionals
                        .get(1)
                        .cloned()
                        .filter(|value| !value.is_empty()),
                ))
            }
        } else {
            Some(CopyBufferTarget::New(
                parsed
                    .positionals
                    .first()
                    .cloned()
                    .filter(|value| !value.is_empty()),
            ))
        };
        let outcome = CopyModeCommandOutcome {
            cancel,
            transfer: Some(CopyModeTransfer {
                data,
                buffer_target,
                append: false,
                pipe_command: if pipe {
                    Some(copy_pipe_command(&parsed.positionals))
                } else {
                    None
                },
            }),
        };
        Ok(self.finish_policy(outcome, ClearPolicy::Always))
    }

    pub(super) fn transfer_end_of_line(
        &mut self,
        args: &[String],
        pipe: bool,
        cancel: bool,
        count: usize,
    ) -> Result<CopyModeCommandOutcome, RmuxError> {
        let parsed = parse_flagged_args(args, "CP")?;
        ensure_max_positional(
            "copy-end-of-line",
            &parsed.positionals,
            if pipe { 2 } else { 1 },
        )?;
        let data = self.current_end_of_line_transfer_bytes(count);
        let buffer_target = if pipe {
            if parsed.flags.contains(&'P') {
                None
            } else {
                Some(CopyBufferTarget::New(
                    parsed
                        .positionals
                        .get(1)
                        .cloned()
                        .filter(|value| !value.is_empty()),
                ))
            }
        } else {
            Some(CopyBufferTarget::New(
                parsed
                    .positionals
                    .first()
                    .cloned()
                    .filter(|value| !value.is_empty()),
            ))
        };
        let outcome = CopyModeCommandOutcome {
            cancel,
            transfer: Some(CopyModeTransfer {
                data,
                buffer_target,
                append: false,
                pipe_command: if pipe {
                    Some(copy_pipe_command(&parsed.positionals))
                } else {
                    None
                },
            }),
        };
        Ok(self.finish_policy(outcome, ClearPolicy::Always))
    }

    fn current_selection_bytes(&self) -> Vec<u8> {
        self.extract_selection()
            .map(|text| text.into_bytes())
            .unwrap_or_default()
    }

    fn current_line_transfer_bytes(&self, count: usize) -> Vec<u8> {
        let start_y = self.logical_line_start_y(self.cursor.y);
        let end_y = self.counted_physical_line_end_y(start_y, count);
        self.extract_line_span(start_y, end_y).into_bytes()
    }

    fn current_end_of_line_transfer_bytes(&self, count: usize) -> Vec<u8> {
        let end_y = self.counted_physical_line_end_y(self.cursor.y, count);
        let mut text = String::new();
        for y in self.cursor.y..=end_y {
            let line = self.line(y);
            let start = if y == self.cursor.y {
                line.owning_cell_x(self.cursor.x).unwrap_or(0)
            } else {
                0
            };
            let end = self.line_end_x(y);
            let trim_spaces = !line.wrapped() || y == end_y;
            text.push_str(&self.extract_line_range(&line, start, end, trim_spaces));
            if y < end_y && !line.wrapped() {
                text.push('\n');
            }
        }
        text.into_bytes()
    }

    fn counted_physical_line_end_y(&self, y: usize, count: usize) -> usize {
        let stepped_y = y
            .saturating_add(count.saturating_sub(1))
            .min(self.total_lines().saturating_sub(1));
        self.logical_line_end_y(stepped_y)
    }

    fn extract_selection(&self) -> Option<String> {
        let selection = self.selection_snapshot()?;
        let (start, end) = normalize_positions(selection.anchor, selection.end);
        if selection.mode == SelectionMode::Line {
            return Some(self.extract_line_selection(start.y, end.y));
        }
        if selection.mode == SelectionMode::Char && !self.rectangle {
            return Some(match self.mode_keys {
                ModeKeys::Vi => self.extract_char_selection_inclusive(start, end),
                ModeKeys::Emacs => self.extract_char_selection_exclusive(start, end),
            });
        }
        let mut lines = Vec::new();
        let rect_min_x = start.x.min(end.x);
        let rect_max_x = start.x.max(end.x);
        for y in start.y..=end.y {
            let line = self.line(y);
            let text = match selection.mode {
                SelectionMode::Line => unreachable!("line selections are handled above"),
                SelectionMode::Char | SelectionMode::Word if self.rectangle => {
                    self.extract_line_range(&line, rect_min_x, rect_max_x, false)
                }
                SelectionMode::Char | SelectionMode::Word => {
                    let range_start = if y == start.y { start.x } else { 0 };
                    let range_end = if y == end.y {
                        end.x
                    } else {
                        self.line_end_x(y)
                    };
                    self.extract_line_range(
                        &line,
                        range_start,
                        range_end,
                        y != start.y || y != end.y,
                    )
                }
            };
            lines.push(text);
        }
        Some(lines.join("\n"))
    }

    fn extract_line_selection(&self, start_y: usize, end_y: usize) -> String {
        self.line_selection_text(self.extract_line_span(start_y, end_y))
    }

    fn extract_line_span(&self, start_y: usize, end_y: usize) -> String {
        let mut lines = Vec::new();
        let mut y = start_y;
        while y <= end_y {
            let span_end = self.logical_line_end_y(y).min(end_y);
            lines.push(self.logical_line_text_range(y, span_end, true));
            y = span_end.saturating_add(1);
        }
        lines.join("\n")
    }

    fn line_selection_text(&self, mut text: String) -> String {
        if self.mode_keys == ModeKeys::Vi {
            text.push('\n');
        }
        text
    }

    fn extract_char_selection_exclusive(&self, start: CopyPosition, end: CopyPosition) -> String {
        if start == end {
            return String::new();
        }
        let mut text = String::new();
        for y in start.y..=end.y {
            let line = self.line(y);
            let range_start = if y == start.y { start.x } else { 0 };
            let Some(range_end) = (if y == end.y {
                self.exclusive_char_line_end(end)
            } else {
                Some(self.line_end_x(y))
            }) else {
                if y != end.y && !line.wrapped() {
                    text.push('\n');
                }
                continue;
            };
            if range_end >= range_start {
                text.push_str(&self.extract_line_range(
                    &line,
                    range_start,
                    range_end,
                    y != start.y || y != end.y,
                ));
            }
            if y != end.y && !line.wrapped() {
                text.push('\n');
            }
        }
        text
    }

    fn extract_char_selection_inclusive(&self, start: CopyPosition, end: CopyPosition) -> String {
        let mut text = String::new();
        for y in start.y..=end.y {
            let line = self.line(y);
            let range_start = if y == start.y { start.x } else { 0 };
            let range_end = if y == end.y {
                end.x
            } else {
                self.line_end_x(y)
            };
            if range_end >= range_start {
                text.push_str(&self.extract_line_range(
                    &line,
                    range_start,
                    range_end,
                    y != start.y || y != end.y,
                ));
            }
            if y != end.y && !line.wrapped() {
                text.push('\n');
            }
        }
        text
    }

    fn exclusive_char_line_end(&self, end: CopyPosition) -> Option<u32> {
        let line = self.line(end.y);
        let owner = line.owning_cell_x(end.x).unwrap_or(end.x);
        owner_positions(&line).into_iter().rfind(|x| *x < owner)
    }
}

fn copy_pipe_command(positionals: &[String]) -> CopyModePipeCommand {
    explicit_pipe_command(positionals).unwrap_or(CopyModePipeCommand::CopyCommandOption)
}

fn explicit_pipe_command(positionals: &[String]) -> Option<CopyModePipeCommand> {
    positionals
        .first()
        .cloned()
        .filter(|value| !value.is_empty())
        .map(CopyModePipeCommand::Explicit)
}
