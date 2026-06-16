use rmux_core::{Screen, ScreenLineView, Utf8Config};
use rmux_proto::{PaneTarget, TerminalSize};

#[path = "copy_mode/args.rs"]
mod args;
#[path = "copy_mode/commands.rs"]
mod commands;
#[path = "copy_mode/motion.rs"]
mod motion;
#[path = "copy_mode/search.rs"]
mod search;
#[path = "copy_mode/selection.rs"]
mod selection;
#[path = "copy_mode/text.rs"]
mod text;
#[path = "copy_mode/transfer.rs"]
mod transfer;
#[path = "copy_mode/types.rs"]
mod types;
#[path = "copy_mode/word.rs"]
mod word;

use text::{classify_word_char, line_char, owner_positions, pattern_looks_like_regex, WordClass};
#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use transfer::run_pipe_command;
#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use types::{
    CopyBufferTarget, CopyModeCommandContext, CopyModeMouseContext, CopyModePipeCommand,
    CopyModeSummary, CopyModeTransfer, CopyPosition, ModeKeys,
};
use types::{JumpState, SearchDirection, SearchMatch, SelectionState};

const BRACKET_SCAN_LIMIT: usize = 1500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyModeState {
    view_mode: bool,
    source_target: Option<PaneTarget>,
    backing: Screen,
    top_line: usize,
    cursor: CopyPosition,
    selection: Option<SelectionState>,
    rectangle: bool,
    mark: Option<CopyPosition>,
    show_mark: bool,
    show_position: bool,
    exit_on_scroll: bool,
    mode_keys: ModeKeys,
    wrap_search: bool,
    word_separators: String,
    search_pattern: String,
    search_direction: SearchDirection,
    search_results: Vec<SearchMatch>,
    search_current: Option<usize>,
    search_timed_out: bool,
    search_count_partial: bool,
    search_highlighted: bool,
    jump: Option<JumpState>,
}

impl CopyModeState {
    pub(crate) fn new(
        backing: Screen,
        source_target: Option<PaneTarget>,
        view_mode: bool,
        context: &CopyModeCommandContext,
        exit_on_scroll: bool,
        show_position: bool,
    ) -> Self {
        let cursor = CopyPosition {
            x: backing.cursor_position().0,
            y: backing.cursor_absolute_y(),
        };
        let mut state = Self {
            view_mode,
            source_target,
            top_line: 0,
            cursor,
            selection: None,
            rectangle: false,
            mark: None,
            show_mark: false,
            show_position,
            exit_on_scroll,
            mode_keys: context.mode_keys,
            wrap_search: context.wrap_search,
            word_separators: context.word_separators.clone(),
            search_pattern: String::new(),
            search_direction: SearchDirection::Forward,
            search_results: Vec::new(),
            search_current: None,
            search_timed_out: false,
            search_count_partial: false,
            search_highlighted: false,
            jump: None,
            backing,
        };
        state.top_line = state.bottom_top_line();
        state.ensure_cursor_visible();
        state
    }

    #[cfg(test)]
    pub(crate) fn for_test(backing: Screen) -> Self {
        Self::new(
            backing,
            None,
            false,
            &CopyModeCommandContext {
                mode_keys: ModeKeys::Emacs,
                wrap_search: true,
                word_separators: " -_@".to_owned(),
                default_shell: "/bin/sh".to_owned(),
                working_directory: None,
                refresh_screen: None,
                mouse: None,
            },
            false,
            true,
        )
    }

    pub(crate) fn view_mode(&self) -> bool {
        self.view_mode
    }

    pub(crate) fn source_target(&self) -> Option<&PaneTarget> {
        self.source_target.as_ref()
    }

    pub(crate) fn set_source_target(&mut self, source_target: Option<PaneTarget>) {
        self.source_target = source_target;
    }

    pub(crate) fn set_show_position(&mut self, show_position: bool) {
        self.show_position = show_position;
    }

