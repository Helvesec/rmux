//! Rendering a line to text or bytes, with or without escape sequences, and
//! the columns the rendering paints.

use crate::grid::{
    append_cell_text, append_grid_string_code, append_hyperlink, GridRenderOptions, GridStringState,
};
use crate::hyperlinks::Hyperlinks;
use crate::input::{Colour, GridAttr, COLOUR_DEFAULT, COLOUR_NONE, COLOUR_TERMINAL};
use crate::style::Style;

use super::{GridCell, GridCellFlags, GridLine, RenderedLineSpan};

impl GridLine {
    pub(in crate::grid) fn render_text(&self) -> String {
        if let Some(text) = &self.plain_text {
            return text.trim_end_matches(' ').to_owned();
        }
        let mut rendered = String::new();
        for cell in &self.cells {
            if cell.flags.contains(GridCellFlags::PADDING) {
                continue;
            }
            rendered.push_str(cell.text.as_str());
        }
        while rendered.ends_with(' ') {
            rendered.pop();
        }
        rendered
    }

    fn tmux_capture_cell_end(&self, line_width: usize) -> usize {
        let used_end = self.painted_end();
        if used_end == 0 {
            return 0;
        }
        if self.ends_with_default_spaces_after_styled_cells(used_end) {
            return used_end;
        }
        self.tmux_cell_capacity(line_width)
    }

    fn render_cell_end(&self, line_width: usize, options: GridRenderOptions) -> usize {
        let used_end = self.painted_end();
        if options.trim_spaces {
            return if options.include_empty_cells {
                used_end.min(line_width)
            } else {
                used_end
            };
        }
        if options.include_empty_cells && options.use_tmux_cell_capacity {
            self.tmux_capture_cell_end(line_width)
        } else if options.include_empty_cells {
            line_width
        } else {
            used_end
        }
    }

    fn ends_with_default_spaces_after_styled_cells(&self, used_end: usize) -> bool {
        let mut first_trailing_space = used_end;
        while first_trailing_space > 0
            && self.cells[first_trailing_space - 1].is_explicit_default_space()
        {
            first_trailing_space -= 1;
        }

        first_trailing_space < used_end
            && self.cells[..first_trailing_space]
                .iter()
                .any(GridCell::has_non_default_style)
    }

    pub(in crate::grid) fn render_with_options(
        &self,
        line_width: usize,
        options: GridRenderOptions,
        state: &mut GridStringState,
        hyperlinks: Option<&Hyperlinks>,
    ) -> String {
        self.render_with_options_measured(line_width, options, state, hyperlinks)
            .0
    }

    /// Renders the line and reports the columns the rendered text paints.
    ///
    /// Replaying a rendered line moves the cursor by the reported span, so
    /// callers that place the following line by autowrap need the span rather
    /// than the byte length.
    pub(in crate::grid) fn render_with_options_measured(
        &self,
        line_width: usize,
        options: GridRenderOptions,
        state: &mut GridStringState,
        hyperlinks: Option<&Hyperlinks>,
    ) -> (String, RenderedLineSpan) {
        if let Some(text) = &self.plain_text {
            let mut rendered = String::new();
            append_plain_text_state_prefix(options, state, hyperlinks, &mut rendered);
            // A compacted line holds default single-column ASCII, so every
            // rendered byte paints exactly one column.
            let used_end = self.plain_text_render_end(text.len(), options);
            let columns =
                append_plain_text_with_options(text, used_end, line_width, options, &mut rendered);
            return (rendered, RenderedLineSpan::leading_one(columns));
        }
        let mut rendered = String::new();
        let mut has_link = false;
        let end = self.render_cell_end(line_width, options);
        let mut span = RenderedLineSpan::default();

        for cell in self.cells.iter().take(end) {
            if cell.flags.contains(GridCellFlags::PADDING) {
                continue;
            }
            if options.with_sequences {
                append_grid_string_code(
                    &state.last_cell,
                    cell,
                    &mut rendered,
                    options.escape_sequences,
                    hyperlinks,
                    &mut has_link,
                );
                state.last_cell = cell.clone();
            }
            append_cell_text(cell, &mut rendered, options.escape_sequences);
            span.push(u32::from(cell.width.max(1)));
        }
        if options.include_empty_cells && end > self.cells.len() {
            let filled = end - self.cells.len();
            rendered.extend(std::iter::repeat_n(' ', filled));
            for _ in 0..filled {
                span.push(1);
            }
        }

        if has_link {
            append_hyperlink(&mut rendered, "", "", options.escape_sequences);
        }
        if options.trim_spaces {
            while rendered.ends_with(' ') {
                rendered.pop();
                span.pop_column();
            }
        }
        (rendered, span)
    }

    pub(in crate::grid) fn render_bytes_with_options(
        &self,
        line_width: usize,
        options: GridRenderOptions,
        output: &mut Vec<u8>,
    ) -> bool {
        if options.with_sequences || options.escape_sequences {
            return false;
        }
        if let Some(text) = &self.plain_text {
            let used_end = self.plain_text_render_end(text.len(), options);
            append_plain_text_bytes_with_options(text, used_end, line_width, options, output);
            return true;
        }

        let start = output.len();
        let end = self.render_cell_end(line_width, options);

        for cell in self.cells.iter().take(end) {
            if cell.flags.contains(GridCellFlags::PADDING) {
                continue;
            }
            if cell.flags.contains(GridCellFlags::TAB) {
                output.push(b'\t');
                continue;
            }
            output.extend_from_slice(cell.text().as_bytes());
        }
        if options.include_empty_cells && end > self.cells.len() {
            output.extend(std::iter::repeat_n(b' ', end - self.cells.len()));
        }
        if options.trim_spaces {
            trim_trailing_ascii_spaces(output, start);
        }
        true
    }

