//! tmux `format_draw` analog — walks an expanded string containing `#[...]`
//! style clauses, routes output to alignment buckets (left/centre/right/list),
//! and composites them onto a fixed-width canvas for final rendering.

use rmux_core::{
    format_skip_delimiter, style_parse, text_width as tmux_text_width, Colour, Style, StyleAlign,
    StyleCell, StyleDefaultType, StyleList, StyleRange, Utf8Config, COLOUR_DEFAULT,
};

use crate::status_ranges::{StatusRange, StatusRangeType};

use super::{cursor_position_bytes, style_sgr_bytes};

#[path = "format_draw/layout.rs"]
mod layout;
use layout::layout_parsed_line;

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

const BUCKET_COUNT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BucketIndex {
    Left = 0,
    Centre = 1,
    Right = 2,
    AbsoluteCentre = 3,
    List = 4,
    ListLeft = 5,
    ListRight = 6,
    After = 7,
}

impl BucketIndex {
    const fn idx(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListState {
    Before,
    Inside,
    After,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DrawCell {
    text: String,
    style: Style,
    range: Option<StatusRangeType>,
    width: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DrawBucket {
    cells: Vec<DrawCell>,
    width: usize,
}

impl DrawBucket {
    fn push_char(
        &mut self,
        ch: char,
        style: &Style,
        range: Option<StatusRangeType>,
        utf8: &Utf8Config,
    ) {
        let text = ch.to_string();
        let width = tmux_text_width(&text, utf8);
        if width == 0 {
            if let Some(last) = self.cells.last_mut() {
                last.text.push(ch);
            }
            return;
        }
        self.cells.push(DrawCell {
            text,
            style: style.clone(),
            range,
            width,
        });
        self.width += width;
    }

    fn extend(&mut self, cells: impl IntoIterator<Item = DrawCell>) {
        for cell in cells {
            self.width += cell.width;
            self.cells.push(cell);
        }
    }

    fn slice(&self, start: usize, width: usize) -> Vec<DrawCell> {
        if width == 0 {
            return Vec::new();
        }

        let end = start.saturating_add(width);
        let mut position = 0_usize;
        let mut out = Vec::new();

        for cell in &self.cells {
            let cell_start = position;
            let cell_end = position.saturating_add(cell.width);
            position = cell_end;

            if cell_end <= start {
                continue;
            }
            if cell_start >= end {
                break;
            }
            if cell_start < start || cell_end > end {
                continue;
            }
            out.push(cell.clone());
        }

        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CanvasSlot {
    Empty,
    Lead(DrawCell),
    Continuation(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Canvas {
    slots: Vec<CanvasSlot>,
}

impl Canvas {
    fn new(width: usize, filler: &Style) -> Self {
        let mut slots = Vec::with_capacity(width);
        for _ in 0..width {
            slots.push(CanvasSlot::Lead(DrawCell {
                text: " ".to_owned(),
                style: filler.clone(),
                range: None,
                width: 1,
            }));
        }
        Self { slots }
    }

    fn clear_column(&mut self, index: usize) {
        if index >= self.slots.len() {
            return;
        }

        match self.slots[index].clone() {
            CanvasSlot::Empty => {}
            CanvasSlot::Lead(cell) => {
                self.slots[index] = CanvasSlot::Empty;
                for offset in 1..cell.width {
                    if index + offset < self.slots.len() {
                        self.slots[index + offset] = CanvasSlot::Empty;
                    }
                }
            }
            CanvasSlot::Continuation(lead) => self.clear_column(lead),
        }
    }

    fn place_cell(&mut self, offset: usize, cell: DrawCell) {
        if cell.width == 0
            || offset >= self.slots.len()
            || offset.saturating_add(cell.width) > self.slots.len()
        {
            return;
        }

        for column in offset..offset.saturating_add(cell.width) {
            self.clear_column(column);
        }
        self.slots[offset] = CanvasSlot::Lead(cell.clone());
        for column in offset + 1..offset + cell.width {
            self.slots[column] = CanvasSlot::Continuation(offset);
        }
    }

    fn overlay_cells(&mut self, offset: usize, cells: &[DrawCell]) {
        let mut x = offset;
        for cell in cells {
            if x.saturating_add(cell.width) > self.slots.len() {
                break;
            }
            self.place_cell(x, cell.clone());
            x = x.saturating_add(cell.width);
        }
    }

    fn ranges(&self) -> Vec<StatusRange> {
        let mut ranges = Vec::new();
        let mut current_kind: Option<StatusRangeType> = None;
        let mut current_start = 0_usize;

        for column in 0..self.slots.len() {
            let next_kind = self.range_at(column);
            if next_kind == current_kind {
                continue;
            }

            if let Some(kind) = current_kind.take() {
                push_range(&mut ranges, current_start, column.saturating_sub(1), kind);
            }

            current_kind = next_kind;
            current_start = column;
        }

        if let Some(kind) = current_kind {
            push_range(
                &mut ranges,
                current_start,
                self.slots.len().saturating_sub(1),
                kind,
            );
        }

        ranges
    }

    fn range_at(&self, index: usize) -> Option<StatusRangeType> {
        match self.slots.get(index) {
            Some(CanvasSlot::Lead(cell)) => cell.range.clone(),
            Some(CanvasSlot::Continuation(lead)) => match self.slots.get(*lead) {
                Some(CanvasSlot::Lead(cell)) => cell.range.clone(),
                _ => None,
            },
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedFormatDraw {
    buckets: [DrawBucket; BUCKET_COUNT],
    list_align: StyleAlign,
    focus_start: Option<usize>,
    focus_end: Option<usize>,
    fill: Option<Colour>,
}

impl ParsedFormatDraw {
    fn new(base: &Style) -> Self {
        Self {
            buckets: std::array::from_fn(|_| DrawBucket::default()),
            list_align: StyleAlign::Default,
            focus_start: None,
            focus_end: None,
            fill: (base.fill != COLOUR_DEFAULT).then_some(base.fill),
        }
    }

    fn bucket(&self, index: BucketIndex) -> &DrawBucket {
        &self.buckets[index.idx()]
    }

    fn bucket_mut(&mut self, index: BucketIndex) -> &mut DrawBucket {
        &mut self.buckets[index.idx()]
    }
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
// Public API
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FormattedLine {
    width: usize,
    canvas: Canvas,
    pub(crate) ranges: Vec<StatusRange>,
}

impl FormattedLine {
    pub(crate) fn width(&self) -> usize {
        self.width
    }

    pub(crate) fn trim_leading_ascii_space(mut self) -> Self {
        let Some(CanvasSlot::Lead(cell)) = self.canvas.slots.first() else {
            return self;
        };
        if cell.text != " " || cell.width != 1 {
            return self;
        }

        self.canvas.slots.remove(0);
        self.width = self.width.saturating_sub(1);
        self.ranges = self.canvas.ranges();
        self
    }
}

pub(crate) fn format_draw_line(
    expanded: &str,
    base: &Style,
    available: usize,
    utf8: &Utf8Config,
) -> FormattedLine {
    if available == 0 {
        return FormattedLine {
            width: 0,
            canvas: Canvas { slots: Vec::new() },
            ranges: Vec::new(),
        };
    }

    let parsed = parse_expanded_styles(expanded, base, utf8);
    let mut filler = base.clone();
    if let Some(fill) = parsed.fill {
        filler.fill = fill;
    }
    let mut canvas = Canvas::new(available, &filler);
    layout_parsed_line(&mut canvas, &parsed, available);
    let ranges = canvas.ranges();

    FormattedLine {
        width: available,
        canvas,
        ranges,
    }
}

pub(crate) fn format_draw_content_width(expanded: &str, base: &Style, utf8: &Utf8Config) -> usize {
    let parsed = parse_expanded_styles(expanded, base, utf8);
    parsed.buckets.iter().map(|bucket| bucket.width).sum()
}

pub(crate) fn render_formatted_line(frame: &mut Vec<u8>, x: u16, y: u16, line: &FormattedLine) {
    if line.width == 0 {
        return;
    }

    frame.extend_from_slice(b"\x1b7\x1b[0m");
    frame.extend_from_slice(cursor_position_bytes(y, x).as_slice());

    let mut active_style: Option<Style> = None;
    for slot in &line.canvas.slots {
        let CanvasSlot::Lead(cell) = slot else {
            continue;
        };
        if active_style.as_ref() != Some(&cell.style) {
            if active_style.is_some() {
                frame.extend_from_slice(b"\x1b[0m");
            }
            frame.extend_from_slice(style_sgr_bytes(&cell.style, true).as_slice());
            active_style = Some(cell.style.clone());
        }
        frame.extend_from_slice(cell.text.as_bytes());
    }

    frame.extend_from_slice(b"\x1b[0m\x1b8");
}

// ---------------------------------------------------------------------------
// Clause parser — walks expanded text, processes `#[...]` style clauses
// ---------------------------------------------------------------------------

fn parse_expanded_styles(expanded: &str, base: &Style, utf8: &Utf8Config) -> ParsedFormatDraw {
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

// ---------------------------------------------------------------------------
// Layout — composites alignment buckets onto the canvas
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

fn push_range(ranges: &mut Vec<StatusRange>, start: usize, end: usize, kind: StatusRangeType) {
    super::push_range(ranges, start, end, kind);
}

#[cfg(test)]
#[path = "format_draw/tests.rs"]
mod tests;
