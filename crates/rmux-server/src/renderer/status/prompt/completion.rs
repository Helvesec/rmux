use rmux_core::{text_width, BoxLines, OptionStore, Style, Utf8Config};
use rmux_proto::OptionName;

use super::super::StatusGeometry;
use crate::renderer::{render_menu_overlay, MenuRenderSpec, OverlayRect, RenderedPrompt};

pub(super) fn render_completion_menu(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    prompt: &RenderedPrompt,
    geometry: StatusGeometry,
    prompt_y: u16,
) -> Vec<u8> {
    if prompt.completion_menu.is_empty() {
        return Vec::new();
    }
    let size = geometry.terminal_size;
    let count =
        prompt
            .completion_menu
            .len()
            .min(crate::status_lines::prompt_completion_menu_height(
                size,
                geometry.status_lines,
            ));
    if count == 0 {
        return Vec::new();
    }
    let height = count as u16 + 2;
    let utf8 = Utf8Config::from_options(options);
    let width = prompt
        .completion_menu
        .iter()
        .map(|item| text_width(&item.label, &utf8) + 8)
        .max()
        .unwrap_or(0)
        .min(usize::from(size.cols)) as u16;
    let leading = prompt.input.len() - prompt.input.trim_start_matches(' ').len();
    let x = text_width(&prompt.prompt, &utf8)
        .saturating_add(leading)
        .saturating_sub(2);
    let x = x.min(usize::from(size.cols.saturating_sub(width))) as u16;
    let y = if prompt_y >= height {
        prompt_y - height
    } else {
        geometry.status_lines
    };
    let style = |name| {
        Style::parse(
            options
                .resolve_for_window(session.name(), session.active_window_index(), name)
                .unwrap_or_default(),
        )
        .unwrap_or_default()
    };
    render_menu_overlay(&MenuRenderSpec {
        rect: OverlayRect {
            x,
            y,
            width,
            height,
        },
        title: String::new(),
        style: style(OptionName::MenuStyle),
        selected_style: style(OptionName::MenuSelectedStyle),
        border_style: style(OptionName::MenuBorderStyle),
        border_lines: BoxLines::parse(options.resolve_for_window(
            session.name(),
            session.active_window_index(),
            OptionName::MenuBorderLines,
        )),
        items: prompt.completion_menu[..count].to_vec(),
    })
}
