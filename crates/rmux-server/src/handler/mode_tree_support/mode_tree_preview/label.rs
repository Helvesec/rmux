use rmux_core::{
    formats::{styled_text_width, truncate_styled_text_to_width, FormatContext},
    Pane, Style, Utf8Config, Window,
};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;
use crate::renderer::format_draw_line;

use super::PreviewColumn;

#[cfg(test)]
#[path = "../../../handler_mode_tree/preview/label_tests.rs"]
mod tests;

pub(in crate::handler::mode_tree_support) struct PreviewLabel {
    pub(in crate::handler::mode_tree_support) text: String,
    pub(in crate::handler::mode_tree_support) style: Style,
}

pub(super) fn resolve_label(
    state: &HandlerState,
    session: rmux_core::ManagedSessionView<'_>,
    window_index: u32,
    window: &Window,
    pane: Option<&Pane>,
    window_context: bool,
) -> PreviewLabel {
    let mut base = FormatContext::new()
        .with_managed_session(session)
        .with_window(
            window_index,
            window,
            window_index == session.active_window_index(),
            Some(window_index) == session.last_window_index(),
        );
    if let Some(pane) = pane {
        base = base.with_pane(pane, pane.index() == window.active_pane_index());
    }
    let mut runtime = RuntimeFormatContext::new(base)
        .with_state(state)
        .with_session(session)
        .with_window(window_index, window)
        .with_named_value("window_format", if window_context { "1" } else { "0" })
        .with_named_value("pane_format", if window_context { "0" } else { "1" });
    if let Some(pane) = pane {
        runtime = runtime.with_pane(pane);
    }
    let value = |name| {
        if let Some(pane) = pane.filter(|_| !window_context) {
            state
                .options
                .resolve_name_for_pane(session.name(), window_index, pane.index(), name)
        } else {
            state
                .options
                .resolve_name_for_window(session.name(), window_index, name)
        }
        .unwrap_or_default()
    };
    let text = render_runtime_template(&value("tree-mode-preview-format"), &runtime, false);
    let style = Style::parse(&render_runtime_template(
        &value("tree-mode-preview-style"),
        &runtime,
        false,
    ))
    .map(|style| Style::with_cell(style.cell))
    .unwrap_or_default();
    PreviewLabel { text, style }
}

pub(in crate::handler::mode_tree_support) fn render_preview_segment_row(
    column: &PreviewColumn,
    row: usize,
    width: usize,
    height: usize,
    utf8: &Utf8Config,
) -> String {
    let content = column
        .lines
        .get(row)
        .filter(|line| !line.is_empty())
        .cloned()
        .unwrap_or_else(|| " ".repeat(width));
    if width < 5 || height < 3 {
        return content;
    }
    let label = truncate_styled_text_to_width(&column.label.text, width - 4, utf8);
    let label_width = styled_text_width(&label, utf8);
    let y = height.div_ceil(2);
    if label_width == 0 || !(y - 1..=y + 1).contains(&row) {
        return content;
    }
    let x = (width - label_width).div_ceil(2);
    let box_text = if row == y {
        format!("│{}│", " ".repeat(label_width + 2))
    } else {
        let (left, right) = if row < y {
            ('┌', '┐')
        } else {
            ('└', '┘')
        };
        format!("{left}{}{right}", "─".repeat(label_width + 2))
    };
    let mut line = format_draw_line(&content, &Style::default(), width, utf8);
    line.overlay(
        x - 2,
        &format_draw_line(&box_text, &Style::default(), label_width + 4, utf8),
    );
    if row == y {
        line.overlay(
            x,
            &format_draw_line(&label, &column.label.style, label_width, utf8),
        );
    }
    line.to_styled_text()
}
