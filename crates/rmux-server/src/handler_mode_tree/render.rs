use rmux_core::{
    formats::FormatContext, text_width as tmux_text_width,
    truncate_to_width as tmux_truncate_to_width, Style, Utf8Config,
};
use rmux_proto::OptionName;

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;

use super::mode_tree_model::TreeDepth;
use super::mode_tree_model::{ModeTreeBuild, ModeTreeClientState, ModeTreeItem, PreviewMode};
use super::mode_tree_sort::sort_order_name;

pub(super) fn render_mode_tree_overlay(
    state: &HandlerState,
    mode: &ModeTreeClientState,
    build: &ModeTreeBuild,
) -> Vec<u8> {
    let Some(session) = state.sessions.session(&mode.session_name) else {
        return Vec::new();
    };
    let size = session.window().size();
    let geometry = crate::renderer::StatusGeometry::for_session(session, &state.options);
    let usable_rows = geometry.content_rows();
    let content_y_offset = geometry.content_y_offset();
    if usable_rows == 0 || size.cols == 0 {
        return Vec::new();
    }

    let list_rows = mode_tree_list_rows(usable_rows, build.visible.len(), mode.preview_mode);
    let utf8 = Utf8Config::from_options(&state.options);
    let default_style = Style::default();
    let selected_style = Style::parse(
        state
            .options
            .resolve_for_window(
                session.name(),
                session.active_window_index(),
                OptionName::ModeStyle,
            )
            .unwrap_or("default"),
    )
    .unwrap_or_default();
    let mut frame = Vec::new();
    frame.extend_from_slice(b"\x1b[s\x1b[?25l");
    let key_width = mode_tree_key_width(state, mode, build, &utf8);

    let mut row = 0_u16;
    for index in 0..usize::from(list_rows) {
        let text = build
            .visible
            .get(mode.scroll + index)
            .and_then(|id| build.items.get(id))
            .map(|item| {
                render_visible_item(
                    state,
                    mode,
                    build,
                    item,
                    mode.scroll + index,
                    key_width,
                    &utf8,
                )
            })
            .unwrap_or_default();
        let selected = build
            .visible
            .get(mode.scroll + index)
            .is_some_and(|id| mode.selected_id.as_ref() == Some(id));
        render_overlay_line(
            &mut frame,
            content_y_offset.saturating_add(row),
            size.cols,
            &text,
            if selected {
                &selected_style
            } else {
                &default_style
            },
            &utf8,
        );
        row = row.saturating_add(1);
    }

    while row < list_rows {
        render_overlay_line(
            &mut frame,
            content_y_offset.saturating_add(row),
            size.cols,
            "",
            &default_style,
            &utf8,
        );
        row = row.saturating_add(1);
    }

    let preview_height = usable_rows.saturating_sub(list_rows);
    if mode.preview_mode != PreviewMode::Off && preview_height >= 2 {
        let title = mode_tree_preview_title(mode, build);
        render_mode_tree_box(
            &mut frame,
            content_y_offset.saturating_add(list_rows),
            preview_height,
            size.cols,
            &title,
            &default_style,
            &utf8,
        );
        let preview_lines = mode
            .selected_id
            .as_ref()
            .and_then(|id| build.items.get(id))
            .map(|item| {
                super::mode_tree_preview_lines(
                    state,
                    mode,
                    item,
                    usize::from(size.cols.saturating_sub(4)),
                    usize::from(preview_height.saturating_sub(2)),
                    &utf8,
                )
            })
            .unwrap_or_default();
        for index in 0..usize::from(preview_height.saturating_sub(2)) {
            let line = preview_lines
                .get(mode.preview_scroll + index)
                .cloned()
                .unwrap_or_default();
            render_overlay_formatted_line(
                &mut frame,
                2,
                content_y_offset
                    .saturating_add(list_rows)
                    .saturating_add(1)
                    .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                size.cols.saturating_sub(4),
                &line,
                &default_style,
                &utf8,
            );
        }
    } else {
        while row < usable_rows {
            render_overlay_line(
                &mut frame,
                content_y_offset.saturating_add(row),
                size.cols,
                "",
                &default_style,
                &utf8,
            );
            row = row.saturating_add(1);
        }
    }

    frame.extend_from_slice(b"\x1b[0m\x1b[u");
    frame
}

