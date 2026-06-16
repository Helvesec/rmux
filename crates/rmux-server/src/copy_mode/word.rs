//! Copy-mode word and space boundary movement.

use super::text::{classify_word_char, line_char, owner_positions, WordBoundary, WordClass};
use super::types::CopyPosition;
use super::CopyModeState;

impl CopyModeState {
    pub(super) fn find_word_boundary(
        &self,
        position: CopyPosition,
        boundary: WordBoundary,
    ) -> Option<CopyPosition> {
        self.find_boundary(position, boundary, false)
    }

    pub(super) fn find_space_boundary(
        &self,
        position: CopyPosition,
        boundary: WordBoundary,
    ) -> Option<CopyPosition> {
        self.find_boundary(position, boundary, true)
    }

    pub(super) fn flatten_owner_positions(&self) -> Vec<CopyPosition> {
        let mut positions = Vec::new();
        for y in 0..self.total_lines() {
            let line = self.line(y);
            for x in owner_positions(&line) {
                positions.push(CopyPosition { x, y });
            }
        }
        positions
    }

    fn find_boundary(
        &self,
        position: CopyPosition,
        boundary: WordBoundary,
        spaces_only: bool,
    ) -> Option<CopyPosition> {
        let positions = self.flatten_owner_positions();
        let index = positions
            .iter()
            .position(|candidate| *candidate == position)?;
        let class_at = |candidate: CopyPosition| -> WordClass {
            let line = self.line(candidate.y);
            let ch = line_char(&line, candidate.x).unwrap_or(' ');
            classify_word_char(ch, &self.word_separators, spaces_only)
        };
        let is_token = |class: WordClass| class != WordClass::Space;
        match boundary {
            WordBoundary::NextStart => {
                self.find_next_token_start(position, &positions, index, class_at, is_token)
            }
            WordBoundary::NextEnd => {
                self.find_next_token_end(position, &positions, index, class_at, is_token)
            }
            WordBoundary::PreviousStart => {
                Self::find_previous_token_start(position, &positions, index, class_at, is_token)
            }
        }
    }

    fn find_next_token_start(
        &self,
        position: CopyPosition,
        positions: &[CopyPosition],
        index: usize,
        class_at: impl Fn(CopyPosition) -> WordClass,
        is_token: impl Fn(WordClass) -> bool,
    ) -> Option<CopyPosition> {
        let current_class = class_at(position);
        let current_token = is_token(current_class).then_some(current_class);
        let mut saw_space = current_token.is_none();
        for candidate in positions.iter().copied().skip(index.saturating_add(1)) {
            let class = class_at(candidate);
            if !is_token(class) {
                saw_space = true;
                continue;
            }
            let Some(current_token) = current_token else {
                return Some(candidate);
            };
            if saw_space || class != current_token {
                return Some(candidate);
            }
        }
        Some(self.end_of_buffer_word_boundary(WordBoundary::NextStart))
    }

    fn find_next_token_end(
        &self,
        position: CopyPosition,
        positions: &[CopyPosition],
        index: usize,
        class_at: impl Fn(CopyPosition) -> WordClass,
        is_token: impl Fn(WordClass) -> bool,
    ) -> Option<CopyPosition> {
        let current_class = class_at(position);
        if is_token(current_class) {
            for candidate in positions.iter().copied().skip(index.saturating_add(1)) {
                if class_at(candidate) != current_class {
                    return Some(candidate);
                }
            }
            return Some(self.end_of_buffer_word_boundary(WordBoundary::NextEnd));
        }

        let mut token_class = None;
        for candidate in positions.iter().copied().skip(index.saturating_add(1)) {
            let class = class_at(candidate);
            if !is_token(class) {
                if token_class.is_some() {
                    return Some(candidate);
                }
                continue;
            }
            match token_class {
                Some(start_class) if start_class == class => {}
                Some(_) => return Some(candidate),
                None => token_class = Some(class),
            }
        }
        Some(self.end_of_buffer_word_boundary(WordBoundary::NextEnd))
    }

    fn find_previous_token_start(
        position: CopyPosition,
        positions: &[CopyPosition],
        index: usize,
        class_at: impl Fn(CopyPosition) -> WordClass,
        is_token: impl Fn(WordClass) -> bool,
    ) -> Option<CopyPosition> {
        let current_class = class_at(position);
        if is_token(current_class) {
            let mut start = position;
            let mut moved = false;
            for candidate in positions.iter().copied().take(index).rev() {
                if class_at(candidate) != current_class {
                    break;
                }
                moved = true;
                start = candidate;
            }
            if moved {
                return Some(start);
            }
        }

        let mut token_class = None;
        let mut token_start = None;
        for candidate in positions.iter().copied().take(index).rev() {
            let class = class_at(candidate);
            if !is_token(class) {
                if token_start.is_some() {
                    break;
                }
                continue;
            }
            match token_class {
                Some(start_class) if start_class == class => {
                    token_start = Some(candidate);
                }
                Some(_) => break,
                None => {
                    token_class = Some(class);
                    token_start = Some(candidate);
                }
            }
        }
        token_start
    }

    fn end_of_buffer_word_boundary(&self, boundary: WordBoundary) -> CopyPosition {
        let y = self.total_lines().saturating_sub(1);
        let x = match boundary {
            WordBoundary::NextStart => self.end_of_buffer_next_start_x(y),
            WordBoundary::NextEnd => self.end_of_buffer_next_end_x(y),
            WordBoundary::PreviousStart => 0,
        };
        CopyPosition { x, y }
    }

    fn end_of_buffer_next_start_x(&self, y: usize) -> u32 {
        self.end_of_buffer_next_x(y)
            .unwrap_or_else(|| 1.min(self.cols().saturating_sub(1)))
    }

    fn end_of_buffer_next_end_x(&self, y: usize) -> u32 {
        self.end_of_buffer_next_x(y).unwrap_or(0)
    }

    fn end_of_buffer_next_x(&self, y: usize) -> Option<u32> {
        let line = self.line(y);
        let positions = owner_positions(&line);
        let last_content = positions.iter().copied().rev().find(|x| {
            line.cell(*x)
                .is_some_and(|cell| !cell.text().chars().all(char::is_whitespace))
        })?;
        Some(
            positions
                .into_iter()
                .find(|x| *x > last_content)
                .unwrap_or_else(|| line.width()),
        )
    }
}
