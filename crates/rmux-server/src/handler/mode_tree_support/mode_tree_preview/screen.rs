use rmux_core::{
    input::{mode, GridAttr},
    style_tostring, GridRenderOptions, ScreenCaptureRange, Style, StyleCell, Utf8Config,
};
use rmux_proto::PaneTarget;

use crate::pane_terminals::{HandlerState, PaneCaptureRequest};

use super::super::mode_tree_render::{pad_visible_width, sanitize_overlay_text};

#[cfg(test)]
mod transcript_policy_tests;

pub(in crate::handler::mode_tree_support) fn preview_lines_for_target(
    state: &HandlerState,
    target: &PaneTarget,
    limit: usize,
    width: usize,
    utf8: &Utf8Config,
) -> Vec<String> {
    if limit == 0 || width == 0 {
        return Vec::new();
    }

    let mut lines = state
        .transcript_handle(target)
        .ok()
        .and_then(|transcript| transcript.preview_screen())
        .map(|screen| preview_lines_for_screen(&screen, limit, width, utf8))
        .or_else(|| {
            state
                .capture_transcript(
                    target,
                    PaneCaptureRequest {
                        range: ScreenCaptureRange::default(),
                        options: GridRenderOptions {
                            include_empty_cells: true,
                            trim_spaces: false,
                            ..GridRenderOptions::default()
                        },
                        alternate: false,
                        use_mode_screen: false,
                        pending_input: false,
                        quiet: true,
                        escape_pending: false,
                        hyperlinks: false,
                    },
                )
                .ok()
                .map(|bytes| {
                    String::from_utf8_lossy(&bytes)
                        .lines()
                        .map(|line| pad_visible_width(&sanitize_overlay_text(line), width, utf8))
                        .collect::<Vec<_>>()
                })
        })
        .unwrap_or_default();
    if lines.len() > limit {
        lines.truncate(limit);
    }
    while lines.len() < limit {
        lines.push(" ".repeat(width));
    }
    lines
}

pub(in crate::handler::mode_tree_support) fn preview_lines_for_screen(
    screen: &rmux_core::Screen,
    limit: usize,
    width: usize,
    utf8: &Utf8Config,
) -> Vec<String> {
    if limit == 0 || width == 0 {
        return Vec::new();
    }

    let rows = usize::from(screen.size().rows.max(1));
    let left = preview_horizontal_offset(screen, width);
    let top = preview_vertical_offset(screen, limit);
    let visible = rows.saturating_sub(top).min(limit);
    if visible == 0 {
        return vec![" ".repeat(width); limit];
    }
    let (cursor_x, cursor_y) = screen.cursor_position();
    let cursor = ((screen.mode() & mode::MODE_CURSOR) != 0)
        .then_some((cursor_x as usize, cursor_y as usize));
    let mut lines = (0..visible)
        .filter_map(|offset| screen.absolute_line_view(screen.history_size() + top + offset))
        .enumerate()
        .map(|(offset, line)| {
            preview_line_for_screen_view(&line, left, width, top + offset, cursor, utf8)
        })
        .collect::<Vec<_>>();
    while lines.len() < limit {
        lines.push(" ".repeat(width));
    }
    lines
}

fn preview_line_for_screen_view(
    line: &rmux_core::ScreenLineView,
    start_x: usize,
    width: usize,
    row: usize,
    cursor: Option<(usize, usize)>,
    _utf8: &Utf8Config,
) -> String {
    let mut rendered = String::new();
    let mut remaining = width;
    let mut active_style = None::<Style>;

    for (x, cell) in line.cells().iter().enumerate().skip(start_x) {
        if remaining == 0 {
            break;
        }
        if cell.is_padding() {
            // A crop starting inside a wide glyph keeps its remaining column.
            // Padding for a glyph already emitted has already consumed width.
            if x == start_x {
                rendered.push(' ');
                remaining -= 1;
            }
            continue;
        }

        let cell_width = usize::from(cell.width().max(1));
        if cell_width > remaining {
            break;
        }

        let text = sanitize_preview_text(cell.text());
        let mut style = preview_cell_style(cell);
        if cursor == Some((x, row)) {
            style.cell.attr |= GridAttr::REVERSE;
        }
        if active_style.as_ref() != Some(&style) {
            rendered.push_str("#[");
            rendered.push_str(&style_tostring(&style));
            rendered.push(']');
            active_style = Some(style);
        }
        rendered.push_str(&super::escape_format_draw_text(&text));
        remaining = remaining.saturating_sub(cell_width);
    }

    if active_style.is_some() {
        rendered.push_str("#[default]");
    }
    rendered.push_str(&" ".repeat(remaining));
    rendered
}

fn sanitize_preview_text(value: &str) -> String {
    sanitize_overlay_text(value)
}

fn preview_cell_style(cell: &rmux_core::ScreenCellView) -> Style {
    Style {
        cell: StyleCell {
            fg: cell.fg(),
            bg: cell.bg(),
            us: cell.us(),
            attr: cell.attr(),
        },
        ..Style::default()
    }
}

pub(in crate::handler::mode_tree_support) fn preview_vertical_offset(
    screen: &rmux_core::Screen,
    height: usize,
) -> usize {
    let rows = usize::from(screen.size().rows.max(1));
    if rows <= height || (screen.mode() & mode::MODE_CURSOR) == 0 {
        return 0;
    }

    let (_, cursor_y) = screen.cursor_position();
    let mut top = cursor_y as usize;
    if top < height / 3 {
        top = 0;
    } else {
        top = top.saturating_sub(height / 3);
    }
    if top + height > rows {
        top = rows.saturating_sub(height);
    }
    top
}

pub(in crate::handler::mode_tree_support) fn preview_horizontal_offset(
    screen: &rmux_core::Screen,
    width: usize,
) -> usize {
    let cols = usize::from(screen.size().cols.max(1));
    if cols <= width || (screen.mode() & mode::MODE_CURSOR) == 0 {
        return 0;
    }

    let (cursor_x, _) = screen.cursor_position();
    let mut left = cursor_x as usize;
    if left < width / 3 {
        left = 0;
    } else {
        left = left.saturating_sub(width / 3);
    }
    if left + width > cols {
        left = cols.saturating_sub(width);
    }
    left
}