pub(super) fn render_visible_item(
    state: &HandlerState,
    mode: &ModeTreeClientState,
    build: &ModeTreeBuild,
    item: &ModeTreeItem,
    line_index: usize,
    key_width: usize,
    utf8: &Utf8Config,
) -> String {
    let shortcut = render_runtime_template(
        &mode.key_format,
        &RuntimeFormatContext::new(FormatContext::new())
            .with_state(state)
            .with_named_value("line", line_index.to_string()),
        false,
    );
    let mut text = String::new();
    if !shortcut.is_empty() {
        let display = format!("({shortcut})");
        text.push_str(&pad_visible_width(&display, key_width, utf8));
    }

    if item.depth > 0 {
        text.push_str(&mode_tree_tree_prefix(build, item));
    }

    text.push_str(mode_tree_branch_marker(mode, build, item));
    text.push_str(&item.line);
    sanitize_overlay_text(&tmux_truncate_to_width(&text, usize::MAX, utf8))
}

fn mode_tree_is_flat_sibling_list(build: &ModeTreeBuild, item: &ModeTreeItem) -> bool {
    let siblings = match item
        .parent
        .as_ref()
        .and_then(|parent| build.items.get(parent))
    {
        Some(parent) => &parent.children,
        None => &build.roots,
    };
    siblings.iter().all(|sibling_id| {
        build
            .items
            .get(sibling_id)
            .map(|sibling| sibling.children.is_empty())
            .unwrap_or(true)
    })
}

fn mode_tree_branch_marker(
    mode: &ModeTreeClientState,
    build: &ModeTreeBuild,
    item: &ModeTreeItem,
) -> &'static str {
    if mode_tree_is_flat_sibling_list(build, item) {
        return "";
    }
    if item.children.is_empty() {
        return "  ";
    }

    let leaf_depth = match mode.tree_depth {
        TreeDepth::Session => 0,
        TreeDepth::Window => 1,
        TreeDepth::Pane => 2,
    };
    if item.depth == leaf_depth && item.children.len() <= 1 {
        return "  ";
    }

    if mode.expanded.contains(&item.id) {
        "- "
    } else {
        "+ "
    }
}

fn render_overlay_line(
    frame: &mut Vec<u8>,
    row: u16,
    cols: u16,
    text: &str,
    base_style: &Style,
    utf8: &Utf8Config,
) {
    let width = usize::from(cols);
    let text = sanitize_overlay_text(&tmux_truncate_to_width(text, width, utf8));
    if !text.contains("#[") {
        render_plain_overlay_line(frame, row, &text, base_style);
        return;
    }
    let line = crate::renderer::format_draw_line(&text, base_style, width, utf8);
    crate::renderer::render_formatted_line(frame, 0, row, &line);
}

fn render_plain_overlay_line(frame: &mut Vec<u8>, row: u16, text: &str, style: &Style) {
    frame.extend_from_slice(format!("\x1b[{};1H", row.saturating_add(1)).as_bytes());
    frame.extend_from_slice(crate::renderer::style_sgr_bytes(style, true).as_slice());
    frame.extend_from_slice(text.as_bytes());
    frame.extend_from_slice(b"\x1b[K");
}

pub(super) fn render_overlay_formatted_line(
    frame: &mut Vec<u8>,
    x: u16,
    row: u16,
    cols: u16,
    text: &str,
    base_style: &Style,
    utf8: &Utf8Config,
) {
    let line = crate::renderer::format_draw_line(text, base_style, usize::from(cols), utf8);
    crate::renderer::render_formatted_line(frame, x, row, &line);
}

pub(super) fn sanitize_overlay_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_control() && ch != '\t' {
                ' '
            } else {
                ch
            }
        })
        .collect()
}

fn mode_tree_key_width(
    state: &HandlerState,
    mode: &ModeTreeClientState,
    build: &ModeTreeBuild,
    utf8: &Utf8Config,
) -> usize {
    build
        .visible
        .iter()
        .enumerate()
        .filter_map(|(index, _)| {
            let rendered = render_runtime_template(
                &mode.key_format,
                &RuntimeFormatContext::new(FormatContext::new())
                    .with_state(state)
                    .with_named_value("line", index.to_string()),
                false,
            );
            (!rendered.is_empty())
                .then(|| tmux_text_width(&format!("({rendered})"), utf8).saturating_add(1))
        })
        .max()
        .unwrap_or(0)
}

