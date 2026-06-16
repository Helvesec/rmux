use std::ops::Range;

use rmux_core::ScreenLineView;

use super::types::{CopyPosition, SearchMatch};

const REGEX_METACHARS: &[char] = &[
    '.', '^', '$', '*', '+', '?', '(', ')', '[', ']', '{', '}', '|', '\\',
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WordBoundary {
    NextStart,
    NextEnd,
    PreviousStart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CopyRange {
    pub(super) start: CopyPosition,
    pub(super) end: CopyPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WordClass {
    Word,
    Separator,
    Space,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LineTextMap {
    pub(super) text: String,
    spans: Vec<(Range<usize>, u32)>,
}

impl LineTextMap {
    pub(super) fn new(line: &ScreenLineView) -> Self {
        let mut text = String::new();
        let mut spans = Vec::new();
        for x in owner_positions(line) {
            let start = text.len();
            if let Some(cell) = line.cell(x) {
                text.push_str(cell.text());
            } else {
                text.push(' ');
            }
            spans.push((start..text.len(), x));
        }
        Self { text, spans }
    }

    pub(super) fn match_range(&self, y: usize, range: Range<usize>) -> Option<SearchMatch> {
        let start_x = self
            .spans
            .iter()
            .find(|(span, _)| span.start <= range.start && range.start < span.end)
            .map(|(_, x)| *x)?;
        let end_x = self
            .spans
            .iter()
            .rev()
            .find(|(span, _)| span.start < range.end && range.end <= span.end)
            .or_else(|| {
                self.spans
                    .iter()
                    .rev()
                    .find(|(span, _)| span.start < range.end)
            })
            .map(|(_, x)| *x)?;
        Some(SearchMatch {
            start: CopyPosition { x: start_x, y },
            end: CopyPosition { x: end_x, y },
            text: self.text.get(range)?.to_owned(),
        })
    }
}

pub(super) fn normalize_positions(
    left: CopyPosition,
    right: CopyPosition,
) -> (CopyPosition, CopyPosition) {
    if position_le(left, right) {
        (left, right)
    } else {
        (right, left)
    }
}

pub(super) fn position_le(left: CopyPosition, right: CopyPosition) -> bool {
    left.y < right.y || (left.y == right.y && left.x <= right.x)
}

pub(super) fn position_ge(left: CopyPosition, right: CopyPosition) -> bool {
    left.y > right.y || (left.y == right.y && left.x >= right.x)
}

pub(super) fn owner_positions(line: &ScreenLineView) -> Vec<u32> {
    (0..line.width())
        .filter(|x| line.cell(*x).is_none_or(|cell| !cell.is_padding()))
        .collect()
}

pub(super) fn line_char(line: &ScreenLineView, x: u32) -> Option<char> {
    let owner = line.owning_cell_x(x).unwrap_or(x);
    match line.cell(owner) {
        Some(cell) => cell.text().chars().next(),
        None if owner < line.width() => Some(' '),
        None => None,
    }
}

pub(super) fn classify_word_char(ch: char, separators: &str, spaces_only: bool) -> WordClass {
    if ch.is_whitespace() {
        WordClass::Space
    } else if !spaces_only && separators.contains(ch) {
        WordClass::Separator
    } else {
        WordClass::Word
    }
}

pub(super) fn pattern_looks_like_regex(pattern: &str) -> bool {
    pattern.chars().any(|ch| REGEX_METACHARS.contains(&ch))
}

pub(super) fn scrollbar_slider_height(rows: u16, history_size: usize) -> usize {
    let sb_h = usize::from(rows.max(1));
    let total_height = history_size.saturating_add(sb_h).max(1);
    (((sb_h as f64) * ((sb_h as f64) / (total_height as f64))).floor() as usize)
        .max(1)
        .min(sb_h)
}