    pub(crate) fn set_exit_on_scroll(&mut self, exit_on_scroll: bool) {
        self.exit_on_scroll = exit_on_scroll;
    }

    pub(crate) fn set_utf8_config(&mut self, utf8_config: Utf8Config) {
        self.backing.set_utf8_config(utf8_config);
    }

    pub(crate) fn resize(&mut self, size: TerminalSize) {
        self.backing.resize(size);
        self.selection = None;
        self.search_timed_out = false;
        self.search_count_partial = false;
        self.search_highlighted = false;
        self.clamp_cursor();
        self.top_line = self.top_line.min(self.bottom_top_line());
        self.ensure_cursor_visible();
        if !self.search_pattern.is_empty() {
            let plain = !pattern_looks_like_regex(&self.search_pattern);
            self.rebuild_search_results(plain);
        }
    }

    pub(crate) fn refresh_from_screen(&mut self, backing: Screen) {
        self.backing = backing;
        self.cursor = CopyPosition {
            x: self.backing.cursor_position().0,
            y: self.backing.cursor_absolute_y(),
        };
        self.top_line = self.bottom_top_line();
        self.selection = None;
        self.search_timed_out = false;
        self.search_count_partial = false;
        self.search_highlighted = false;
        if !self.search_pattern.is_empty() {
            let plain = !pattern_looks_like_regex(&self.search_pattern);
            self.rebuild_search_results(plain);
        }
    }

    pub(crate) fn render_screen(&self) -> Screen {
        let mut viewport = self
            .backing
            .clone_viewport(self.top_line, self.cursor.x, self.cursor.y);
        if let Some(selection) = self.selection_snapshot() {
            self.mark_selection_in_viewport(&mut viewport, selection);
        }
        viewport
    }

    pub(crate) fn summary(&self) -> CopyModeSummary {
        let (selection_start, selection_end, selection_active, selection_present, selection_mode) =
            if let Some(selection) = self.selection_snapshot() {
                (
                    Some(selection.anchor),
                    Some(selection.end),
                    selection.active,
                    true,
                    Some(selection.mode),
                )
            } else {
                (None, None, false, false, None)
            };
        let search_match = self
            .search_current
            .and_then(|index| self.search_results.get(index))
            .map(|result| result.text.clone());
        CopyModeSummary {
            view_mode: self.view_mode,
            scroll_position: self.bottom_top_line().saturating_sub(self.top_line),
            rectangle_toggle: self.rectangle,
            cursor_x: self.cursor.x,
            cursor_y: self.cursor.y.saturating_sub(self.top_line),
            selection_start,
            selection_end,
            selection_active,
            selection_present,
            selection_mode,
            search_present: !self.search_pattern.is_empty(),
            search_timed_out: self.search_timed_out,
            search_count: self.search_results.len(),
            search_count_partial: self.search_count_partial,
            search_match,
            copy_cursor_word: self.current_word().unwrap_or_default(),
            copy_cursor_line: self.current_line_text(),
            copy_cursor_hyperlink: self.current_hyperlink().unwrap_or_default(),
            pane_search_string: self.search_pattern.clone(),
            top_line_time: if self.top_line < self.backing.history_size() {
                self.backing
                    .absolute_line_view(self.top_line)
                    .map(|line| line.time())
                    .unwrap_or_default()
            } else {
                0
            },
        }
    }

    fn current_word(&self) -> Option<String> {
        let range = self.word_selection_range(self.cursor);
        let line = self.line(range.start.y);
        let class = line_char(&line, range.start.x)
            .map(|ch| classify_word_char(ch, &self.word_separators, false))
            .unwrap_or(WordClass::Space);
        if class != WordClass::Word {
            return None;
        }
        Some(self.extract_line_range(&line, range.start.x, range.end.x, false))
    }

    fn current_line_text(&self) -> String {
        self.full_line_text(self.cursor.y, true)
    }