fn mode_tree_is_last_sibling(build: &ModeTreeBuild, id: &str) -> bool {
    let Some(item) = build.items.get(id) else {
        return true;
    };
    match item
        .parent
        .as_ref()
        .and_then(|parent| build.items.get(parent))
    {
        Some(parent) => parent
            .children
            .last()
            .map(|child| child == id)
            .unwrap_or(true),
        None => build.roots.last().map(|root| root == id).unwrap_or(true),
    }
}

fn mode_tree_tree_prefix(build: &ModeTreeBuild, item: &ModeTreeItem) -> String {
    if item.depth == 0 {
        return String::new();
    }

    let mut ancestors = Vec::new();
    let mut current = item.parent.as_ref();
    while let Some(parent_id) = current {
        ancestors.push(parent_id.clone());
        current = build
            .items
            .get(parent_id)
            .and_then(|parent| parent.parent.as_ref());
    }
    ancestors.reverse();

    let mut prefix = String::new();
    for ancestor_id in ancestors.iter().take(ancestors.len().saturating_sub(1)) {
        prefix.push_str(if mode_tree_is_last_sibling(build, ancestor_id) {
            "    "
        } else {
            "│   "
        });
    }
    prefix.push_str(if mode_tree_is_last_sibling(build, &item.id) {
        "└─> "
    } else {
        "├─> "
    });
    prefix
}

pub(super) fn mode_tree_list_rows(
    total_rows: u16,
    line_count: usize,
    preview_mode: PreviewMode,
) -> u16 {
    if preview_mode == PreviewMode::Off {
        return total_rows;
    }

    let line_count = u16::try_from(line_count).unwrap_or(u16::MAX);
    let mut height = match preview_mode {
        PreviewMode::Normal => {
            let mut height = (total_rows / 3) * 2;
            if height > line_count {
                height = total_rows / 2;
            }
            if height < 10 {
                height = total_rows;
            }
            height
        }
        PreviewMode::Big => {
            let mut height = total_rows / 4;
            if height > line_count {
                height = line_count;
            }
            if height < 2 {
                height = 2;
            }
            height
        }
        PreviewMode::Off => total_rows,
    };

    if total_rows.saturating_sub(height) < 2 {
        height = total_rows;
    }
    height
}

fn mode_tree_preview_title(mode: &ModeTreeClientState, build: &ModeTreeBuild) -> String {
    if build.no_matches {
        return " no matches ".to_owned();
    }

    let item_name = mode
        .selected_id
        .as_ref()
        .and_then(|id| build.items.get(id))
        .map(|item| {
            item.line
                .split(": ")
                .next()
                .unwrap_or(&item.line)
                .to_owned()
        })
        .unwrap_or_else(|| "Preview".to_owned());

    let mut title = format!(" {item_name}");
    if let Some(sort_order) = mode.sort_order {
        title.push_str(" (sort: ");
        title.push_str(sort_order_name(sort_order));
        if mode.reversed {
            title.push_str(", reversed");
        }
        title.push(')');
    }
    title.push(' ');
    title
}

fn render_mode_tree_box(
    frame: &mut Vec<u8>,
    start_row: u16,
    height: u16,
    cols: u16,
    title: &str,
    style: &Style,
    utf8: &Utf8Config,
) {
    if height < 2 || cols < 2 {
        return;
    }

    let inner = usize::from(cols.saturating_sub(2));
    let top = format!("┌{}┐", pad_box_title(title, inner, utf8));
    let middle = format!("│{}│", " ".repeat(inner));
    let bottom = format!("└{}┘", "─".repeat(inner));

    render_overlay_line(frame, start_row, cols, &top, style, utf8);
    for row in start_row.saturating_add(1)..start_row.saturating_add(height.saturating_sub(1)) {
        render_overlay_line(frame, row, cols, &middle, style, utf8);
    }
    render_overlay_line(
        frame,
        start_row.saturating_add(height.saturating_sub(1)),
        cols,
        &bottom,
        style,
        utf8,
    );
}

fn pad_box_title(title: &str, width: usize, utf8: &Utf8Config) -> String {
    let clipped = tmux_truncate_to_width(title, width, utf8);
    let visible = tmux_text_width(&clipped, utf8);
    format!("{clipped}{}", "─".repeat(width.saturating_sub(visible)))
}

pub(super) fn pad_visible_width(value: &str, width: usize, utf8: &Utf8Config) -> String {
    let clipped = tmux_truncate_to_width(value, width, utf8);
    let visible = tmux_text_width(&clipped, utf8);
    format!("{clipped}{}", " ".repeat(width.saturating_sub(visible)))
}
