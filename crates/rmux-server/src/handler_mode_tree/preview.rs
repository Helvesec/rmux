use rmux_core::{
    parse_colour, style_tostring, text_width as tmux_text_width,
    truncate_to_width as tmux_truncate_to_width, Style, StyleCell, Utf8Config,
};
use rmux_proto::{OptionName, PaneTarget, SessionName};

use crate::pane_indices::visible_pane_index;
use crate::pane_terminals::HandlerState;

use super::mode_tree_model::{ModeTreeAction, ModeTreeClientState, ModeTreeItem};
use super::mode_tree_render::pad_visible_width;
use super::mode_tree_sort::sort_windows;

#[path = "preview/screen.rs"]
mod screen;

pub(super) use self::screen::preview_lines_for_target;
#[cfg(test)]
pub(super) use self::screen::{
    preview_horizontal_offset, preview_lines_for_screen, preview_vertical_offset,
};

const TMUX_TREE_MIN_PREVIEW_WIDTH: usize = 24;

pub(super) fn mode_tree_preview_lines(
    state: &HandlerState,
    mode: &ModeTreeClientState,
    item: &ModeTreeItem,
    width: usize,
    height: usize,
    utf8: &Utf8Config,
) -> Vec<String> {
    if width == 0 || height == 0 {
        return Vec::new();
    }

    match &item.action {
        ModeTreeAction::TreeTarget {
            session_name,
            window_index: Some(window_index),
            pane_index: Some(pane_index),
            ..
        } => render_tree_pane_preview(
            state,
            PaneTarget::with_window(session_name.clone(), *window_index, *pane_index),
            width,
            height,
            utf8,
        ),
        ModeTreeAction::TreeTarget {
            session_name,
            window_index: Some(window_index),
            pane_index: None,
            ..
        } => render_tree_window_preview(
            state,
            mode,
            session_name,
            *window_index,
            width,
            height,
            utf8,
        ),
        ModeTreeAction::TreeTarget {
            session_name,
            window_index: None,
            pane_index: None,
            ..
        } => render_tree_session_preview(state, mode, session_name, width, height, utf8),
        _ => item
            .preview
            .iter()
            .take(height)
            .map(|line| pad_visible_width(line, width, utf8))
            .collect(),
    }
}

fn render_tree_pane_preview(
    state: &HandlerState,
    target: PaneTarget,
    width: usize,
    height: usize,
    utf8: &Utf8Config,
) -> Vec<String> {
    preview_lines_for_target(state, &target, height, width, utf8)
        .into_iter()
        .take(height)
        .collect()
}

fn render_tree_session_preview(
    state: &HandlerState,
    mode: &ModeTreeClientState,
    session_name: &SessionName,
    width: usize,
    height: usize,
    utf8: &Utf8Config,
) -> Vec<String> {
    let Some(session) = state.sessions.session(session_name) else {
        return vec![String::new(); height];
    };
    let mut windows = session.windows().iter().collect::<Vec<_>>();
    sort_windows(&mut windows, mode.sort_order, mode.reversed);
    let current = windows
        .iter()
        .position(|(window_index, _)| *window_index == &session.active_window_index())
        .unwrap_or(0);
    let total_columns = windows.len();
    let Some(layout) = tree_preview_layout(total_columns, current, width) else {
        return vec![" ".repeat(width); height];
    };
    let columns = windows
        .into_iter()
        .enumerate()
        .map(|(column_index, (window_index, window))| PreviewColumn {
            label: tree_session_preview_label(*window_index, window.name().unwrap_or_default()),
            label_area_width: preview_segment_width(&layout, column_index),
            lines: preview_lines_for_target(
                state,
                &PaneTarget::with_window(
                    session_name.clone(),
                    *window_index,
                    window.active_pane_index(),
                ),
                height,
                preview_segment_width(&layout, column_index),
                utf8,
            ),
        })
        .collect::<Vec<_>>();
    let inactive_label_style = preview_label_style(state, session_name, false);
    let active_label_style = preview_label_style(state, session_name, true);
    render_tree_columns_preview(
        columns,
        current,
        width,
        height,
        &inactive_label_style,
        &active_label_style,
        utf8,
    )
}

