use regex::RegexBuilder;
use rmux_proto::RmuxError;
use std::borrow::Cow;

use super::args::parse_single_argument;
use super::text::{
    line_char, owner_positions, pattern_looks_like_regex, position_ge, position_le, LineTextMap,
};
use super::types::{
    ClearPolicy, CopyModeCommandOutcome, JumpKind, JumpState, ModeKeys, SearchDirection,
};
use super::CopyModeState;

const SEARCH_REGEX_SIZE_LIMIT: usize = 1_000_000;
const SEARCH_REGEX_DFA_SIZE_LIMIT: usize = 1_000_000;
const SEARCH_OFF_LOCK_LINE_THRESHOLD: usize = 10_000;
const SEARCH_OFF_LOCK_ARG_BYTES_THRESHOLD: usize = 4096;

impl CopyModeState {
    pub(crate) fn command_runs_search(command: &str) -> bool {
        matches!(
            command,
            "search-again"
                | "search-backward"
                | "search-backward-text"
                | "search-backward-incremental"
                | "search-forward"
                | "search-forward-text"
                | "search-forward-incremental"
                | "search-reverse"
        )
    }

    pub(crate) fn mark_search_timed_out(&mut self) {
        self.search_timed_out = true;
        self.search_count_partial = true;
    }

    pub(crate) fn search_should_run_off_lock(&self, args: &[String]) -> bool {
        let arg_bytes = args.iter().map(String::len).sum::<usize>();
        self.total_lines() > SEARCH_OFF_LOCK_LINE_THRESHOLD
            || arg_bytes > SEARCH_OFF_LOCK_ARG_BYTES_THRESHOLD
    }

