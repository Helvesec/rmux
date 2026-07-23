use rmux_core::{formats::FormatContext, OptionStore, Pane, Session, Style, Utf8Config};
use rmux_proto::OptionName;

use crate::copy_mode::CopyModeSummary;
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};

use super::{
    apply_runtime_style_overlay, copy_mode_line_numbers, cursor_position_bytes,
    format_draw_content_width, format_draw_line, render_formatted_line, visible_pane_geometry,
    StatusGeometry,
};

pub(crate) fn render_copy_mode_position(
    session: &Session,
    options: &OptionStore,
    window_index: u32,
    pane: &Pane,
    summary: &CopyModeSummary,
    history_size: usize,
    alternate_on: bool,
) -> Vec<u8> {
    if !summary.show_position {
        return Vec::new();
    }
    let geometry = StatusGeometry::for_session(session, options);
    let Some(raw_pane_geometry) =
        visible_pane_geometry(session, options, pane, geometry.content_rows)
    else {
        return Vec::new();
    };
    let pane_geometry = crate::pane_scrollbar::PaneScrollbarConfig::resolve(
        options,
        session.name(),
        window_index,
        pane.index(),
    )
    .content_geometry(raw_pane_geometry, alternate_on, true);
    if pane_geometry.cols() == 0 || pane_geometry.rows() == 0 {
        return Vec::new();
    }
    let line_number_layout =
        copy_mode_line_numbers::layout_for_summary(session, options, pane, summary);
    let line_number_width =
        line_number_layout.map_or(0, |layout| layout.physical_width(pane_geometry.cols()));
    let content_cols = pane_geometry.cols().saturating_sub(line_number_width);
    if content_cols == 0 {
        return Vec::new();
    }

    let line_number_option = options.resolve_for_pane(
        session.name(),
        window_index,
        pane.index(),
        OptionName::CopyModeLineNumbers,
    );
    let (copy_position, copy_position_limit) =
        summary.position_for_line_number_option(line_number_option);
    let context = FormatContext::from_session(session)
        .with_window(session.active_window_index(), session.window(), true, false)
        .with_window_pane(session.window(), pane)
        .with_named_value("scroll_position", summary.scroll_position.to_string())
        .with_named_value("history_size", history_size.to_string())
        .with_named_value("copy_position", copy_position.to_string())
        .with_named_value("copy_position_limit", copy_position_limit.to_string())
        .with_named_value("search_timed_out", bool_text(summary.search_timed_out))
        .with_named_value("search_count", summary.search_count.to_string())
        .with_named_value(
            "search_count_partial",
            bool_text(summary.search_count_partial),
        )
        .with_named_value("top_line_time", summary.top_line_time.to_string());
    let runtime = RuntimeFormatContext::new(context)
        .with_options(options)
        .with_session(session)
        .with_window(session.active_window_index(), session.window())
        .with_pane(pane);
    let template = options
        .resolve_for_pane(
            session.name(),
            window_index,
            pane.index(),
            OptionName::CopyModePositionFormat,
        )
        .unwrap_or("[#{copy_position}/#{copy_position_limit}]");
    let style = apply_runtime_style_overlay(
        &Style::default(),
        options.resolve_for_window(
            session.name(),
            window_index,
            OptionName::CopyModePositionStyle,
        ),
        &runtime,
    );
    let expanded = format!(
        "#[align=right {}]{}",
        rmux_core::style_tostring(&style),
        render_runtime_template(template, &runtime, true)
    );
    let utf8 = Utf8Config::from_options(options);
    let content_width = format_draw_content_width(&expanded, &Style::default(), &utf8);
    let width = content_width.min(usize::from(content_cols));
    if width == 0 {
        return Vec::new();
    }
    let line =
        format_draw_line(&expanded, &Style::default(), width, &utf8).trim_leading_ascii_space();
    let mut frame = Vec::new();
    render_formatted_line(
        &mut frame,
        pane_geometry
            .x()
            .saturating_add(line_number_width)
            .saturating_add(content_cols.saturating_sub(line.width() as u16)),
        pane_geometry.y().saturating_add(geometry.content_y_offset),
        &line,
    );
    if line_number_layout.is_some()
        && summary.cursor_x >= u32::from(content_cols)
        && summary.cursor_y == 0
    {
        // tmux paints the clipped-cursor marker after the position badge.
        // Repaint it here because the pane body is composed before this
        // indicator in the final attach frame.
        frame.extend_from_slice(
            cursor_position_bytes(
                pane_geometry.y().saturating_add(geometry.content_y_offset),
                pane_geometry
                    .x()
                    .saturating_add(pane_geometry.cols().saturating_sub(1)),
            )
            .as_slice(),
        );
        frame.extend_from_slice(b"\x1b[0m$\x1b[0m");
    }
    frame
}

fn bool_text(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}