fn render_tree_window_preview(
    state: &HandlerState,
    _mode: &ModeTreeClientState,
    session_name: &SessionName,
    window_index: u32,
    width: usize,
    height: usize,
    utf8: &Utf8Config,
) -> Vec<String> {
    let Some(session) = state.sessions.session(session_name) else {
        return vec![String::new(); height];
    };
    let Some(window) = session.window_at(window_index) else {
        return vec![String::new(); height];
    };
    let mut panes = window.panes().iter().collect::<Vec<_>>();
    panes.sort_by_key(|pane| pane.index());
    let current = panes
        .iter()
        .position(|pane| pane.index() == window.active_pane_index())
        .unwrap_or(0);
    let total_columns = panes.len();
    let Some(layout) = tree_preview_layout(total_columns, current, width) else {
        return vec![" ".repeat(width); height];
    };
    let columns = panes
        .into_iter()
        .enumerate()
        .map(|(column_index, pane)| PreviewColumn {
            label: tree_window_preview_label(visible_pane_index(
                session,
                &state.options,
                window_index,
                pane.index(),
            )),
            label_area_width: layout.each,
            lines: preview_lines_for_target(
                state,
                &PaneTarget::with_window(session_name.clone(), window_index, pane.index()),
                height,
                preview_segment_width(&layout, column_index),
                utf8,
            ),
        })
        .collect::<Vec<_>>();
    let inactive_label_style = preview_label_style(state, session_name, false);
    let active_label_style = preview_label_style(state, session_name, true);
    render_tree_columns_preview(
        columns,
        current,
        width,
        height,
        &inactive_label_style,
        &active_label_style,
        utf8,
    )
}

pub(super) fn render_tree_columns_preview(
    columns: Vec<PreviewColumn>,
    current: usize,
    width: usize,
    height: usize,
    inactive_label_style: &Style,
    active_label_style: &Style,
    utf8: &Utf8Config,
) -> Vec<String> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    if columns.is_empty() {
        return vec![String::new(); height];
    }

    let Some(layout) = tree_preview_layout(columns.len(), current, width) else {
        return vec![String::new(); height];
    };
    let mut lines = Vec::with_capacity(height);
    for row in 0..height {
        let mut line = String::new();
        if layout.left {
            line.push(if row == height / 2 { '<' } else { ' ' });
            line.push(' ');
            line.push('│');
        }

        for (column_index, column) in columns
            .iter()
            .enumerate()
            .take(layout.end)
            .skip(layout.start)
        {
            let segment_width = if column_index == layout.end.saturating_sub(1) {
                layout.each + layout.remaining
            } else {
                layout.each.saturating_sub(1)
            };
            if segment_width == 0 {
                continue;
            }
            let label_style = if column_index == current {
                active_label_style
            } else {
                inactive_label_style
            };
            line.push_str(&render_preview_segment_row(
                column,
                row,
                segment_width,
                column.label_area_width,
                height,
                label_style,
                utf8,
            ));
            if column_index != layout.end.saturating_sub(1) {
                line.push('│');
            }
        }

        if layout.right {
            line.push('│');
            line.push(' ');
            line.push(if row == height / 2 { '>' } else { ' ' });
        }
        lines.push(line);
    }
    lines
}

fn tree_session_preview_label(window_index: u32, window_name: &str) -> String {
    format!(" {window_index}:{window_name} ")
}

pub(super) fn tree_window_preview_label(pane_index: u32) -> String {
    format!(" {pane_index} ")
}

pub(super) fn render_preview_segment_row(
    column: &PreviewColumn,
    row: usize,
    width: usize,
    label_area_width: usize,
    height: usize,
    label_style: &Style,
    utf8: &Utf8Config,
) -> String {
    if width == 0 {
        return String::new();
    }
    let label = preview_label_text(&column.label, width, utf8);
    let label_width = tmux_text_width(&label, utf8);
    let label_x = label_area_width.saturating_sub(label_width).div_ceil(2);
    let label_y = height.div_ceil(2);
    let box_top = label_y.saturating_sub(1);
    let box_bottom = box_top.saturating_add(2);

    if height >= 3
        && label_x > 1
        && label_x + label_width < label_area_width.saturating_sub(1)
        && (box_top..=box_bottom).contains(&row)
    {
        return render_preview_label_box_row(
            row,
            width,
            label_width,
            label_x,
            box_top,
            label_y,
            label_style,
            &label,
        );
    }

    if row == label_y {
        return render_preview_centered_label_row(width, label_x, label_style, &label);
    }

    match column.lines.get(row) {
        Some(line) if !line.is_empty() => line.clone(),
        _ => " ".repeat(width),
    }
}

fn preview_segment_width(layout: &TreePreviewLayout, column_index: usize) -> usize {
    if column_index == layout.end.saturating_sub(1) {
        layout.each + layout.remaining
    } else {
        layout.each.saturating_sub(1)
    }
}