    pub(super) fn search_with_arg(
        &mut self,
        args: &[String],
        direction: SearchDirection,
        plain_text: bool,
    ) -> Result<CopyModeCommandOutcome, RmuxError> {
        let pattern = parse_single_argument("search", args)?;
        self.perform_search(pattern, direction, plain_text)?;
        let outcome = self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always);
        self.search_highlighted = !self.search_results.is_empty();
        Ok(outcome)
    }

    pub(super) fn search_again(&mut self) -> Result<CopyModeCommandOutcome, RmuxError> {
        if self.search_pattern.is_empty() || self.search_results.is_empty() {
            return Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always));
        }
        self.advance_search_match(self.search_direction);
        Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always))
    }

    pub(super) fn search_reverse(&mut self) -> Result<CopyModeCommandOutcome, RmuxError> {
        if self.search_pattern.is_empty() || self.search_results.is_empty() {
            return Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always));
        }
        let reverse = match self.search_direction {
            SearchDirection::Forward => SearchDirection::Backward,
            SearchDirection::Backward => SearchDirection::Forward,
        };
        self.advance_search_match(reverse);
        Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always))
    }

    pub(super) fn incremental_search(
        &mut self,
        args: &[String],
        default_direction: SearchDirection,
    ) -> Result<CopyModeCommandOutcome, RmuxError> {
        let value = parse_single_argument("incremental-search", args)?;
        let (prefix, pattern) = match value.split_once(':') {
            Some((prefix, pattern)) => (prefix, pattern),
            None => ("=", value.as_str()),
        };
        if prefix.is_empty() {
            return Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always));
        }
        let direction = match prefix {
            "+" => SearchDirection::Forward,
            "-" => SearchDirection::Backward,
            "=" => self.search_direction,
            _ => default_direction,
        };
        if pattern.is_empty() {
            self.search_pattern.clear();
            self.search_results.clear();
            self.search_current = None;
            self.search_highlighted = false;
            return Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always));
        }
        let plain = !pattern_looks_like_regex(pattern);
        self.perform_search(pattern.to_owned(), direction, plain)?;
        let outcome = self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always);
        self.search_highlighted = !self.search_results.is_empty();
        Ok(outcome)
    }

    pub(super) fn jump_with_arg(
        &mut self,
        args: &[String],
        kind: JumpKind,
    ) -> Result<CopyModeCommandOutcome, RmuxError> {
        let arg = parse_single_argument("jump", args)?;
        let ch = arg.chars().next().ok_or_else(|| {
            RmuxError::Server("jump command expects a non-empty character".to_owned())
        })?;
        self.jump = Some(JumpState { kind, ch });
        self.execute_jump(kind, ch)?;
        Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::EmacsOnly))
    }

    pub(super) fn jump_again(
        &mut self,
        reverse: bool,
    ) -> Result<CopyModeCommandOutcome, RmuxError> {
        let Some(jump) = self.jump.clone() else {
            return Ok(
                self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::EmacsOnly)
            );
        };
        let kind = if reverse {
            jump.kind.reverse()
        } else {
            jump.kind
        };
        self.execute_jump(kind, jump.ch)?;
        Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::EmacsOnly))
    }

    fn perform_search(
        &mut self,
        pattern: String,
        direction: SearchDirection,
        plain_text: bool,
    ) -> Result<(), RmuxError> {
        self.search_pattern = pattern;
        self.search_direction = direction;
        self.search_timed_out = false;
        self.search_count_partial = false;
        self.rebuild_search_results(plain_text);
        self.search_highlighted = true;
        self.search_current = self.find_search_match(direction);
        if let Some(index) = self.search_current {
            if let Some(result) = self.search_results.get(index) {
                self.cursor = match self.mode_keys {
                    ModeKeys::Vi => result.start,
                    ModeKeys::Emacs => result.end,
                };
                self.ensure_cursor_visible();
            }
        }
        Ok(())
    }

    pub(super) fn rebuild_search_results(&mut self, plain_text: bool) {
        self.search_results.clear();
        if self.search_pattern.is_empty() {
            self.search_current = None;
            return;
        }
        let case_insensitive = self
            .search_pattern
            .chars()
            .all(|ch| !ch.is_ascii_uppercase());
        let plain_needle = plain_text.then(|| {
            if case_insensitive {
                Cow::Owned(self.search_pattern.to_lowercase())
            } else {
                Cow::Borrowed(self.search_pattern.as_str())
            }
        });
        let regex = if plain_text {
            None
        } else {
            let mut builder = RegexBuilder::new(&self.search_pattern);
            builder.case_insensitive(case_insensitive);
            builder.size_limit(SEARCH_REGEX_SIZE_LIMIT);
            builder.dfa_size_limit(SEARCH_REGEX_DFA_SIZE_LIMIT);
            match builder.build() {
                Ok(regex) => Some(regex),
                Err(_) => {
                    self.search_count_partial = true;
                    return;
                }
            }
        };
        for y in 0..self.total_lines() {
            let line = self.line(y);
            let map = LineTextMap::new(&line);
            if map.text.is_empty() {
                continue;
            }
            if plain_text {
                let needle = plain_needle
                    .as_ref()
                    .expect("plain search needle must be initialized");
                let needle = needle.as_ref();
                let haystack = if case_insensitive {
                    Cow::Owned(map.text.to_lowercase())
                } else {
                    Cow::Borrowed(map.text.as_str())
                };
                let mut offset = 0;
                while let Some(found) = haystack[offset..].find(needle) {
                    let start = offset + found;
                    let end = start + needle.len();
                    if let Some(result) = map.match_range(y, start..end) {
                        self.search_results.push(result);
                    }
                    offset = start.saturating_add(needle.len().max(1));
                    if offset >= haystack.len() {
                        break;
                    }
                }
            } else if let Some(regex) = &regex {
                for matched in regex.find_iter(&map.text) {
                    if let Some(result) = map.match_range(y, matched.start()..matched.end()) {
                        self.search_results.push(result);
                    }
                }
            }
        }
    }

    fn advance_search_match(&mut self, direction: SearchDirection) {
        if self.search_results.is_empty() {
            return;
        }
        let current = self.search_current;
        let next = match direction {
            SearchDirection::Forward => {
                // Advance to the next match after the current one.
                let next = current
                    .and_then(|index| {
                        if index + 1 < self.search_results.len() {
                            Some(index + 1)
                        } else if self.wrap_search {
                            Some(0)
                        } else {
                            None
                        }
                    })
                    .or_else(|| self.find_search_match(direction));
                if next.is_none() && current.is_some() {
                    return;
                }
                next
            }
            SearchDirection::Backward => {
                // Go to the previous match before the current one.
                let next = current
                    .and_then(|index| {
                        if index > 0 {
                            Some(index - 1)
                        } else if self.wrap_search {
                            Some(self.search_results.len() - 1)
                        } else {
                            None
                        }
                    })
                    .or_else(|| self.find_search_match(direction));
                if next.is_none() && current.is_some() {
                    return;
                }
                next
            }
        };
        self.search_current = next;
        if let Some(index) = next {
            if let Some(result) = self.search_results.get(index) {
                self.cursor = match self.mode_keys {
                    ModeKeys::Vi => result.start,
                    ModeKeys::Emacs => result.end,
                };
                self.ensure_cursor_visible();
            }
        }
        self.search_highlighted = true;
    }

    fn find_search_match(&self, direction: SearchDirection) -> Option<usize> {
        match direction {
            SearchDirection::Forward => self
                .search_results
                .iter()
                .enumerate()
                .find(|(_, result)| position_ge(result.start, self.cursor))
                .map(|(index, _)| index)
                .or_else(|| (self.wrap_search && !self.search_results.is_empty()).then_some(0)),
            SearchDirection::Backward => self
                .search_results
                .iter()
                .enumerate()
                .rev()
                .find(|(_, result)| position_le(result.end, self.cursor))
                .map(|(index, _)| index)
                .or_else(|| {
                    self.wrap_search
                        .then(|| self.search_results.len().checked_sub(1))
                        .flatten()
                }),
        }
    }

    fn execute_jump(&mut self, kind: JumpKind, ch: char) -> Result<(), RmuxError> {
        let line = self.line(self.cursor.y);
        let positions = owner_positions(&line);
        if positions.is_empty() {
            return Ok(());
        }
        let current_owner = line.owning_cell_x(self.cursor.x).unwrap_or(0);
        let found = match kind {
            JumpKind::Forward => positions
                .iter()
                .copied()
                .find(|x| *x > current_owner && line_char(&line, *x) == Some(ch)),
            JumpKind::ToForward => positions
                .iter()
                .copied()
                .find(|x| *x > current_owner.saturating_add(1) && line_char(&line, *x) == Some(ch)),
            JumpKind::Backward => positions
                .iter()
                .rev()
                .copied()
                .find(|x| *x < current_owner && line_char(&line, *x) == Some(ch)),
            JumpKind::ToBackward => positions.iter().rev().copied().find(|x| {
                self.next_owner_in_line(&line, *x)
                    .is_some_and(|next| next < current_owner)
                    && line_char(&line, *x) == Some(ch)
            }),
        };
        if let Some(found) = found {
            self.cursor.x = match kind {
                JumpKind::Forward | JumpKind::Backward => found,
                JumpKind::ToForward => previous_owner(&positions, found).unwrap_or(found),
                JumpKind::ToBackward => self.next_owner_in_line(&line, found).unwrap_or(found),
            };
            self.ensure_cursor_visible();
            self.sync_selection_with_cursor();
        }
        Ok(())
    }
}

fn previous_owner(positions: &[u32], found: u32) -> Option<u32> {
    positions.iter().rev().copied().find(|x| *x < found)
}