    fn current_hyperlink(&self) -> Option<String> {
        let line = self.line(self.cursor.y);
        let x = line.owning_cell_x(self.cursor.x).unwrap_or(self.cursor.x);
        let cell = line.cell(x)?;
        let link = cell.link();
        if link == 0 {
            return None;
        }
        self.backing.hyperlink_uri(link).map(str::to_owned)
    }

    fn find_matching_bracket(&self, forward: bool) -> Option<CopyPosition> {
        let current_line = self.line(self.cursor.y);
        let current_char = line_char(&current_line, self.cursor.x)?;
        let (open, close, scan_forward) = match current_char {
            '(' => ('(', ')', true),
            '[' => ('[', ']', true),
            '{' => ('{', '}', true),
            ')' => ('(', ')', false),
            ']' => ('[', ']', false),
            '}' => ('{', '}', false),
            _ => return None,
        };
        let scan_forward = if forward { scan_forward } else { !scan_forward };
        let positions = self.flatten_owner_positions();
        let index = positions
            .iter()
            .position(|position| *position == self.cursor)?;
        let iter: Box<dyn Iterator<Item = CopyPosition>> = if scan_forward {
            Box::new(
                positions
                    .into_iter()
                    .skip(index.saturating_add(1))
                    .take(BRACKET_SCAN_LIMIT),
            )
        } else {
            Box::new(
                positions
                    .into_iter()
                    .take(index)
                    .rev()
                    .take(BRACKET_SCAN_LIMIT),
            )
        };
        let mut depth = 1usize;
        for position in iter {
            let line = self.line(position.y);
            let ch = line_char(&line, position.x)?;
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(position);
                }
            }
        }
        None
    }

    fn line_blank(&self, y: usize) -> bool {
        self.full_line_text(y, true).trim().is_empty()
    }

    fn full_line_text(&self, y: usize, trim_spaces: bool) -> String {
        let line = self.line(y);
        self.extract_line_range(&line, 0, self.cols().saturating_sub(1), trim_spaces)
    }

    fn logical_line_text(&self, y: usize, trim_spaces: bool) -> String {
        let start = self.logical_line_start_y(y);
        let end = self.logical_line_end_y(y);
        self.logical_line_text_range(start, end, trim_spaces)
    }

    fn logical_line_text_range(&self, start_y: usize, end_y: usize, trim_spaces: bool) -> String {
        let mut text = String::new();
        for y in start_y..=end_y {
            text.push_str(&self.full_line_text(y, trim_spaces && y == end_y));
        }
        text
    }

    fn logical_line_start_y(&self, mut y: usize) -> usize {
        while y > 0 && self.line(y - 1).wrapped() {
            y -= 1;
        }
        y
    }

    fn logical_line_end_y(&self, mut y: usize) -> usize {
        let total_lines = self.total_lines();
        while y + 1 < total_lines && self.line(y).wrapped() {
            y += 1;
        }
        y
    }

    fn extract_line_range(
        &self,
        line: &ScreenLineView,
        start: u32,
        end: u32,
        trim_spaces: bool,
    ) -> String {
        let start = line.owning_cell_x(start).unwrap_or(start);
        let end = line.owning_cell_x(end).unwrap_or(end);
        let mut output = String::new();
        let mut x = start;
        let last = end.min(self.cols().saturating_sub(1));
        while x <= last {
            match line.cell(x) {
                Some(cell) if !cell.is_padding() => {
                    output.push_str(cell.text());
                    x = x.saturating_add(u32::from(cell.width().max(1)));
                }
                Some(_) => {
                    x = x.saturating_add(1);
                }
                None if x < line.width() => {
                    output.push(' ');
                    x = x.saturating_add(1);
                }
                None => break,
            }
        }
        if trim_spaces && !line.wrapped() {
            output.trim_end_matches(' ').to_owned()
        } else {
            output
        }
    }

    fn previous_cell_position(&self, position: CopyPosition) -> Option<CopyPosition> {
        let line = self.line(position.y);
        let owner = line.owning_cell_x(position.x).unwrap_or(position.x);
        if let Some(previous) = self.previous_owner_in_line(&line, owner) {
            return Some(CopyPosition {
                x: previous,
                y: position.y,
            });
        }
        if position.y == 0 {
            return None;
        }
        let previous_y = position.y - 1;
        Some(CopyPosition {
            x: self.line_end_x(previous_y),
            y: previous_y,
        })
    }

    fn next_cell_position(&self, position: CopyPosition) -> Option<CopyPosition> {
        let line = self.line(position.y);
        let owner = line.owning_cell_x(position.x).unwrap_or(position.x);
        let end = self.line_end_x(position.y);
        if owner < end {
            if let Some(next) = self
                .next_owner_in_line(&line, owner)
                .filter(|next| *next <= end)
            {
                return Some(CopyPosition {
                    x: next,
                    y: position.y,
                });
            }
        }
        if position.y + 1 >= self.total_lines() {
            return None;
        }
        Some(CopyPosition {
            x: 0,
            y: position.y + 1,
        })
    }

    fn previous_owner_in_line(&self, line: &ScreenLineView, x: u32) -> Option<u32> {
        owner_positions(line)
            .into_iter()
            .take_while(|candidate| *candidate < x)
            .last()
    }

    fn next_owner_in_line(&self, line: &ScreenLineView, x: u32) -> Option<u32> {
        owner_positions(line)
            .into_iter()
            .find(|candidate| *candidate > x)
    }

    fn owning_or_zero(&self, y: usize, x: u32) -> u32 {
        self.line(y).owning_cell_x(x).unwrap_or(0)
    }

    fn line_end_x(&self, y: usize) -> u32 {
        let line = self.line(y);
        let positions = owner_positions(&line);
        let Some(last_content) = positions.iter().copied().rev().find(|x| {
            line.cell(*x)
                .is_some_and(|cell| !cell.text().chars().all(char::is_whitespace))
        }) else {
            return 0;
        };
        positions
            .into_iter()
            .find(|x| *x > last_content)
            .unwrap_or(last_content)
    }

    fn line(&self, y: usize) -> ScreenLineView {
        self.backing
            .absolute_line_view(y)
            .expect("copy-mode line must exist")
    }

    fn clamp_cursor(&mut self) {
        if self.total_lines() == 0 {
            self.cursor = CopyPosition { x: 0, y: 0 };
            return;
        }
        self.cursor.y = self.cursor.y.min(self.total_lines().saturating_sub(1));
        self.cursor.x = self.owning_or_zero(
            self.cursor.y,
            self.cursor.x.min(self.cols().saturating_sub(1)),
        );
    }

    fn ensure_cursor_visible(&mut self) {
        let rows = usize::from(self.rows().max(1));
        let bottom = self.top_line + rows;
        if self.cursor.y < self.top_line {
            self.top_line = self.cursor.y;
        } else if self.cursor.y >= bottom {
            self.top_line = self.cursor.y.saturating_sub(rows.saturating_sub(1));
        }
        self.top_line = self.top_line.min(self.bottom_top_line());
    }

    fn rows(&self) -> u16 {
        self.backing.size().rows.max(1)
    }

    fn cols(&self) -> u32 {
        u32::from(self.backing.size().cols.max(1))
    }

    fn total_lines(&self) -> usize {
        self.backing.absolute_line_count().max(1)
    }

    fn bottom_top_line(&self) -> usize {
        self.total_lines()
            .saturating_sub(usize::from(self.rows().max(1)))
    }

    fn at_bottom(&self) -> bool {
        self.top_line >= self.bottom_top_line()
    }
}

#[cfg(test)]
#[path = "copy_mode/tests.rs"]
mod tests;