    pub(in crate::grid) fn render_with_default_style(
        &self,
        line_width: usize,
        options: GridRenderOptions,
        state: &mut GridStringState,
        hyperlinks: Option<&Hyperlinks>,
        style: &Style,
    ) -> String {
        let mut line = self.clone();
        line.overlay_default_style(style);
        line.render_with_options(line_width, options, state, hyperlinks)
    }

    fn overlay_default_style(&mut self, style: &Style) {
        if self.plain_text.is_some() {
            self.materialize_plain_text(self.width as usize);
        }
        let background = effective_background(style);
        for cell in &mut self.cells {
            if cell.is_padding() {
                continue;
            }
            if style_colour_is_set(style.cell.fg) && style_colour_is_unset(cell.fg()) {
                cell.set_fg(style.cell.fg);
            }
            if style_colour_is_set(background) && style_colour_is_unset(cell.bg()) {
                cell.set_bg(background);
            }
            if style_colour_is_set(style.cell.us) && style_colour_is_unset(cell.us()) {
                cell.set_us(style.cell.us);
            }
            if style.cell.attr != 0 && cell.attr() == 0 {
                cell.set_attr(style.cell.attr & !GridAttr::NOATTR);
            }
        }
    }

    fn plain_text_render_end(&self, text_len: usize, options: GridRenderOptions) -> usize {
        if options.join_wrapped && !options.trim_spaces {
            usize::from(self.plain_text_data_end).max(text_len)
        } else {
            text_len
        }
    }
}

fn effective_background(style: &Style) -> Colour {
    if style_colour_is_set(style.cell.bg) {
        style.cell.bg
    } else {
        style.fill
    }
}

fn style_colour_is_set(colour: Colour) -> bool {
    !style_colour_is_unset(colour)
}

fn style_colour_is_unset(colour: Colour) -> bool {
    matches!(colour, COLOUR_DEFAULT | COLOUR_TERMINAL | COLOUR_NONE)
}

fn append_plain_text_state_prefix(
    options: GridRenderOptions,
    state: &mut GridStringState,
    hyperlinks: Option<&Hyperlinks>,
    output: &mut String,
) {
    if !options.with_sequences {
        return;
    }
    let default_cell = GridCell::blank_with_bg(COLOUR_DEFAULT);
    let mut has_link = false;
    append_grid_string_code(
        &state.last_cell,
        &default_cell,
        output,
        options.escape_sequences,
        hyperlinks,
        &mut has_link,
    );
    state.last_cell = default_cell;
    if has_link {
        append_hyperlink(output, "", "", options.escape_sequences);
    }
}

/// Appends the compacted plain-ASCII text and returns the columns it paints.
fn append_plain_text_with_options(
    text: &str,
    used_end: usize,
    line_width: usize,
    options: GridRenderOptions,
    output: &mut String,
) -> u32 {
    let start = output.len();
    let end = plain_text_capture_end(used_end, line_width, options);
    let copied_end = text.len().min(end);
    output.push_str(&text[..copied_end]);
    if text.len() < end {
        output.extend(std::iter::repeat_n(' ', end - text.len()));
    }
    if options.trim_spaces {
        trim_trailing_spaces(output, start);
    }
    u32::try_from(output.len().saturating_sub(start)).unwrap_or(u32::MAX)
}

fn append_plain_text_bytes_with_options(
    text: &str,
    used_end: usize,
    line_width: usize,
    options: GridRenderOptions,
    output: &mut Vec<u8>,
) {
    let start = output.len();
    let end = plain_text_capture_end(used_end, line_width, options);
    let bytes = text.as_bytes();
    output.extend_from_slice(&bytes[..bytes.len().min(end)]);
    if bytes.len() < end {
        output.extend(std::iter::repeat_n(b' ', end - bytes.len()));
    }
    if options.trim_spaces {
        trim_trailing_ascii_spaces(output, start);
    }
}

pub(super) fn plain_text_capture_end(
    used_end: usize,
    line_width: usize,
    options: GridRenderOptions,
) -> usize {
    if options.trim_spaces {
        return if options.include_empty_cells {
            used_end.min(line_width)
        } else {
            used_end
        };
    }
    if options.include_empty_cells && options.use_tmux_cell_capacity {
        if used_end == 0 {
            return 0;
        }
        let bucket_used = used_end.max(1);
        let quarter = (line_width / 4).max(1);
        let half = (line_width / 2).max(quarter);
        if bucket_used < quarter {
            quarter
        } else if bucket_used < half {
            half
        } else {
            line_width
        }
    } else if options.include_empty_cells {
        line_width
    } else {
        used_end
    }
}

fn trim_trailing_ascii_spaces(output: &mut Vec<u8>, floor: usize) {
    while output.len() > floor && output.last() == Some(&b' ') {
        output.pop();
    }
}

fn trim_trailing_spaces(output: &mut String, floor: usize) {
    while output.len() > floor && output.ends_with(' ') {
        output.pop();
    }
}
