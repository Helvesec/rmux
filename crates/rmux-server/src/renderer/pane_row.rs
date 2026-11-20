use std::io::Write as _;

use rmux_core::{
    text_width as tmux_text_width, truncate_to_width as tmux_truncate_to_width, Screen, Utf8Config,
};

use super::cursor_position_bytes;

const ECH_MIN_COLUMNS: u16 = 10;
const OSC8_CLOSE: &[u8] = b"\x1b]8;;\x1b\\";

/// One pane row after renderer-generated cleared cells have been separated
/// from application-authored cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenderedPaneRow {
    bytes: Vec<u8>,
    columns: u16,
}

impl RenderedPaneRow {
    pub(super) const fn new(bytes: Vec<u8>, columns: u16) -> Self {
        Self { bytes, columns }
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(super) fn bytes_mut(&mut self) -> &mut Vec<u8> {
        &mut self.bytes
    }

    pub(super) const fn columns(&self) -> u16 {
        self.columns
    }

    pub(super) fn extend_columns_to(&mut self, columns: u16) {
        self.columns = self.columns.max(columns);
    }
}

#[cfg(test)]
#[allow(dead_code)]
pub(super) fn capture_pane_empty_tail_starts(screen: &Screen, rows: u16, width: u16) -> Vec<u16> {
    capture_pane_empty_tail_starts_from(screen, 0, usize::from(rows), width)
}

pub(super) fn capture_pane_empty_tail_starts_from(
    screen: &Screen,
    source_row_start: usize,
    rows: usize,
    width: u16,
) -> Vec<u16> {
    (0..rows)
        .map(|row| {
            screen
                .visible_line_tmux_empty_tail_start(
                    source_row_start.saturating_add(row),
                    usize::from(width),
                )
                .unwrap_or(0)
        })
        .collect()
}

pub(super) fn truncate_rendered_pane_row(
    line: &[u8],
    width: usize,
    utf8: &Utf8Config,
) -> RenderedPaneRow {
    if line.is_ascii() {
        return truncate_rendered_ascii_pane_row(line, width, utf8);
    }

    let Ok(line_text) = std::str::from_utf8(line) else {
        return RenderedPaneRow::new(Vec::new(), 0);
    };
    let mut visible_text = String::with_capacity(line.len());
    let mut index = 0_usize;
    while index < line.len() {
        if let Some(end) = rendered_control_end(line, index) {
            index = end;
            continue;
        }

        let Some(ch) = line_text[index..].chars().next() else {
            break;
        };
        visible_text.push(ch);
        index += ch.len_utf8();
    }
    let visible_prefix_len = tmux_truncate_to_width(&visible_text, width, utf8).len();
    let columns = tmux_text_width(&visible_text[..visible_prefix_len], utf8);

    let mut output = Vec::with_capacity(line.len().min(width.saturating_mul(4)));
    let mut visible_copied = 0_usize;
    let mut hyperlink_active = false;
    index = 0;
    while index < line.len() {
        if let Some(end) = rendered_control_end(line, index) {
            let sequence = &line[index..end];
            update_osc8_state(sequence, &mut hyperlink_active);
            output.extend_from_slice(sequence);
            index = end;
            continue;
        }

        let Some(ch) = line_text[index..].chars().next() else {
            break;
        };
        let ch_len = ch.len_utf8();
        if visible_copied.saturating_add(ch_len) > visible_prefix_len {
            break;
        }
        output.extend_from_slice(&line[index..index + ch_len]);
        visible_copied += ch_len;
        index += ch_len;
    }
    if hyperlink_active {
        output.extend_from_slice(OSC8_CLOSE);
    }
    RenderedPaneRow::new(output, u16::try_from(columns).unwrap_or(u16::MAX))
}

pub(crate) fn truncate_rendered_pane_line(line: &[u8], width: usize, utf8: &Utf8Config) -> Vec<u8> {
    if width == 0 {
        return Vec::new();
    }
    truncate_rendered_pane_row(line, width, utf8).bytes
}

pub(super) fn crop_rendered_pane_row(
    line: &[u8],
    start: usize,
    width: usize,
    utf8: &Utf8Config,
) -> RenderedPaneRow {
    if width == 0 {
        return RenderedPaneRow::new(Vec::new(), 0);
    }
    let Ok(line_text) = std::str::from_utf8(line) else {
        return RenderedPaneRow::new(Vec::new(), 0);
    };
    // Strip only control sequences for width calculation. The core truncators
    // retain whole tmux cells, including ZWJ, combining and configured widths.
    let mut visible = String::with_capacity(line.len());
    let mut index = 0;
    while index < line.len() {
        if let Some(end) = rendered_control_end(line, index) {
            index = end;
        } else {
            let ch = line_text[index..]
                .chars()
                .next()
                .expect("character boundary");
            visible.push(ch);
            index += ch.len_utf8();
        }
    }
    let total = tmux_text_width(&visible, utf8);
    if total == 0 {
        return truncate_rendered_pane_row(line, 0, utf8);
    }
    let start = start.min(total);
    let end = start.saturating_add(width).min(total);
    let before = tmux_truncate_to_width(&visible, start, utf8);
    let suffix = rmux_core::truncate_right_to_width(&visible, total - start, utf8);
    let first_byte = visible.len() - suffix.len();
    let first_col = total - tmux_text_width(&suffix, utf8);
    let prefix = tmux_truncate_to_width(&visible, end, utf8);
    let last_byte = prefix.len();
    let last_col = tmux_text_width(&prefix, utf8);
    let left_spaces = first_col.min(end) - start;
    let right_spaces = end - last_col.max(first_col.min(end));
    let mut output = Vec::with_capacity(line.len());
    let mut hyperlink_active = false;
    let mut visible_byte = 0;
    index = 0;
    while index < line.len() {
        if let Some(next) = rendered_control_end(line, index) {
            let sequence = &line[index..next];
            update_osc8_state(sequence, &mut hyperlink_active);
            output.extend_from_slice(sequence);
            index = next;
            continue;
        }
        if visible_byte == before.len() {
            output.extend(std::iter::repeat_n(b' ', left_spaces));
        }
        if visible_byte == last_byte {
            output.extend(std::iter::repeat_n(b' ', right_spaces));
            break;
        }
        let ch = line_text[index..]
            .chars()
            .next()
            .expect("character boundary");
        let len = ch.len_utf8();
        if visible_byte >= first_byte && visible_byte < last_byte {
            output.extend_from_slice(&line[index..index + len]);
        }
        visible_byte += len;
        index += len;
    }
    let emitted_cols = end - start;

    if hyperlink_active {
        output.extend_from_slice(OSC8_CLOSE);
    }
    RenderedPaneRow::new(output, u16::try_from(emitted_cols).unwrap_or(u16::MAX))
}

pub(super) fn append_pane_row_tail(
    frame: &mut Vec<u8>,
    terminal_row: u16,
    pane_x: u16,
    pane_cols: u16,
    terminal_cols: u16,
    painted_cols: u16,
) {
    let painted_cols = painted_cols.min(pane_cols);
    let remaining = pane_cols.saturating_sub(painted_cols);
    if remaining == 0 {
        return;
    }

    if remaining < ECH_MIN_COLUMNS {
        frame.extend(std::iter::repeat_n(b' ', usize::from(remaining)));
        return;
    }

    let reaches_terminal_eol = pane_x.saturating_add(pane_cols) >= terminal_cols;
    if painted_cols == 0 {
        if reaches_terminal_eol {
            frame.extend_from_slice(b"\x1b[K");
        } else if pane_x == 0 {
            frame.extend_from_slice(
                cursor_position_bytes(
                    terminal_row,
                    pane_x.saturating_add(pane_cols.saturating_sub(1)),
                )
                .as_slice(),
            );
            frame.extend_from_slice(b"\x1b[1K");
        } else {
            append_ech(frame, remaining);
        }
        return;
    }

    if reaches_terminal_eol {
        frame.extend_from_slice(b"\x1b[K");
    } else {
        append_ech(frame, remaining);
    }
}

fn append_ech(frame: &mut Vec<u8>, columns: u16) {
    write!(frame, "\x1b[{columns}X").expect("writing to Vec cannot fail");
}

fn truncate_rendered_ascii_pane_row(
    line: &[u8],
    width: usize,
    utf8: &Utf8Config,
) -> RenderedPaneRow {
    let mut output = Vec::with_capacity(line.len().min(width.saturating_mul(4)));
    let mut used = 0_usize;
    let mut index = 0_usize;
    let mut hyperlink_active = false;
    while index < line.len() {
        if let Some(end) = rendered_control_end(line, index) {
            let sequence = &line[index..end];
            update_osc8_state(sequence, &mut hyperlink_active);
            output.extend_from_slice(sequence);
            index = end;
            continue;
        }

        let mut buf = [0_u8; 4];
        let text = char::from(line[index]).encode_utf8(&mut buf);
        let cell_width = tmux_text_width(text, utf8);
        if cell_width != 0 && used.saturating_add(cell_width) > width {
            break;
        }
        output.push(line[index]);
        used = used.saturating_add(cell_width);
        index += 1;
    }
    if hyperlink_active {
        output.extend_from_slice(OSC8_CLOSE);
    }
    RenderedPaneRow::new(output, u16::try_from(used).unwrap_or(u16::MAX))
}

fn update_osc8_state(sequence: &[u8], active: &mut bool) {
    if !sequence.starts_with(b"\x1b]8;") {
        return;
    }
    let body_end = if sequence.ends_with(b"\x07") {
        sequence.len().saturating_sub(1)
    } else if sequence.ends_with(b"\x1b\\") {
        sequence.len().saturating_sub(2)
    } else {
        return;
    };
    let Some(separator) = sequence[4..body_end]
        .iter()
        .position(|byte| *byte == b';')
        .map(|offset| offset + 4)
    else {
        return;
    };
    *active = separator.saturating_add(1) < body_end;
}

/// Grid rendering emits SO/SI for charset transitions alongside ANSI sequences.
/// Keep these bytes in the output, but exclude them from cell width and folding.
fn rendered_control_end(line: &[u8], start: usize) -> Option<usize> {
    match line[start] {
        0x0e | 0x0f => Some(start + 1),
        0x1b => Some(ansi_sequence_end(line, start)),
        _ => None,
    }
}

fn ansi_sequence_end(line: &[u8], start: usize) -> usize {
    let Some(&kind) = line.get(start.saturating_add(1)) else {
        return line.len();
    };
    match kind {
        b'[' => line[start + 2..]
            .iter()
            .position(|byte| (0x40..=0x7e).contains(byte))
            .map_or(line.len(), |offset| start + 3 + offset),
        b']' => osc_sequence_end(line, start),
        _ => start.saturating_add(2).min(line.len()),
    }
}

fn osc_sequence_end(line: &[u8], start: usize) -> usize {
    let mut index = start.saturating_add(2);
    while index < line.len() {
        match line[index] {
            0x07 => return index + 1,
            0x1b if line.get(index + 1) == Some(&b'\\') => return index + 2,
            _ => index += 1,
        }
    }
    line.len()
}

#[cfg(test)]
mod charset_width_tests {
    use super::*;

