//! Clause parser: walks expanded text and routes cells to alignment buckets, applying `#[...]` style clauses.

use rmux_core::{
    format_skip_delimiter, style_parse, Style, StyleAlign, StyleCell, StyleDefaultType, StyleList,
    StyleRange, Utf8Config, COLOUR_DEFAULT,
};

use crate::status_ranges::StatusRangeType;

use super::{BucketIndex, ParsedFormatDraw};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListState {
    Before,
    Inside,
    After,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParseState {
    parsed: ParsedFormatDraw,
    current_bucket: BucketIndex,
    current_style: Style,
    current_default: StyleCell,
    base_default: StyleCell,
    list_state: ListState,
    align_map: [BucketIndex; 5],
}

impl ParseState {
    fn new(base: &Style) -> Self {
        let default_cell = base.cell;
        Self {
            parsed: ParsedFormatDraw::new(base),
            current_bucket: BucketIndex::Left,
            current_style: base.clone(),
            current_default: default_cell,
            base_default: default_cell,
            list_state: ListState::Before,
            align_map: [
                BucketIndex::Left,
                BucketIndex::Left,
                BucketIndex::Centre,
                BucketIndex::Right,
                BucketIndex::AbsoluteCentre,
            ],
        }
    }

    fn push_char(&mut self, ch: char, utf8: &Utf8Config) {
        let range = style_range_to_status(&self.current_style.range);
        self.parsed
            .bucket_mut(self.current_bucket)
            .push_char(ch, &self.current_style, range, utf8);
    }

    fn apply_style_clause(&mut self, clause: &str) {
        let saved_style = self.current_style.clone();
        if style_parse(&mut self.current_style, &self.current_default, clause).is_err() {
            return;
        }

        if self.current_style.fill != COLOUR_DEFAULT {
            self.parsed.fill = Some(self.current_style.fill);
        }

        match self.current_style.default_type {
            StyleDefaultType::Push => {
                self.current_default = saved_style.cell;
                self.current_style.default_type = StyleDefaultType::Base;
            }
            StyleDefaultType::Pop => {
                self.current_default = self.base_default;
                self.current_style.default_type = StyleDefaultType::Base;
            }
            StyleDefaultType::Set => {
                self.base_default = saved_style.cell;
                self.current_default = saved_style.cell;
                self.current_style.default_type = StyleDefaultType::Base;
            }
            StyleDefaultType::Base => {}
        }

        match self.current_style.list {
            StyleList::On => {
                if self.list_state != ListState::Inside {
                    self.list_state = ListState::Inside;
                    self.parsed.list_align = self.current_style.align;
                }
                if self.parsed.focus_start.is_some() && self.parsed.focus_end.is_none() {
                    self.parsed.focus_end = Some(self.parsed.bucket(BucketIndex::List).width);
                }
                self.current_bucket = BucketIndex::List;
            }
            StyleList::Focus => {
                if self.list_state == ListState::Inside && self.parsed.focus_start.is_none() {
                    self.parsed.focus_start = Some(self.parsed.bucket(BucketIndex::List).width);
                }
            }
            StyleList::Off => {
                if self.list_state == ListState::Inside {
                    if self.parsed.focus_start.is_some() && self.parsed.focus_end.is_none() {
                        self.parsed.focus_end = Some(self.parsed.bucket(BucketIndex::List).width);
                    }
                    self.align_map[align_slot(self.parsed.list_align)] = BucketIndex::After;
                    if self.parsed.list_align == StyleAlign::Left {
                        self.align_map[align_slot(StyleAlign::Default)] = BucketIndex::After;
                    }
                    self.list_state = ListState::After;
                }
                self.current_bucket = self.align_map[align_slot(self.current_style.align)];
            }
            StyleList::LeftMarker => {
                if self.list_state == ListState::Inside
                    && self.parsed.bucket(BucketIndex::ListLeft).cells.is_empty()
                {
                    if self.parsed.focus_start.is_some() && self.parsed.focus_end.is_none() {
                        self.parsed.focus_start = None;
                        self.parsed.focus_end = None;
                    }
                    self.current_bucket = BucketIndex::ListLeft;
                }
            }
            StyleList::RightMarker => {
                if self.list_state == ListState::Inside
                    && self.parsed.bucket(BucketIndex::ListRight).cells.is_empty()
                {
                    if self.parsed.focus_start.is_some() && self.parsed.focus_end.is_none() {
                        self.parsed.focus_start = None;
                        self.parsed.focus_end = None;
                    }
                    self.current_bucket = BucketIndex::ListRight;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Clause parser — walks expanded text, processes `#[...]` style clauses
// ---------------------------------------------------------------------------

pub(super) fn parse_expanded_styles(
    expanded: &str,
    base: &Style,
    utf8: &Utf8Config,
) -> ParsedFormatDraw {
    let bytes = expanded.as_bytes();
    let mut index = 0_usize;
    let mut state = ParseState::new(base);

    while index < bytes.len() {
        // tmux hash-doubling: runs of `#` before `[` are halved (##[ → literal #[).
        // Odd runs open a style clause; even runs emit literal hashes plus `[`.
        if bytes[index] == b'#' && index + 1 < bytes.len() && bytes[index + 1] != b'[' {
            let mut count = 1_usize;
            while index + count < bytes.len() && bytes[index + count] == b'#' {
                count += 1;
            }
            let even = count & 1 == 0;

            if bytes.get(index + count).copied() != Some(b'[') {
                index += count;
                let draw_count = if even { count / 2 } else { (count / 2) + 1 };
                for _ in 0..draw_count {
                    state.push_char('#', utf8);
                }
                continue;
            }

            if even {
                index += count + 1;
            } else {
                index += count - 1;
            }
            if state.current_style.ignore {
                continue;
            }
            for _ in 0..(count / 2) {
                state.push_char('#', utf8);
            }
            if even {
                state.push_char('[', utf8);
            }
            continue;
        }

        if bytes[index] != b'#'
            || index + 1 >= bytes.len()
            || bytes[index + 1] != b'['
            || state.current_style.ignore
        {
            let mut chars = expanded[index..].chars();
            let Some(ch) = chars.next() else {
                break;
            };
            index += ch.len_utf8();
            if ch.is_control() {
                continue;
            }
            state.push_char(ch, utf8);
            continue;
        }

        let Some(off) = format_skip_delimiter(&expanded[index + 2..], b"]") else {
            break;
        };
        let end = index + 2 + off;
        state.apply_style_clause(&expanded[index + 2..end]);
        index = end + 1;
    }

    state.parsed
}

fn style_range_to_status(range: &StyleRange) -> Option<StatusRangeType> {
    match range {
        StyleRange::None => None,
        StyleRange::Left => Some(StatusRangeType::Left),
        StyleRange::Right => Some(StatusRangeType::Right),
        StyleRange::Pane(id) => Some(StatusRangeType::Pane(rmux_core::PaneId::new(*id))),
        StyleRange::Window(id) => Some(StatusRangeType::Window(*id)),
        StyleRange::Session(id) => Some(StatusRangeType::Session(*id)),
        StyleRange::User(_) => Some(StatusRangeType::User),
        StyleRange::Control(id) => Some(StatusRangeType::Control(*id)),
    }
}

fn align_slot(align: StyleAlign) -> usize {
    match align {
        StyleAlign::Default => 0,
        StyleAlign::Left => 1,
        StyleAlign::Centre => 2,
        StyleAlign::Right => 3,
        StyleAlign::AbsoluteCentre => 4,
    }
}
