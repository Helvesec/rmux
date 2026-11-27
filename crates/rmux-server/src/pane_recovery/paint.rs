//! Byte emission for the recovery paint: title, scroll region, tab stops,
//! cursor state and the rendered rows themselves.

use std::collections::VecDeque;

use rmux_core::input::mode;
use rmux_core::Screen;

use super::rows::RenderedRow;
use super::{LINE_BREAK, RESET_RENDITION, ROW_RESET};

pub(super) fn append_title_state(out: &mut Vec<u8>, screen: &Screen) -> bool {
    let mut complete = true;
    for _ in 0..Screen::title_stack_limit() {
        out.extend_from_slice(b"\x1b[23;2t");
    }
    for title in screen.title_stack() {
        complete &= append_osc_text(out, 2, title);
        out.extend_from_slice(b"\x1b[22;2t");
    }
    complete &= append_osc_text(out, 2, screen.title());
    complete
}

/// Appends a safely replayable OSC value and reports whether it was lossless.
///
/// Unicode control characters are omitted instead of being embedded in the
/// recovery envelope, so callers must include the result in metadata coverage.
pub(super) fn append_osc_text(out: &mut Vec<u8>, command: u8, value: &str) -> bool {
    let mut complete = true;
    out.extend_from_slice(b"\x1b]");
    out.extend_from_slice(command.to_string().as_bytes());
    out.push(b';');
    for character in value.chars() {
        if character.is_control() {
            complete = false;
            continue;
        }
        let mut encoded = [0_u8; 4];
        out.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
    }
    out.extend_from_slice(b"\x1b\\");
    complete
}

pub(super) fn append_scroll_region(out: &mut Vec<u8>, screen: &Screen) {
    let (top, bottom) = screen.scroll_region();
    let default_bottom = u32::from(screen.size().rows.max(1)).saturating_sub(1);
    if top != 0 || bottom != default_bottom {
        out.extend_from_slice(
            format!(
                "\x1b[{};{}r",
                top.saturating_add(1),
                bottom.saturating_add(1)
            )
            .as_bytes(),
        );
    }
}

pub(super) fn append_tab_stops(out: &mut Vec<u8>, screen: &Screen) {
    out.extend_from_slice(b"\x1b[3g");
    for (column, enabled) in screen.tab_stops().iter().copied().enumerate() {
        if enabled {
            append_cup(out, column.saturating_add(1) as u32, 1);
            out.extend_from_slice(b"\x1bH");
        }
    }
}

pub(super) fn append_saved_decsc(
    out: &mut Vec<u8>,
    screen: &Screen,
    saved_cursor: (u32, u32, bool),
    saved_cell_state: &[u8],
) {
    let (x, y, origin) = saved_cursor;
    out.extend_from_slice(b"\x1b[?6l");
    out.extend_from_slice(RESET_RENDITION);
    out.extend_from_slice(saved_cell_state);
    if origin {
        out.extend_from_slice(b"\x1b[?6h");
        let row = y.saturating_sub(screen.scroll_region().0).saturating_add(1);
        append_cup(out, x.saturating_add(1), row);
    } else {
        append_cup(out, x.saturating_add(1), y.saturating_add(1));
    }
    out.extend_from_slice(b"\x1b7");
    out.extend_from_slice(b"\x1b[?6l");
}

pub(super) fn append_active_cursor_state(
    out: &mut Vec<u8>,
    screen: &Screen,
    rows: &[RenderedRow],
    active_cell_state: &[u8],
) {
    let (x, y) = screen.cursor_position();
    if screen.mode() & mode::MODE_ORIGIN != 0 {
        out.extend_from_slice(b"\x1b[?6h");
    } else {
        out.extend_from_slice(b"\x1b[?6l");
    }
    out.extend_from_slice(RESET_RENDITION);
    append_cursor_state(
        out,
        screen,
        x,
        y,
        screen.pending_wrap(),
        Some(rows),
        (screen.mode() & mode::MODE_ORIGIN != 0).then(|| screen.scroll_region().0),
    );
    out.extend_from_slice(RESET_RENDITION);
    out.extend_from_slice(active_cell_state);
    out.extend_from_slice(if screen.mode() & mode::MODE_CURSOR != 0 {
        b"\x1b[?25h"
    } else {
        b"\x1b[?25l"
    });
}

pub(super) fn append_cursor_state(
    out: &mut Vec<u8>,
    screen: &Screen,
    x: u32,
    y: u32,
    pending_wrap: bool,
    lines: Option<&[RenderedRow]>,
    origin_top: Option<u32>,
) {
    let cursor_row = y.saturating_sub(origin_top.unwrap_or(0)).saturating_add(1);
    if pending_wrap {
        if let Some(line) = lines.and_then(|lines| lines.get(y as usize)) {
            append_cup(out, 1, cursor_row);
            out.extend_from_slice(RESET_RENDITION);
            out.extend_from_slice(&line.bytes);
            return;
        }
    }
    let size = screen.size();
    append_cup(
        out,
        x.min(u32::from(size.cols.saturating_sub(1)))
            .saturating_add(1),
        cursor_row,
    );
}

fn append_cup(out: &mut Vec<u8>, col: u32, row: u32) {
    out.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
}

/// Separator emitted between two rows that the terminal will not wrap itself.
///
/// Rows are linked by [`link_rows`] before they are appended, so a wrapped row
/// is always followed by a row the terminal wraps onto: only an unwrapped row
/// needs this separator.
fn row_separator(previous: Option<&RenderedRow>) -> &'static [u8] {
    match previous {
        Some(previous) if previous.wrapped => b"",
        Some(_) => LINE_BREAK,
        None => b"",
    }
}

pub(super) fn append_rows(out: &mut Vec<u8>, rows: &[RenderedRow]) {
    for (index, row) in rows.iter().enumerate() {
        out.extend_from_slice(row_separator(
            index.checked_sub(1).map(|previous| &rows[previous]),
        ));
        out.extend_from_slice(ROW_RESET);
        out.extend_from_slice(&row.bytes);
    }
}

pub(super) fn append_rows_deque(out: &mut Vec<u8>, rows: &VecDeque<RenderedRow>) {
    let mut previous = None;
    for row in rows {
        out.extend_from_slice(row_separator(previous));
        out.extend_from_slice(ROW_RESET);
        out.extend_from_slice(&row.bytes);
        previous = Some(row);
    }
}

pub(super) fn encoded_rows_len(rows: &[RenderedRow]) -> usize {
    rows.iter()
        .enumerate()
        .fold(0_usize, |length, (index, row)| {
            length
                .saturating_add(
                    row_separator(index.checked_sub(1).map(|previous| &rows[previous])).len(),
                )
                .saturating_add(ROW_RESET.len())
                .saturating_add(row.bytes.len())
        })
}

pub(super) fn encoded_rows_len_deque(rows: &VecDeque<RenderedRow>) -> usize {
    let mut previous = None;
    rows.iter().fold(0_usize, |length, row| {
        let separator = row_separator(previous).len();
        previous = Some(row);
        length
            .saturating_add(separator)
            .saturating_add(ROW_RESET.len())
            .saturating_add(row.bytes.len())
    })
}