    #[test]
    fn shifts_do_not_consume_columns_in_ascii_or_utf8_rows() {
        for (line, width, expected) in [
            ("\x0eAB\x0fCD!", 4, "\x0eAB\x0fCD"),
            ("\x0eéB\x0fCD!", 4, "\x0eéB\x0fCD"),
            ("\x0e界\x0fAB!", 4, "\x0e界\x0fAB"),
            ("\x0eA\x0f\u{301}BCD!", 4, "\x0eA\x0f\u{301}BCD"),
            ("ABCD\x0f!", 4, "ABCD\x0f"),
        ] {
            let row = truncate_rendered_pane_row(line.as_bytes(), width, &Utf8Config::default());
            assert_eq!(row.bytes(), expected.as_bytes(), "{line:?}");
            assert_eq!(usize::from(row.columns()), width, "{line:?}");
        }
    }

    #[test]
    fn shifts_preserve_sgr_and_close_truncated_hyperlinks() {
        for text in ["ABCD", "éBCD"] {
            let line = format!("\x1b]8;;https://example.org\x1b\\\x0e\x1b[31m{text}!");
            let expected =
                format!("\x1b]8;;https://example.org\x1b\\\x0e\x1b[31m{text}\x1b]8;;\x1b\\");
            let row = truncate_rendered_pane_row(line.as_bytes(), 4, &Utf8Config::default());
            assert_eq!(row.bytes(), expected.as_bytes());
            assert_eq!(row.columns(), 4);
        }
    }

    #[test]
    fn projected_rows_crop_cells_without_counting_shifts() {
        for (line, expected) in [
            ("\x0eAB\x0fCD", "\x0eB\x0fC"),
            ("\x0eéB\x0fCD", "\x0eB\x0fC"),
            ("\x0e界\x0fAB", "\x0e \x0fA"),
        ] {
            let row = crop_rendered_pane_row(line.as_bytes(), 1, 2, &Utf8Config::default());
            assert_eq!(row.bytes(), expected.as_bytes(), "{line:?}");
            assert_eq!(row.columns(), 2);
        }
    }
}
