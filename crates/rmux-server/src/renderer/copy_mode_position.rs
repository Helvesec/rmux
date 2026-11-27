use rmux_core::{formats::FormatContext, OptionStore, Pane, Style, Utf8Config};
use rmux_proto::OptionName;

use crate::copy_mode::CopyModeSummary;
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};

use super::projection::{
    ProjectedPaneScrollbarContext, TerminalPaneProjection, TerminalSceneProjection,
};
use super::{
    apply_runtime_style_overlay, copy_mode_line_numbers, cursor_position_bytes,
    format_draw_content_width, format_draw_line, render_formatted_line_segment,
};

pub(crate) fn render_copy_mode_position(
    session: rmux_core::ManagedSessionView<'_>,
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
    let projection = TerminalSceneProjection::resolve(session, options);
    let Some(pane_projection) = projection.pane_projection(
        session,
        options,
        pane,
        ProjectedPaneScrollbarContext {
            history_size,
            alternate_on,
            copy_mode_scroll_position: Some(summary.scroll_position),
        },
    ) else {
        return Vec::new();
    };
    if pane_projection.spans().is_empty() {
        return Vec::new();
    }
    let line_number_layout =
        copy_mode_line_numbers::layout_for_summary(session, options, pane, summary);
    let line_number_width = line_number_layout.map_or(0, |layout| {
        layout.physical_width(pane_projection.source_width())
    });
    let content_cols = pane_projection
        .source_width()
        .saturating_sub(line_number_width);
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
    let context = FormatContext::new()
        .with_managed_session(session)
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
    let source_x = usize::from(line_number_width)
        .saturating_add(usize::from(content_cols).saturating_sub(line.width()));
    render_projected_formatted_line(&mut frame, &pane_projection, source_x, &line);
    if line_number_layout.is_some()
        && summary.cursor_x >= u32::from(content_cols)
        && summary.cursor_y == 0
    {
        // tmux paints the clipped-cursor marker after the position badge.
        // Repaint it here because the pane body is composed before this
        // indicator in the final attach frame.
        if let Some((x, y)) = projected_row_zero_cell(
            &pane_projection,
            usize::from(pane_projection.source_width().saturating_sub(1)),
        ) {
            frame.extend_from_slice(cursor_position_bytes(y, x).as_slice());
            frame.extend_from_slice(b"\x1b[0m$\x1b[0m");
        }
    }
    frame
}

fn render_projected_formatted_line(
    frame: &mut Vec<u8>,
    projection: &TerminalPaneProjection,
    source_x: usize,
    line: &super::FormattedLine,
) {
    let source_end = source_x.saturating_add(line.width());
    for span in projection
        .spans()
        .iter()
        .filter(|span| span.source_row == 0)
    {
        let span_start = span.source_col;
        let span_end = span_start.saturating_add(usize::from(span.cols));
        let start = source_x.max(span_start);
        let end = source_end.min(span_end);
        if start >= end {
            continue;
        }
        let x = span.x.saturating_add(
            u16::try_from(start.saturating_sub(span.source_col)).unwrap_or(u16::MAX),
        );
        render_formatted_line_segment(
            frame,
            x,
            span.y,
            line,
            start.saturating_sub(source_x),
            end.saturating_sub(start),
        );
    }
}

fn projected_row_zero_cell(
    projection: &TerminalPaneProjection,
    source_x: usize,
) -> Option<(u16, u16)> {
    projection
        .spans()
        .iter()
        .find(|span| {
            span.source_row == 0
                && source_x >= span.source_col
                && source_x < span.source_col.saturating_add(usize::from(span.cols))
        })
        .map(|span| {
            (
                span.x.saturating_add(
                    u16::try_from(source_x.saturating_sub(span.source_col)).unwrap_or(u16::MAX),
                ),
                span.y,
            )
        })
}

fn bool_text(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}
