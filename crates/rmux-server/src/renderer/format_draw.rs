//! tmux `format_draw` analog — walks an expanded string containing `#[...]`
//! style clauses, routes output to alignment buckets (left/centre/right/list),
//! and composites them onto a fixed-width canvas for final rendering.

use rmux_core::{
    text_width as tmux_text_width, Colour, Style, StyleAlign, Utf8Config, COLOUR_DEFAULT,
};

use crate::status_ranges::{StatusRange, StatusRangeType};

use super::{cursor_position_bytes, style_sgr_bytes};

mod clause_parse;
use clause_parse::parse_expanded_styles;

mod layout;
use layout::layout_parsed_line;

mod overlay;

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
        for slot in &mut self.canvas.slots {
            if let CanvasSlot::Continuation(lead) = slot {
                *lead = (*lead).saturating_sub(1);
            }
        }
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
    render_formatted_line_segment(frame, x, y, line, 0, line.width);
}

pub(crate) fn render_formatted_line_segment(
    frame: &mut Vec<u8>,
    x: u16,
    y: u16,
    line: &FormattedLine,
    start: usize,
    width: usize,
) {
    if line.width == 0 {
        return;
    }
    let end = start.saturating_add(width).min(line.width);
    if start >= end {
        return;
    }

    frame.extend_from_slice(b"\x1b7\x1b[0m");
    frame.extend_from_slice(cursor_position_bytes(y, x).as_slice());
    write_formatted_cells(frame, line, start, end);
    frame.extend_from_slice(b"\x1b8");
}

/// Writes the cells of one drawn line, and nothing around them.
///
/// This is the loop `render_formatted_line_segment` has always had, without its
/// envelope: no cursor save, no restore and no positioning. A caller that feeds
/// a pane's own transcript needs exactly this - a line of cells - and must not
/// push a cursor save into it; the callers that paint a client's screen keep
/// their envelope, byte for byte, around this same loop.
pub(crate) fn write_formatted_cells(
    frame: &mut Vec<u8>,
    line: &FormattedLine,
    start: usize,
    end: usize,
) {
    let mut active_style: Option<Style> = None;
    let mut column = start;
    while column < end {
        let (cell, complete) = match &line.canvas.slots[column] {
            CanvasSlot::Lead(cell) => (cell, column + cell.width <= end),
            CanvasSlot::Continuation(lead) => {
                let Some(CanvasSlot::Lead(cell)) = line.canvas.slots.get(*lead) else {
                    frame.push(b' ');
                    column += 1;
                    continue;
                };
                (cell, false)
            }
            CanvasSlot::Empty => {
                frame.push(b' ');
                column += 1;
                continue;
            }
        };
        if active_style.as_ref() != Some(&cell.style) {
            if active_style.is_some() {
                frame.extend_from_slice(b"\x1b[0m");
            }
            frame.extend_from_slice(style_sgr_bytes(&cell.style, true).as_slice());
            active_style = Some(cell.style.clone());
        }
        if complete {
            frame.extend_from_slice(cell.text.as_bytes());
            column += cell.width;
        } else {
            frame.push(b' ');
            column += 1;
        }
    }
    frame.extend_from_slice(b"\x1b[0m");
}

// ---------------------------------------------------------------------------
// Layout — composites alignment buckets onto the canvas
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn push_range(ranges: &mut Vec<StatusRange>, start: usize, end: usize, kind: StatusRangeType) {
    super::push_range(ranges, start, end, kind);
}

#[cfg(test)]
#[path = "format_draw/tests.rs"]
mod tests;