fn preview_label_text(label: &str, width: usize, utf8: &Utf8Config) -> String {
    if tmux_text_width(label, utf8) <= width {
        return label.to_owned();
    }
    let fallback = label
        .split_once(':')
        .map(|(head, _)| head.to_owned())
        .unwrap_or_else(|| label.to_owned());
    if tmux_text_width(&fallback, utf8) <= width {
        fallback
    } else {
        tmux_truncate_to_width(label, width, utf8)
    }
}

#[allow(clippy::too_many_arguments)]
fn render_preview_label_box_row(
    row: usize,
    width: usize,
    label_width: usize,
    label_x: usize,
    box_top: usize,
    label_y: usize,
    label_style: &Style,
    label: &str,
) -> String {
    let mut line = String::new();
    let box_left = label_x.saturating_sub(1);
    let box_right = box_left + label_width + 1;
    for x in 0..width {
        let ch = if row == box_top || row == box_top + 2 {
            match x {
                x if x == box_left => {
                    if row == box_top {
                        '┌'
                    } else {
                        '└'
                    }
                }
                x if x == box_right => {
                    if row == box_top {
                        '┐'
                    } else {
                        '┘'
                    }
                }
                x if x > box_left && x < box_right => '─',
                _ => ' ',
            }
        } else if row == label_y {
            match x {
                x if x == box_left || x == box_right => '│',
                _ => ' ',
            }
        } else {
            ' '
        };
        line.push(ch);
    }
    if row == label_y {
        overlay_coloured_text(&line, label_x, label_style, label)
    } else {
        line
    }
}

fn render_preview_centered_label_row(
    width: usize,
    label_x: usize,
    label_style: &Style,
    label: &str,
) -> String {
    let line = " ".repeat(width);
    overlay_coloured_text(&line, label_x, label_style, label)
}

fn overlay_coloured_text(base: &str, x: usize, style: &Style, text: &str) -> String {
    let prefix = base.chars().take(x).collect::<String>();
    let suffix = base
        .chars()
        .skip(x.saturating_add(text.chars().count()))
        .collect::<String>();
    format!(
        "{prefix}#[{}]{}#[default]{suffix}",
        style_tostring(style),
        escape_format_draw_text(text)
    )
}

fn escape_format_draw_text(value: &str) -> String {
    value.replace("#[", "##[")
}

fn preview_label_style(state: &HandlerState, session_name: &SessionName, active: bool) -> Style {
    let option = if active {
        OptionName::DisplayPanesActiveColour
    } else {
        OptionName::DisplayPanesColour
    };
    let colour = state
        .options
        .resolve(Some(session_name), option)
        .and_then(|value| parse_colour(value).ok())
        .unwrap_or(rmux_core::COLOUR_DEFAULT);
    Style {
        cell: StyleCell {
            fg: colour,
            ..StyleCell::default()
        },
        ..Style::default()
    }
}

pub(super) fn tree_preview_layout(
    total: usize,
    current: usize,
    width: usize,
) -> Option<TreePreviewLayout> {
    if total == 0 || width == 0 {
        return None;
    }
    let mut visible = if width / total < TMUX_TREE_MIN_PREVIEW_WIDTH {
        (width / TMUX_TREE_MIN_PREVIEW_WIDTH).max(1)
    } else {
        total
    };
    visible = visible.min(total);

    let current = current.min(total.saturating_sub(1));
    let (mut start, mut end) = if current < visible {
        (0, visible)
    } else if current >= total.saturating_sub(visible) {
        (total.saturating_sub(visible), total)
    } else {
        let start = current.saturating_sub(visible / 2);
        (start, start + visible)
    };
    let mut left = start != 0;
    let mut right = end != total;
    if ((left && right) && width <= 6) || ((left || right) && width <= 3) {
        left = false;
        right = false;
        start = 0;
        end = visible.min(total);
    }

    let reserved = if left && right {
        6
    } else if left || right {
        3
    } else {
        0
    };
    let each = width.saturating_sub(reserved) / visible.max(1);
    if each == 0 {
        return None;
    }
    let remaining = width.saturating_sub(reserved) - (visible * each);
    Some(TreePreviewLayout {
        start,
        end,
        left,
        right,
        each,
        remaining,
    })
}

pub(super) struct PreviewColumn {
    pub(super) label: String,
    pub(super) label_area_width: usize,
    pub(super) lines: Vec<String>,
}

pub(super) struct TreePreviewLayout {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) left: bool,
    pub(super) right: bool,
    pub(super) each: usize,
    pub(super) remaining: usize,
}
