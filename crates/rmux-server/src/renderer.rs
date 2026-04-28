use rmux_core::input::{
    Colour, GridAttr, COLOUR_DEFAULT, COLOUR_FLAG_256, COLOUR_FLAG_RGB, COLOUR_NONE,
    COLOUR_TERMINAL,
};
use rmux_core::style::{parse_colour, Style};
use rmux_core::GridRenderOptions;
use rmux_core::{
    formats::FormatContext, text_width as tmux_text_width, OptionStore, Pane, Screen,
    ScreenCaptureRange, Session, Utf8Config,
};
use rmux_proto::OptionName;

use crate::copy_mode::CopyModeSummary;
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
#[path = "renderer/borders.rs"]
mod borders;
#[path = "renderer/clock_mode.rs"]
mod clock_mode;
#[path = "renderer/display_panes.rs"]
mod display_panes;
#[path = "renderer/format_draw.rs"]
mod format_draw;
#[path = "renderer/overlay.rs"]
mod overlay;
#[path = "renderer/pane_delta.rs"]
mod pane_delta;
#[path = "renderer/status.rs"]
mod status;
#[cfg(test)]
use borders::{border_cells, BorderCell, BorderStyle};
use borders::{content_pane_geometry, render_cells, runtime_border_cells};
#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use clock_mode::{
    render_clock_overlay, render_clock_restore_frame, ClockPaneRestoreData,
};
#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use display_panes::{
    display_pane_targets, display_panes_label_count, render_display_panes_clear,
    render_display_panes_clear_with_base, render_display_panes_overlay,
};
pub(crate) use format_draw::{
    format_draw_content_width, format_draw_line, render_formatted_line, FormattedLine,
};
#[allow(unused_imports)]
pub(crate) use overlay::{
    render_menu_overlay, render_popup_overlay, resolve_overlay_rect, status_line_layout,
    MenuRenderItem, MenuRenderSpec, OverlayMousePosition, OverlayPositionContext, OverlayRect,
    PopupRenderSpec,
};
pub(crate) use pane_delta::{PaneRenderDelta, PaneRenderSnapshot};
#[cfg(test)]
use status::status_bar_runs;
use status::{
    format_status_message_line, prompt_status_runs, render_status_bar, sanitize_status_text,
    status_bar_line, status_runs_width, StatusGeometry,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderedPrompt {
    pub(crate) prompt: String,
    pub(crate) input: String,
    pub(crate) cursor: usize,
    pub(crate) command_prompt: bool,
}

pub(crate) fn render(session: &Session, options: &OptionStore) -> Vec<u8> {
    render_with_attached_count_and_prompt(session, options, 0, None)
}

#[allow(dead_code)]
pub(crate) fn render_with_attached_count(
    session: &Session,
    options: &OptionStore,
    attached_count: usize,
) -> Vec<u8> {
    render_with_attached_count_and_prompt(session, options, attached_count, None)
}

pub(crate) fn render_with_attached_count_and_prompt(
    session: &Session,
    options: &OptionStore,
    attached_count: usize,
    prompt: Option<&RenderedPrompt>,
) -> Vec<u8> {
    render_with_attached_count_prompt_and_pane_title(session, options, attached_count, prompt, None)
}

pub(crate) fn render_with_attached_count_prompt_and_pane_title(
    session: &Session,
    options: &OptionStore,
    attached_count: usize,
    prompt: Option<&RenderedPrompt>,
    pane_title: Option<&str>,
) -> Vec<u8> {
    let geometry = StatusGeometry::for_session(session, options);
    let mut frame = Vec::new();

    if !session.window().is_zoomed() && session.window().pane_count() > 1 {
        frame.extend_from_slice(
            render_cells(runtime_border_cells(session, options, geometry).as_slice()).as_slice(),
        );
    }

    frame.extend_from_slice(
        render_status_bar(
            session,
            options,
            geometry,
            attached_count,
            prompt,
            pane_title,
        )
        .as_slice(),
    );
    frame
}

pub(crate) fn render_pane_screen(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    screen: &Screen,
) -> Vec<u8> {
    let geometry = StatusGeometry::for_session(session, options);
    let pane_geometry = content_pane_geometry(pane, geometry.content_rows);
    if pane_geometry.cols() == 0 || pane_geometry.rows() == 0 {
        return Vec::new();
    }

    let mut styled_screen = screen.clone();
    if let Some(style) = options.resolve_for_pane(
        session.name(),
        session.active_window_index(),
        pane.index(),
        OptionName::CopyModeSelectionStyle,
    ) {
        styled_screen.overlay_style_on_selected(style);
    }

    let rendered = styled_screen.capture_transcript(
        ScreenCaptureRange::default(),
        GridRenderOptions {
            with_sequences: true,
            include_empty_cells: true,
            trim_spaces: false,
            ..GridRenderOptions::default()
        },
    );
    let utf8 = Utf8Config::from_options(options);
    let mut frame = Vec::new();
    frame.extend_from_slice(b"\x1b[s\x1b[0m");
    for (row, line) in rendered.split(|byte| *byte == b'\n').enumerate() {
        if row >= usize::from(pane_geometry.rows()) {
            break;
        }
        let line = truncate_rendered_pane_line(line, usize::from(pane_geometry.cols()), &utf8);
        frame.extend_from_slice(
            cursor_position_bytes(
                pane_geometry
                    .y()
                    .saturating_add(geometry.content_y_offset)
                    .saturating_add(row as u16),
                pane_geometry.x(),
            )
            .as_slice(),
        );
        frame.extend_from_slice(&line);
    }
    frame.extend_from_slice(b"\x1b[0m\x1b[u");
    frame
}

fn truncate_rendered_pane_line(line: &[u8], width: usize, utf8: &Utf8Config) -> Vec<u8> {
    if width == 0 {
        return Vec::new();
    }

    let mut output = Vec::with_capacity(line.len().min(width.saturating_mul(4)));
    let mut used = 0_usize;
    let mut index = 0_usize;
    while index < line.len() {
        if line[index] == 0x1b {
            let end = ansi_sequence_end(line, index);
            output.extend_from_slice(&line[index..end]);
            index = end;
            continue;
        }

        let Ok(rest) = std::str::from_utf8(&line[index..]) else {
            break;
        };
        let Some(ch) = rest.chars().next() else {
            break;
        };
        let ch_len = ch.len_utf8();
        let mut buf = [0_u8; 4];
        let text = ch.encode_utf8(&mut buf);
        let cell_width = tmux_text_width(text, utf8);
        if cell_width != 0 && used.saturating_add(cell_width) > width {
            break;
        }
        output.extend_from_slice(&line[index..index + ch_len]);
        used = used.saturating_add(cell_width);
        index += ch_len;
    }
    output
}

fn ansi_sequence_end(line: &[u8], start: usize) -> usize {
    let Some(&kind) = line.get(start.saturating_add(1)) else {
        return line.len();
    };
    match kind {
        b'[' => line[start + 2..]
            .iter()
            .position(|byte| (0x40..=0x7e).contains(byte))
            .map_or(line.len(), |offset| start + 3 + offset),
        b']' => osc_sequence_end(line, start),
        _ => start.saturating_add(2).min(line.len()),
    }
}

fn osc_sequence_end(line: &[u8], start: usize) -> usize {
    let mut index = start.saturating_add(2);
    while index < line.len() {
        match line[index] {
            0x07 => return index + 1,
            0x1b if line.get(index + 1) == Some(&b'\\') => return index + 2,
            _ => index += 1,
        }
    }
    line.len()
}

pub(crate) fn render_pane_cursor(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    screen: &Screen,
) -> Vec<u8> {
    let geometry = StatusGeometry::for_session(session, options);
    let pane_geometry = content_pane_geometry(pane, geometry.content_rows);
    if pane_geometry.cols() == 0 || pane_geometry.rows() == 0 {
        return Vec::new();
    }

    let (cursor_x, cursor_y) = screen.cursor_position();
    let x = pane_geometry
        .x()
        .saturating_add(cursor_x.min(u32::from(pane_geometry.cols().saturating_sub(1))) as u16);
    let y = pane_geometry
        .y()
        .saturating_add(geometry.content_y_offset)
        .saturating_add(cursor_y.min(u32::from(pane_geometry.rows().saturating_sub(1))) as u16);
    cursor_position_bytes(y, x)
}

pub(crate) fn render_copy_mode_position(
    session: &Session,
    options: &OptionStore,
    window_index: u32,
    pane: &Pane,
    summary: &CopyModeSummary,
    history_size: usize,
) -> Vec<u8> {
    let geometry = StatusGeometry::for_session(session, options);
    let pane_geometry = content_pane_geometry(pane, geometry.content_rows);
    if pane_geometry.cols() == 0 || pane_geometry.rows() == 0 {
        return Vec::new();
    }

    let context = FormatContext::from_session(session)
        .with_window(session.active_window_index(), session.window(), true, false)
        .with_window_pane(session.window(), pane)
        .with_named_value("scroll_position", summary.scroll_position.to_string())
        .with_named_value("history_size", history_size.to_string())
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
        .unwrap_or("[#{scroll_position}/#{history_size}]");
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
    let width = format_draw_content_width(&expanded, &Style::default(), &utf8)
        .min(usize::from(pane_geometry.cols()));
    if width == 0 {
        return Vec::new();
    }
    let line = format_draw_line(&expanded, &Style::default(), width, &utf8);
    let mut frame = Vec::new();
    render_formatted_line(
        &mut frame,
        pane_geometry
            .x()
            .saturating_add(pane_geometry.cols().saturating_sub(width as u16)),
        pane_geometry.y().saturating_add(geometry.content_y_offset),
        &line,
    );
    frame
}

fn bool_text(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

pub(crate) fn render_status_only_with_attached_count_and_prompt(
    session: &Session,
    options: &OptionStore,
    attached_count: usize,
    prompt: Option<&RenderedPrompt>,
) -> Vec<u8> {
    let geometry = StatusGeometry::for_session(session, options);
    render_status_bar(session, options, geometry, attached_count, prompt, None)
}

pub(crate) fn render_status_message(
    session: &Session,
    options: &OptionStore,
    message: &str,
) -> Vec<u8> {
    let geometry = StatusGeometry::for_session(session, options);
    let Some(status_y) = geometry.status_y else {
        return Vec::new();
    };
    let width = usize::from(geometry.terminal_size.cols);
    if width == 0 {
        return Vec::new();
    }

    let line = format_status_message_line(session, options, width, message, false);
    let mut frame = Vec::new();
    render_formatted_line(&mut frame, 0, status_y, &line);
    frame
}

fn cursor_position_bytes(y: u16, x: u16) -> Vec<u8> {
    format!("\x1b[{};{}H", y.saturating_add(1), x.saturating_add(1)).into_bytes()
}

fn parse_standalone_style(value: Option<&str>) -> Style {
    let Some(value) = value else {
        return Style::default();
    };
    Style::parse(value).unwrap_or_default()
}

fn apply_style_overlay(base: &Style, value: Option<&str>) -> Style {
    let Some(value) = value else {
        return base.clone();
    };
    base.overlaid(value).unwrap_or_else(|_| base.clone())
}

fn apply_runtime_style_overlay(
    base: &Style,
    value: Option<&str>,
    runtime: &RuntimeFormatContext<'_>,
) -> Style {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return base.clone();
    };
    let expanded = render_runtime_template(value, runtime, true);
    base.overlaid(&expanded).unwrap_or_else(|_| base.clone())
}

fn push_range(
    ranges: &mut Vec<crate::status_ranges::StatusRange>,
    start: usize,
    end: usize,
    kind: crate::status_ranges::StatusRangeType,
) {
    if start > end {
        return;
    }
    let Ok(start) = u16::try_from(start) else {
        return;
    };
    let Ok(end) = u16::try_from(end) else {
        return;
    };
    ranges.push(crate::status_ranges::StatusRange {
        x: start..=end,
        kind,
    });
}

pub(super) fn parse_option_colour(value: Option<&str>) -> Option<Colour> {
    value.and_then(|s| parse_colour(s).ok())
}

fn colour_inherits_base(colour: Colour) -> bool {
    matches!(colour, COLOUR_DEFAULT | COLOUR_TERMINAL)
}

fn colour_is_unset(colour: Colour) -> bool {
    matches!(colour, COLOUR_DEFAULT | COLOUR_TERMINAL | COLOUR_NONE)
}

fn style_sgr_bytes(style: &Style, use_fill_background: bool) -> Vec<u8> {
    let mut params = Vec::new();
    for code in attr_sgr_codes(style.cell.attr) {
        params.push(sgr_code_text(code));
    }
    if !colour_is_unset(style.cell.fg) {
        push_colour_parameter(&mut params, foreground_sgr_parameter(style.cell.fg));
    }
    let background = effective_background(style, use_fill_background);
    if !colour_is_unset(background) {
        push_colour_parameter(&mut params, background_sgr_parameter(background));
    }
    if !colour_is_unset(style.cell.us) {
        push_colour_parameter(&mut params, underline_sgr_parameter(style.cell.us));
    }
    if params.is_empty() {
        return b"\x1b[0m".to_vec();
    }
    if style_requires_reset_prefix(style) {
        params.insert(0, "0".to_owned());
    }
    format!("\x1b[{}m", params.join(";")).into_bytes()
}

fn style_requires_reset_prefix(style: &Style) -> bool {
    style.cell.attr != 0 || !colour_is_unset(style.cell.bg) || !colour_is_unset(style.cell.us)
}

fn effective_background(style: &Style, use_fill_background: bool) -> Colour {
    if colour_is_unset(style.cell.bg) && use_fill_background && !colour_is_unset(style.fill) {
        style.fill
    } else {
        style.cell.bg
    }
}

pub(super) fn foreground_sgr_parameter(colour: Colour) -> Option<String> {
    colour_sgr_parameter(colour, ColourTarget::Foreground)
}

pub(super) fn background_sgr_parameter(colour: Colour) -> Option<String> {
    colour_sgr_parameter(colour, ColourTarget::Background)
}

fn underline_sgr_parameter(colour: Colour) -> Option<String> {
    colour_sgr_parameter(colour, ColourTarget::Underline)
}

fn colour_sgr_parameter(colour: Colour, target: ColourTarget) -> Option<String> {
    if colour == COLOUR_NONE {
        return None;
    }

    let prefix = match target {
        ColourTarget::Foreground => "38",
        ColourTarget::Background => "48",
        ColourTarget::Underline => "58",
    };
    let default = match target {
        ColourTarget::Foreground => "39",
        ColourTarget::Background => "49",
        ColourTarget::Underline => "59",
    };

    if colour & COLOUR_FLAG_256 != 0 {
        return Some(format!("{prefix};5;{}", colour & 0xff));
    }
    if colour & COLOUR_FLAG_RGB != 0 {
        let red = (colour >> 16) & 0xff;
        let green = (colour >> 8) & 0xff;
        let blue = colour & 0xff;
        return Some(format!("{prefix};2;{red};{green};{blue}"));
    }

    match target {
        ColourTarget::Foreground => match colour {
            0..=7 => Some((colour + 30).to_string()),
            90..=97 => Some(colour.to_string()),
            _ if colour_inherits_base(colour) => Some(default.to_owned()),
            _ => None,
        },
        ColourTarget::Background => match colour {
            0..=7 => Some((colour + 40).to_string()),
            90..=97 => Some((colour + 10).to_string()),
            _ if colour_inherits_base(colour) => Some(default.to_owned()),
            _ => None,
        },
        ColourTarget::Underline => match colour {
            _ if colour_inherits_base(colour) => Some(default.to_owned()),
            _ => None,
        },
    }
}

fn push_colour_parameter(params: &mut Vec<String>, parameter: Option<String>) {
    if let Some(parameter) = parameter {
        params.push(parameter);
    }
}

fn attr_sgr_codes(attr: u16) -> Vec<i32> {
    ATTR_CODES
        .iter()
        .filter_map(|(mask, code)| (attr & mask != 0).then_some(*code))
        .collect()
}

fn sgr_code_text(code: i32) -> String {
    if code < 10 {
        code.to_string()
    } else {
        format!("{}:{}", code / 10, code % 10)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]

enum ColourTarget {
    Foreground,
    Background,
    Underline,
}

const ATTR_CODES: &[(u16, i32)] = &[
    (GridAttr::BRIGHT, 1),
    (GridAttr::DIM, 2),
    (GridAttr::ITALICS, 3),
    (GridAttr::UNDERSCORE, 4),
    (GridAttr::BLINK, 5),
    (GridAttr::REVERSE, 7),
    (GridAttr::HIDDEN, 8),
    (GridAttr::STRIKETHROUGH, 9),
    (GridAttr::UNDERSCORE_2, 42),
    (GridAttr::UNDERSCORE_3, 43),
    (GridAttr::UNDERSCORE_4, 44),
    (GridAttr::UNDERSCORE_5, 45),
    (GridAttr::OVERLINE, 53),
];

#[cfg(test)]
#[path = "renderer/tests.rs"]
mod tests;
