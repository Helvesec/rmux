use rmux_core::{
    formats::{FormatContext, FormatVariables},
    style::Style,
    text_width as tmux_text_width, truncate_to_width as tmux_truncate_to_width, OptionStore,
    Session, Utf8Config,
};
use rmux_proto::OptionName;

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};

use super::{
    apply_runtime_style_overlay, apply_style_overlay, colour_inherits_base, cursor_position_bytes,
    format_draw_line, parse_option_colour, parse_standalone_style, render_formatted_line,
    FormattedLine, RenderedPrompt,
};

#[path = "status/geometry.rs"]
mod geometry;
#[path = "status/message.rs"]
mod message;
#[path = "status/prompt.rs"]
mod prompt;
#[path = "status/runs.rs"]
mod runs;

pub(super) use geometry::StatusGeometry;
pub(super) use message::format_status_message_line;
pub(super) use prompt::prompt_status_runs;
pub(super) use runs::{sanitize_status_text, status_runs_width, StatusRun};

use prompt::prompt_status_layout;
use runs::{push_spaces, push_status_run, render_status_runs, truncate_status_runs, StatusStyle};

pub(super) fn render_status_bar(
    session: &Session,
    options: &OptionStore,
    geometry: StatusGeometry,
    attached_count: usize,
    prompt: Option<&RenderedPrompt>,
    pane_title: Option<&str>,
) -> Vec<u8> {
    let Some(status_y) = geometry.status_y else {
        return Vec::new();
    };
    let width = usize::from(geometry.terminal_size.cols);
    if width == 0 {
        return Vec::new();
    }

    if let Some(prompt) = prompt {
        let layout = prompt_status_layout(session, options, geometry.terminal_size.cols, prompt);
        let mut frame = render_status_runs(status_y, &layout.runs);
        frame.extend_from_slice(cursor_position_bytes(status_y, layout.cursor_x).as_slice());
        return frame;
    }

    let line = status_bar_line_with_pane_title(
        session,
        options,
        geometry.terminal_size.cols,
        attached_count,
        pane_title,
    );
    let mut frame = Vec::new();
    render_formatted_line(&mut frame, 0, status_y, &line);
    frame
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn status_bar_runs(
    session: &Session,
    options: &OptionStore,
    columns: u16,
    attached_count: usize,
) -> Vec<StatusRun> {
    let width = usize::from(columns);
    let utf8_config = Utf8Config::from_options(options);
    let session_name = session.name();
    let base_style = resolved_status_style(options, session_name);
    let left_style = apply_style_overlay(
        &base_style,
        options.resolve(Some(session_name), OptionName::StatusLeftStyle),
    );
    let right_style = apply_style_overlay(
        &base_style,
        options.resolve(Some(session_name), OptionName::StatusRightStyle),
    );
    let context = active_format_context(session, attached_count);
    let left_template = options
        .resolve(Some(session_name), OptionName::StatusLeft)
        .unwrap_or_default();
    let right_template = options
        .resolve(Some(session_name), OptionName::StatusRight)
        .unwrap_or_default();
    let left_limit = option_usize(options, session_name, OptionName::StatusLeftLength);
    let right_limit = option_usize(options, session_name, OptionName::StatusRightLength);
    let mut runtime = RuntimeFormatContext::new(context)
        .with_options(options)
        .with_session(session)
        .with_window(session.active_window_index(), session.window());
    if let Some(pane) = session.window().active_pane() {
        runtime = runtime.with_pane(pane);
    }
    let left = tmux_truncate_to_width(
        &render_runtime_template(left_template, &runtime, true),
        left_limit.min(width),
        &utf8_config,
    );
    let left_width = tmux_text_width(&left, &utf8_config);
    let right_room = width.saturating_sub(left_width);
    let right = tmux_truncate_to_width(
        &render_runtime_template(right_template, &runtime, true),
        right_limit.min(right_room),
        &utf8_config,
    );
    let right_width = tmux_text_width(&right, &utf8_config);
    let window_area_width = width.saturating_sub(left_width).saturating_sub(right_width);

    let mut runs = Vec::new();
    push_status_run(&mut runs, left, left_style);

    let window_runs = aligned_status_window_runs(
        session,
        options,
        window_area_width,
        base_style.clone(),
        &utf8_config,
    );
    runs.extend(window_runs);

    push_status_run(&mut runs, right, right_style);
    let rendered_width = status_runs_width(&runs, &utf8_config);
    push_spaces(&mut runs, width.saturating_sub(rendered_width), base_style);
    runs
}

fn active_format_context(session: &Session, attached_count: usize) -> FormatContext {
    let mut context = FormatContext::from_session(session)
        .with_session_attached(attached_count)
        .with_window(session.active_window_index(), session.window(), true, false);
    if let Some(pane) = session.window().active_pane() {
        context = context.with_window_pane(session.window(), pane);
    }
    context
}

pub(super) fn status_bar_line(
    session: &Session,
    options: &OptionStore,
    columns: u16,
    attached_count: usize,
) -> FormattedLine {
    status_bar_line_with_pane_title(session, options, columns, attached_count, None)
}

fn status_bar_line_with_pane_title(
    session: &Session,
    options: &OptionStore,
    columns: u16,
    attached_count: usize,
    pane_title: Option<&str>,
) -> FormattedLine {
    let width = usize::from(columns);
    let utf8_config = Utf8Config::from_options(options);
    let session_name = session.name();
    let base_style = resolved_status_style(options, session_name);
    let mut runtime = RuntimeFormatContext::new(active_format_context(session, attached_count))
        .with_options(options)
        .with_session(session)
        .with_window(session.active_window_index(), session.window());
    if let Some(pane) = session.window().active_pane() {
        runtime = runtime.with_pane(pane);
    }
    if let Some(pane_title) = pane_title {
        runtime = runtime.with_named_value("pane_title", pane_title);
    }

    let left_template = options
        .resolve(Some(session_name), OptionName::StatusLeft)
        .unwrap_or_default();
    let right_template = options
        .resolve(Some(session_name), OptionName::StatusRight)
        .unwrap_or_default();
    let left_limit = option_usize(options, session_name, OptionName::StatusLeftLength);
    let right_limit = option_usize(options, session_name, OptionName::StatusRightLength);
    let left = sanitize_status_text(tmux_truncate_to_width(
        &render_runtime_template(left_template, &runtime, true),
        left_limit.min(width),
        &utf8_config,
    ));
    let left_width = tmux_text_width(&left, &utf8_config);
    let right_room = width.saturating_sub(left_width);
    let right = sanitize_status_text(tmux_truncate_to_width(
        &render_runtime_template(right_template, &runtime, true),
        right_limit.min(right_room),
        &utf8_config,
    ));

    let left_style = apply_runtime_style_overlay(
        &base_style,
        options.resolve(Some(session_name), OptionName::StatusLeftStyle),
        &runtime,
    );
    let right_style = apply_runtime_style_overlay(
        &base_style,
        options.resolve(Some(session_name), OptionName::StatusRightStyle),
        &runtime,
    );

    let mut expanded = String::new();
    if !left.is_empty() {
        expanded.push_str(&format!(
            "#[align=left range=left {}]{}#[norange default]",
            rmux_core::style_tostring(&left_style),
            left
        ));
    }

    expanded.push_str(&format!(
        "#[list=on align={}]",
        status_justify_name(status_justify(
            options.resolve(Some(session_name), OptionName::StatusJustify)
        ))
    ));
    expanded.push_str(&status_window_format_body(
        session,
        options,
        &base_style,
        attached_count,
    ));
    expanded.push_str("#[nolist]");

    if !right.is_empty() {
        expanded.push_str(&format!(
            "#[align=right range=right {}]{}#[norange default]",
            rmux_core::style_tostring(&right_style),
            right
        ));
    }

    format_draw_line(&expanded, &base_style, width, &utf8_config)
}

fn status_window_format_body(
    session: &Session,
    options: &OptionStore,
    base_style: &Style,
    attached_count: usize,
) -> String {
    let session_name = session.name();
    let active_window = session.active_window_index();
    let last_window = session.last_window_index();
    let mut rendered = String::new();
    let windows = session
        .windows()
        .iter()
        .map(|(window_index, window)| (*window_index, window))
        .collect::<Vec<_>>();

    for (position, (window_index, window)) in windows.iter().enumerate() {
        let active = *window_index == active_window;
        let last = Some(*window_index) == last_window;
        let format_option = if active {
            OptionName::WindowStatusCurrentFormat
        } else {
            OptionName::WindowStatusFormat
        };
        let format = options
            .resolve_for_window(session_name, *window_index, format_option)
            .unwrap_or_default();

        let mut context =
            FormatContext::from_session(session).with_window(*window_index, window, active, last);
        context = context.with_session_attached(attached_count);
        if let Some(pane) = window.active_pane() {
            context = context.with_window_pane(window, pane);
        }
        let mut runtime = RuntimeFormatContext::new(context)
            .with_options(options)
            .with_session(session)
            .with_window(*window_index, window);
        if let Some(pane) = window.active_pane() {
            runtime = runtime.with_pane(pane);
        }

        let style = resolved_window_status_style(
            base_style,
            options,
            session_name,
            *window_index,
            active,
            last,
            &runtime,
        );
        rendered.push_str(&format!(
            "#[range=window|{}{} {}]",
            window.id().as_u32(),
            if active { " list=focus" } else { "" },
            rmux_core::style_tostring(&style)
        ));
        rendered.push_str(&sanitize_status_text(render_runtime_template(
            format, &runtime, true,
        )));
        rendered.push_str("#[norange list=on default]");

        if position + 1 != windows.len() {
            let separator = options
                .resolve_for_window(
                    session_name,
                    *window_index,
                    OptionName::WindowStatusSeparator,
                )
                .unwrap_or(" ");
            let rendered_separator = render_runtime_template(separator, &runtime, true);
            if !rendered_separator.is_empty() {
                rendered.push_str(&sanitize_status_text(rendered_separator));
            }
        }
    }

    rendered
}

fn resolved_window_status_style(
    base_style: &Style,
    options: &OptionStore,
    session_name: &rmux_proto::SessionName,
    window_index: u32,
    active: bool,
    last: bool,
    runtime: &RuntimeFormatContext<'_>,
) -> Style {
    let primary = if active {
        OptionName::WindowStatusCurrentStyle
    } else {
        OptionName::WindowStatusStyle
    };
    let mut style = apply_runtime_style_overlay(
        base_style,
        options.resolve_for_window(session_name, window_index, primary),
        runtime,
    );
    if last {
        style = apply_runtime_style_overlay(
            &style,
            options.resolve_for_window(
                session_name,
                window_index,
                OptionName::WindowStatusLastStyle,
            ),
            runtime,
        );
    }
    let has_bell = runtime.format_value_by_name("window_bell_flag").as_deref() == Some("1");
    let has_activity = runtime
        .format_value_by_name("window_activity_flag")
        .as_deref()
        == Some("1");
    let has_silence = runtime
        .format_value_by_name("window_silence_flag")
        .as_deref()
        == Some("1");
    if has_bell {
        style = apply_runtime_style_overlay(
            &style,
            options.resolve_for_window(
                session_name,
                window_index,
                OptionName::WindowStatusBellStyle,
            ),
            runtime,
        );
    } else if has_activity || has_silence {
        style = apply_runtime_style_overlay(
            &style,
            options.resolve_for_window(
                session_name,
                window_index,
                OptionName::WindowStatusActivityStyle,
            ),
            runtime,
        );
    }
    style
}

#[cfg_attr(not(test), allow(dead_code))]
fn aligned_status_window_runs(
    session: &Session,
    options: &OptionStore,
    width: usize,
    base_style: StatusStyle,
    utf8_config: &Utf8Config,
) -> Vec<StatusRun> {
    if width == 0 {
        return Vec::new();
    }

    let runs = truncate_status_runs(
        &status_window_runs(session, options, base_style.clone()),
        width,
        utf8_config,
    );
    let run_width = status_runs_width(&runs, utf8_config);
    let extra = width.saturating_sub(run_width);
    let leading =
        match status_justify(options.resolve(Some(session.name()), OptionName::StatusJustify)) {
            StatusJustify::Left => 0,
            StatusJustify::Centre => extra / 2,
            StatusJustify::Right => extra,
        };
    let trailing = extra.saturating_sub(leading);
    let mut aligned = Vec::new();
    push_spaces(&mut aligned, leading, base_style.clone());
    aligned.extend(runs);
    push_spaces(&mut aligned, trailing, base_style);
    aligned
}

#[cfg_attr(not(test), allow(dead_code))]
fn status_window_runs(
    session: &Session,
    options: &OptionStore,
    base_style: StatusStyle,
) -> Vec<StatusRun> {
    let session_name = session.name();
    let active_window = session.active_window_index();
    let last_window = session.last_window_index();
    let mut runs = Vec::new();

    for (window_index, window) in session.windows() {
        if !runs.is_empty() {
            push_status_run(&mut runs, " ".to_owned(), base_style.clone());
        }

        let active = *window_index == active_window;
        let last = Some(*window_index) == last_window;
        let format_option = if active {
            OptionName::WindowStatusCurrentFormat
        } else {
            OptionName::WindowStatusFormat
        };
        let style_option = if active {
            OptionName::WindowStatusCurrentStyle
        } else {
            OptionName::WindowStatusStyle
        };
        let format = options
            .resolve_for_window(session_name, *window_index, format_option)
            .unwrap_or_default();
        let style = apply_style_overlay(
            &base_style,
            options.resolve_for_window(session_name, *window_index, style_option),
        );
        let mut context =
            FormatContext::from_session(session).with_window(*window_index, window, active, last);
        if let Some(pane) = window.active_pane() {
            context = context.with_window_pane(window, pane);
        }

        let mut runtime = RuntimeFormatContext::new(context)
            .with_options(options)
            .with_session(session)
            .with_window(*window_index, window);
        if let Some(pane) = window.active_pane() {
            runtime = runtime.with_pane(pane);
        }

        push_status_run(
            &mut runs,
            render_runtime_template(format, &runtime, true),
            style,
        );
    }

    runs
}

fn resolved_status_style(
    options: &OptionStore,
    session_name: &rmux_proto::SessionName,
) -> StatusStyle {
    let mut style =
        parse_standalone_style(options.resolve(Some(session_name), OptionName::StatusStyle));

    if let Some(colour) =
        parse_option_colour(options.resolve(Some(session_name), OptionName::StatusFg))
        && !colour_inherits_base(colour) {
            style.cell.fg = colour;
        }
    if let Some(colour) =
        parse_option_colour(options.resolve(Some(session_name), OptionName::StatusBg))
        && !colour_inherits_base(colour) {
            style.cell.bg = colour;
        }

    style
}

fn option_usize(
    options: &OptionStore,
    session_name: &rmux_proto::SessionName,
    option: OptionName,
) -> usize {
    options
        .resolve(Some(session_name), option)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}

fn status_justify(value: Option<&str>) -> StatusJustify {
    match value {
        Some("right") => StatusJustify::Right,
        Some("centre" | "center" | "absolute-centre") => StatusJustify::Centre,
        _ => StatusJustify::Left,
    }
}

fn status_justify_name(value: StatusJustify) -> &'static str {
    match value {
        StatusJustify::Left => "left",
        StatusJustify::Centre => "centre",
        StatusJustify::Right => "right",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusJustify {
    Left,
    Centre,
    Right,
}
