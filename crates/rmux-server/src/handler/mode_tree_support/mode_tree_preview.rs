use rmux_core::Utf8Config;
use rmux_proto::{PaneTarget, SessionName};

use crate::pane_terminals::HandlerState;

use super::mode_tree_model::{ModeTreeAction, ModeTreeClientState, ModeTreeItem};
use super::mode_tree_render::pad_visible_width;
use super::mode_tree_sort::sort_windows;

mod screen;

mod label;
use label::resolve_label;
pub(super) use label::{render_preview_segment_row, PreviewLabel};

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
    let Some(session) = state.sessions.session(target.session_name()) else {
        return vec![String::new(); height];
    };
    let Some(window) = session.window_at(target.window_index()) else {
        return vec![String::new(); height];
    };
    let Some(pane) = window.pane(target.pane_index()) else {
        return vec![String::new(); height];
    };
    render_tree_columns_preview(
        vec![PreviewColumn {
            label: resolve_label(
                state,
                session,
                target.window_index(),
                window,
                Some(pane),
                false,
            ),
            lines: preview_lines_for_target(state, &target, height, width, utf8),
        }],
        0,
        width,
        height,
        utf8,
    )
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
    let mut windows = session.windows().collect::<Vec<_>>();
    sort_windows(&mut windows, mode.sort_order, mode.reversed);
    let current = windows
        .iter()
        .position(|(window_index, _)| *window_index == session.active_window_index())
        .unwrap_or(0);
    let total_columns = windows.len();
    let Some(layout) = tree_preview_layout(total_columns, current, width) else {
        return vec![" ".repeat(width); height];
    };
    let columns = windows
        .into_iter()
        .enumerate()
        .map(|(column_index, (window_index, window))| PreviewColumn {
            label: resolve_label(
                state,
                session,
                window_index,
                window,
                window.active_pane(),
                true,
            ),
            lines: preview_lines_for_target(
                state,
                &PaneTarget::with_window(
                    session_name.clone(),
                    window_index,
                    window.active_pane_index(),
                ),
                height,
                preview_segment_width(&layout, column_index),
                utf8,
            ),
        })
        .collect::<Vec<_>>();
    render_tree_columns_preview(columns, current, width, height, utf8)
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
            label: resolve_label(state, session, window_index, window, Some(pane), false),
            lines: preview_lines_for_target(
                state,
                &PaneTarget::with_window(session_name.clone(), window_index, pane.index()),
                height,
                preview_segment_width(&layout, column_index),
                utf8,
            ),
        })
        .collect::<Vec<_>>();
    render_tree_columns_preview(columns, current, width, height, utf8)
}

pub(super) fn render_tree_columns_preview(
    columns: Vec<PreviewColumn>,
    current: usize,
    width: usize,
    height: usize,
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
            line.push_str(&render_preview_segment_row(
                column,
                row,
                segment_width,
                height,
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

fn preview_segment_width(layout: &TreePreviewLayout, column_index: usize) -> usize {
    if column_index == layout.end.saturating_sub(1) {
        layout.each + layout.remaining
    } else {
        layout.each.saturating_sub(1)
    }
}

fn escape_format_draw_text(value: &str) -> String {
    value.replace("#[", "##[")
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
    pub(super) label: PreviewLabel,
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
