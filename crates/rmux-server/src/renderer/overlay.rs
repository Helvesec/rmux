#![allow(dead_code)]

use rmux_core::{BoxLines, PaneGeometry, Style};
use rmux_proto::TerminalSize;

use super::{RenderedPrompt, StatusGeometry};
use crate::format_runtime::RuntimeFormatContext;
use crate::status_ranges::{StatusLineLayout, StatusRange, StatusRangeType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OverlayRect {
    pub(crate) x: u16,
    pub(crate) y: u16,
    pub(crate) width: u16,
    pub(crate) height: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OverlayMousePosition {
    pub(crate) x: u16,
    pub(crate) y: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OverlayPositionContext {
    pub(crate) client_size: TerminalSize,
    pub(crate) pane: Option<PaneGeometry>,
    pub(crate) mouse: Option<OverlayMousePosition>,
    pub(crate) status_at: Option<u16>,
    pub(crate) status_lines: u16,
    pub(crate) window_status_x: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MenuRenderItem {
    pub(crate) label: String,
    pub(crate) shortcut: Option<String>,
    pub(crate) separator: bool,
    pub(crate) selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MenuRenderSpec {
    pub(crate) rect: OverlayRect,
    pub(crate) title: String,
    pub(crate) style: Style,
    pub(crate) selected_style: Style,
    pub(crate) border_style: Style,
    pub(crate) border_lines: BoxLines,
    pub(crate) items: Vec<MenuRenderItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PopupRenderSpec {
    pub(crate) rect: OverlayRect,
    pub(crate) title: String,
    pub(crate) style: Style,
    pub(crate) border_style: Style,
    pub(crate) border_lines: BoxLines,
    pub(crate) content_lines: Vec<String>,
}

pub(crate) fn resolve_overlay_rect(
    mut runtime: RuntimeFormatContext<'_>,
    context: OverlayPositionContext,
    x_expr: Option<&str>,
    y_expr: Option<&str>,
    width: u16,
    height: u16,
) -> Option<OverlayRect> {
    let size = context.client_size;
    if width == 0 || height == 0 || width > size.cols || height > size.rows {
        return None;
    }

    runtime = runtime
        .with_named_value("popup_width", width.to_string())
        .with_named_value("popup_height", height.to_string())
        .with_named_value(
            "popup_centre_x",
            popup_centre_x(size.cols, width).to_string(),
        )
        .with_named_value(
            "popup_centre_y",
            popup_centre_y(size.rows, height).to_string(),
        );

    if let Some(mouse) = context.mouse {
        runtime = runtime
            .with_named_value("popup_mouse_x", mouse.x.to_string())
            .with_named_value("popup_mouse_y", mouse.y.to_string())
            .with_named_value(
                "popup_mouse_centre_x",
                popup_mouse_centre_x(size.cols, width, mouse.x).to_string(),
            )
            .with_named_value(
                "popup_mouse_centre_y",
                popup_mouse_centre_y(size.rows, height, mouse.y).to_string(),
            )
            .with_named_value(
                "popup_mouse_top",
                popup_mouse_top(size.rows, height, mouse.y).to_string(),
            )
            .with_named_value(
                "popup_mouse_bottom",
                popup_mouse_bottom(height, mouse.y).to_string(),
            );
    }

    let content_y_offset = if context.status_at == Some(0) {
        context.status_lines
    } else {
        0
    };
    if let Some(pane) = context.pane {
        runtime = runtime
            .with_named_value("popup_pane_left", pane.x().to_string())
            .with_named_value(
                "popup_pane_right",
                popup_pane_right(size.cols, width, pane).to_string(),
            )
            .with_named_value(
                "popup_pane_top",
                popup_pane_top(size.rows, height, content_y_offset, pane).to_string(),
            )
            .with_named_value(
                "popup_pane_bottom",
                popup_pane_bottom(content_y_offset, pane).to_string(),
            );
    }

    if let Some(status_at) = context.status_at {
        let line = 0_u16;
        let top_position = status_at == 0;
        if let Some(window_status_x) = context.window_status_x {
            runtime =
                runtime.with_named_value("popup_window_status_line_x", window_status_x.to_string());
            let window_status_y = if top_position {
                line.saturating_add(1).saturating_add(height)
            } else {
                size.rows
                    .saturating_sub(context.status_lines)
                    .saturating_add(line)
            };
            runtime =
                runtime.with_named_value("popup_window_status_line_y", window_status_y.to_string());
        }
        let status_line_y = if top_position {
            context.status_lines.saturating_add(height)
        } else {
            size.rows.saturating_sub(context.status_lines)
        };
        runtime = runtime.with_named_value("popup_status_line_y", status_line_y.to_string());
    }

    let x_template = match x_expr.unwrap_or("C") {
        "C" => "#{popup_centre_x}",
        "R" => "#{popup_pane_right}",
        "P" => "#{popup_pane_left}",
        "M" => "#{popup_mouse_centre_x}",
        "W" => "#{popup_window_status_line_x}",
        other => other,
    };
    let y_template = match y_expr.unwrap_or("C") {
        "C" => "#{popup_centre_y}",
        "P" => "#{popup_pane_bottom}",
        "M" => "#{popup_mouse_top}",
        "S" => "#{popup_status_line_y}",
        "W" => "#{popup_window_status_line_y}",
        other => other,
    };

    let resolved_x = crate::format_runtime::render_runtime_template(x_template, &runtime, false);
    let resolved_y = crate::format_runtime::render_runtime_template(y_template, &runtime, false);
    let x_value = resolved_x.parse::<i64>().unwrap_or_default();
    let mut y_value = resolved_y.parse::<i64>().unwrap_or_default();

    let max_x = i64::from(size.cols.saturating_sub(width));
    let x = x_value.clamp(0, max_x) as u16;

    if y_value < i64::from(height) {
        y_value = 0;
    } else {
        y_value -= i64::from(height);
    }
    let max_y = i64::from(size.rows.saturating_sub(height));
    let y = y_value.clamp(0, max_y) as u16;

    Some(OverlayRect {
        x,
        y,
        width,
        height,
    })
}

pub(crate) fn status_line_layout(
    session: &rmux_core::Session,
    options: &rmux_core::OptionStore,
    attached_count: usize,
    prompt: Option<&RenderedPrompt>,
) -> Option<StatusLineLayout> {
    let geometry = StatusGeometry::for_session(session, options);
    let _ = geometry.status_y?;
    let width = usize::from(geometry.terminal_size.cols);
    if width == 0 {
        return None;
    }

    if let Some(prompt) = prompt {
        let utf8 = rmux_core::Utf8Config::from_options(options);
        let rendered = super::status_runs_width(
            &super::prompt_status_runs(
                session,
                options,
                u16::try_from(width).unwrap_or(u16::MAX),
                prompt,
            ),
            &utf8,
        );
        let mut ranges = Vec::new();
        if rendered > 0 {
            push_range(
                &mut ranges,
                0,
                rendered.saturating_sub(1),
                StatusRangeType::Left,
            );
        }
        return Some(StatusLineLayout { ranges });
    }

    Some(StatusLineLayout {
        ranges: super::status_bar_line(
            session,
            options,
            u16::try_from(width).unwrap_or(u16::MAX),
            attached_count,
        )
        .ranges,
    })
}

pub(crate) fn render_menu_overlay(spec: &MenuRenderSpec) -> Vec<u8> {
    let mut frame = Vec::new();
    fill_rect(&mut frame, spec.rect, &spec.style);
    if spec.border_lines.visible() {
        draw_box(
            &mut frame,
            spec.rect,
            &spec.border_style,
            spec.border_lines,
            Some(spec.title.as_str()),
            TitleAlign::Left,
        );
    }

    let inner = inner_rect(spec.rect, spec.border_lines);
    let row_fill_width = usize::from(inner.width);
    let row_text_width = usize::from(inner.width.saturating_sub(2));
    let row_fill = " ".repeat(row_fill_width);
    let start_x = inner.x.saturating_add(1);
    for (index, item) in spec.items.iter().enumerate() {
        let row = inner
            .y
            .saturating_add(u16::try_from(index).unwrap_or(u16::MAX));
        if row >= inner.y.saturating_add(inner.height) {
            break;
        }
        if item.separator {
            draw_separator(&mut frame, spec, row);
            continue;
        }

        let row_style = if item.selected {
            &spec.selected_style
        } else {
            &spec.style
        };
        draw_styled_text(&mut frame, inner.x, row, &row_fill, row_style);

        let label_text = super::sanitize_status_text(item.label.clone());
        let shortcut_text = item
            .shortcut
            .as_ref()
            .map(|shortcut| super::sanitize_status_text(shortcut.clone()));
        let label_width = shortcut_text
            .as_ref()
            .map(|shortcut| {
                row_text_width.saturating_sub(
                    super::tmux_text_width(shortcut, &rmux_core::Utf8Config::default()) + 1,
                )
            })
            .unwrap_or(row_text_width);
        let label = label_text;
        draw_formatted_text(&mut frame, start_x, row, &label, row_style, label_width);

        if let Some(shortcut) = shortcut_text.as_ref() {
            let shortcut_width =
                super::tmux_text_width(shortcut, &rmux_core::Utf8Config::default());
            let shortcut_x = start_x.saturating_add(
                u16::try_from(row_text_width.saturating_sub(shortcut_width)).unwrap_or(0),
            );
            draw_formatted_text(
                &mut frame,
                shortcut_x,
                row,
                shortcut,
                row_style,
                shortcut_width,
            );
        }
    }
    frame
}

pub(crate) fn render_popup_overlay(spec: &PopupRenderSpec) -> Vec<u8> {
    let mut frame = Vec::new();
    fill_rect(&mut frame, spec.rect, &spec.style);
    if spec.border_lines.visible() {
        draw_box(
            &mut frame,
            spec.rect,
            &spec.border_style,
            spec.border_lines,
            Some(spec.title.as_str()),
            TitleAlign::Centre,
        );
    }

    let inner = inner_rect(spec.rect, spec.border_lines);
    for (index, line) in spec.content_lines.iter().enumerate() {
        let row = inner
            .y
            .saturating_add(u16::try_from(index).unwrap_or(u16::MAX));
        if row >= inner.y.saturating_add(inner.height) {
            break;
        }
        draw_formatted_text(
            &mut frame,
            inner.x,
            row,
            line,
            &spec.style,
            usize::from(inner.width),
        );
    }
    frame
}

fn popup_centre_x(cols: u16, width: u16) -> i64 {
    let value = (i64::from(cols).saturating_sub(1)) / 2 - i64::from(width) / 2;
    value.max(0)
}

fn popup_centre_y(rows: u16, height: u16) -> i64 {
    let value = (i64::from(rows).saturating_sub(1)) / 2 + i64::from(height) / 2;
    value.min(i64::from(rows.saturating_sub(height)))
}

fn popup_mouse_centre_x(cols: u16, width: u16, mouse_x: u16) -> i64 {
    (i64::from(mouse_x) - i64::from(width) / 2).clamp(0, i64::from(cols.saturating_sub(width)))
}

fn popup_mouse_centre_y(rows: u16, height: u16, mouse_y: u16) -> i64 {
    let value = i64::from(mouse_y) - i64::from(height) / 2;
    if value + i64::from(height) >= i64::from(rows) {
        i64::from(rows.saturating_sub(height))
    } else {
        value
    }
}

fn popup_mouse_top(rows: u16, height: u16, mouse_y: u16) -> i64 {
    let value = i64::from(mouse_y) + i64::from(height);
    if value >= i64::from(rows) {
        i64::from(rows.saturating_sub(1))
    } else {
        value
    }
}

fn popup_mouse_bottom(height: u16, mouse_y: u16) -> i64 {
    let value = i64::from(mouse_y) - i64::from(height);
    value.max(0)
}

fn popup_pane_top(rows: u16, height: u16, content_y_offset: u16, pane: PaneGeometry) -> i64 {
    let value = i64::from(content_y_offset) + i64::from(pane.y()) + i64::from(height);
    if value >= i64::from(rows) {
        i64::from(rows.saturating_sub(height))
    } else {
        value
    }
}

fn popup_pane_bottom(content_y_offset: u16, pane: PaneGeometry) -> i64 {
    i64::from(content_y_offset) + i64::from(pane.y()) + i64::from(pane.rows())
}

fn popup_pane_right(cols: u16, width: u16, pane: PaneGeometry) -> i64 {
    let value = i64::from(pane.x()) + i64::from(pane.cols()) - i64::from(width);
    value.clamp(0, i64::from(cols.saturating_sub(width)))
}

fn push_range(ranges: &mut Vec<StatusRange>, start: usize, end: usize, kind: StatusRangeType) {
    super::push_range(ranges, start, end, kind);
}

fn fill_rect(frame: &mut Vec<u8>, rect: OverlayRect, style: &Style) {
    let blank = " ".repeat(usize::from(rect.width));
    for offset in 0..rect.height {
        draw_styled_text(frame, rect.x, rect.y.saturating_add(offset), &blank, style);
    }
}

fn inner_rect(rect: OverlayRect, border_lines: BoxLines) -> OverlayRect {
    if !border_lines.visible() {
        return rect;
    }
    OverlayRect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(1),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TitleAlign {
    Left,
    Centre,
}

fn draw_box(
    frame: &mut Vec<u8>,
    rect: OverlayRect,
    style: &Style,
    lines: BoxLines,
    title: Option<&str>,
    title_align: TitleAlign,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let horizontal = lines
        .horizontal()
        .to_string()
        .repeat(usize::from(rect.width.saturating_sub(2)));
    let top = format!("{}{}{}", lines.top_left(), horizontal, lines.top_right());
    let bottom = format!(
        "{}{}{}",
        lines.bottom_left(),
        horizontal,
        lines.bottom_right()
    );
    draw_styled_text(frame, rect.x, rect.y, &top, style);
    if rect.height > 1 {
        draw_styled_text(
            frame,
            rect.x,
            rect.y.saturating_add(rect.height.saturating_sub(1)),
            &bottom,
            style,
        );
    }
    if rect.height > 2 {
        for row in 1..rect.height.saturating_sub(1) {
            draw_styled_text(
                frame,
                rect.x,
                rect.y.saturating_add(row),
                &lines.vertical().to_string(),
                style,
            );
            draw_styled_text(
                frame,
                rect.x.saturating_add(rect.width.saturating_sub(1)),
                rect.y.saturating_add(row),
                &lines.vertical().to_string(),
                style,
            );
        }
    }

    let Some(title) = title.filter(|title| !title.is_empty()) else {
        return;
    };
    let inner_width = usize::from(rect.width.saturating_sub(4));
    if inner_width == 0 {
        return;
    }
    let expanded = match title_align {
        TitleAlign::Left => format!("#[align=left]{title}"),
        TitleAlign::Centre => format!("#[align=centre]{title}"),
    };
    draw_formatted_text(
        frame,
        rect.x.saturating_add(2),
        rect.y,
        &expanded,
        style,
        inner_width,
    );
}

fn draw_separator(frame: &mut Vec<u8>, spec: &MenuRenderSpec, row: u16) {
    let inner = inner_rect(spec.rect, spec.border_lines);
    if spec.border_lines.visible() {
        if spec.rect.width < 2 {
            return;
        }
        let line = format!(
            "{}{}{}",
            spec.border_lines.left_join(),
            spec.border_lines
                .horizontal()
                .to_string()
                .repeat(usize::from(spec.rect.width.saturating_sub(2))),
            spec.border_lines.right_join()
        );
        draw_styled_text(frame, spec.rect.x, row, &line, &spec.border_style);
    } else {
        let line = spec
            .border_lines
            .horizontal()
            .to_string()
            .repeat(usize::from(inner.width));
        draw_styled_text(frame, inner.x, row, &line, &spec.border_style);
    }
}

fn draw_styled_text(frame: &mut Vec<u8>, x: u16, y: u16, text: &str, style: &Style) {
    frame.extend_from_slice(b"\x1b[s\x1b[0m");
    frame.extend_from_slice(super::cursor_position_bytes(y, x).as_slice());
    frame.extend_from_slice(super::style_sgr_bytes(style, true).as_slice());
    frame.extend_from_slice(text.as_bytes());
    frame.extend_from_slice(b"\x1b[0m\x1b[u");
}

fn draw_formatted_text(
    frame: &mut Vec<u8>,
    x: u16,
    y: u16,
    text: &str,
    style: &Style,
    width: usize,
) {
    let line = super::format_draw_line(
        &super::sanitize_status_text(text.to_owned()),
        style,
        width,
        &rmux_core::Utf8Config::default(),
    );
    super::render_formatted_line(frame, x, y, &line);
}

#[cfg(test)]
#[path = "overlay/tests.rs"]
mod tests;
