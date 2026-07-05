use std::path::Path;
use std::time::Duration;

use rmux_core::{
    formats::{FormatContext, FormatVariables},
    style::Style,
    text_width as tmux_text_width, truncate_to_width as tmux_truncate_to_width, OptionStore,
    Session, Utf8Config,
};
use rmux_proto::OptionName;

use crate::format_runtime::{
    render_runtime_template, render_runtime_template_preserving_jobs, RuntimeFormatContext,
};
use crate::pane_terminals::HandlerState;

use super::{
    apply_runtime_style_overlay, apply_style_overlay, colour_inherits_base, cursor_position_bytes,
    format_draw_line, parse_option_colour, parse_standalone_style, render_formatted_line,
    FormattedLine, RenderedPrompt,
};

#[path = "status/geometry.rs"]
mod geometry;
#[path = "status/jobs.rs"]
mod jobs;
#[path = "status/message.rs"]
mod message;
#[path = "status/prompt.rs"]
mod prompt;
#[path = "status/runs.rs"]
mod runs;
#[path = "status/status_format.rs"]
mod status_format;

pub(super) use geometry::StatusGeometry;
pub(super) use message::format_status_message_line;
pub(super) use prompt::prompt_status_runs;
pub(super) use runs::{sanitize_status_text, status_runs_width, StatusRun};

use jobs::render_template_with_status_jobs;
use prompt::prompt_status_layout;
use runs::{push_spaces, push_status_run, render_status_runs, truncate_status_runs, StatusStyle};
use status_format::{render_explicit_status_format_line, render_status_format_line_at};

pub(super) struct StatusBarRenderRequest<'a> {
    pub(super) session: &'a Session,
    pub(super) options: &'a OptionStore,
    pub(super) geometry: StatusGeometry,
    pub(super) attached_count: usize,
    pub(super) prompt: Option<&'a RenderedPrompt>,
    pub(super) pane_title: Option<&'a str>,
    pub(super) state: Option<&'a HandlerState>,
    pub(super) key_table: Option<&'a str>,
    pub(super) socket_path: Option<&'a Path>,
}

pub(super) fn render_status_bar(request: StatusBarRenderRequest<'_>) -> Vec<u8> {
    let StatusBarRenderRequest {
        session,
        options,
        geometry,
        attached_count,
        prompt,
        pane_title,
        state,
        key_table,
        socket_path,
    } = request;
    let Some(status_y) = geometry.status_y else {
        return Vec::new();
    };
    let width = usize::from(geometry.terminal_size.cols);
    if width == 0 {
        return Vec::new();
    }

    let mut frame = Vec::new();
    let lines = status_bar_lines_with_pane_title(
        session,
        options,
        geometry.terminal_size.cols,
        geometry.status_lines,
        StatusLineContext {
            attached_count,
            pane_title,
            state,
            key_table,
            socket_path,
        },
    );
    for (index, line) in lines.iter().enumerate() {
        let Ok(index) = u16::try_from(index) else {
            break;
        };
        if let Some(line_y) = geometry.status_line_y(index) {
            render_formatted_line(&mut frame, 0, line_y, line);
        }
    }

    if let Some(prompt) = prompt {
        let message_line =
            status_message_line_index(session.name(), options, geometry.status_lines);
        let line_y = geometry.status_line_y(message_line).unwrap_or(status_y);
        let layout = prompt_status_layout(session, options, geometry.terminal_size.cols, prompt);
        frame.extend_from_slice(render_status_runs(line_y, &layout.runs).as_slice());
        frame.extend_from_slice(cursor_position_bytes(line_y, layout.cursor_x).as_slice());
    }
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
    let context = active_format_context(session, attached_count, None, None);
    let left_template = options
        .resolve(Some(session_name), OptionName::StatusLeft)
        .unwrap_or_default();
    let right_template = options
        .resolve(Some(session_name), OptionName::StatusRight)
        .unwrap_or_default();
    let left_limit = option_usize(options, session_name, OptionName::StatusLeftLength);
    let right_limit = option_usize(options, session_name, OptionName::StatusRightLength);
    let status_job_ttl = status_job_cache_ttl(options, session_name);
    let mut runtime = RuntimeFormatContext::new(context)
        .with_options(options)
        .with_session(session)
        .with_window(session.active_window_index(), session.window());
    if let Some(pane) = session.window().active_pane() {
        runtime = runtime.with_pane(pane);
    }
    let left = tmux_truncate_to_width(
        &render_status_template_jobs(left_template, &runtime, status_job_ttl),
        left_limit.min(width),
        &utf8_config,
    );
    let left_width = tmux_text_width(&left, &utf8_config);
    let right_room = width.saturating_sub(left_width);
    let right = tmux_truncate_to_width(
        &render_status_template_jobs(right_template, &runtime, status_job_ttl),
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

fn active_format_context(
    session: &Session,
    attached_count: usize,
    key_table: Option<&str>,
    socket_path: Option<&Path>,
) -> FormatContext {
    let mut context = FormatContext::from_session(session)
        .with_session_attached(attached_count)
        .with_window(session.active_window_index(), session.window(), true, false);
    if let Some(pane) = session.window().active_pane() {
        context = context.with_window_pane(session.window(), pane);
    }
    // The status bar is rendered per session, so client_prefix/client_key_table
    // reflect the (representative) attached client's runtime key table. When the
    // prefix has been pressed the client's key table is "prefix"; otherwise it
    // falls back to the root table. Without this the #{?client_prefix,...} and
    // #{client_key_table} format variables always expanded to empty.
    let prefix_active = key_table == Some("prefix");
    context = context
        .with_named_value("client_key_table", key_table.unwrap_or("root"))
        .with_named_value("client_prefix", if prefix_active { "1" } else { "0" });
    if let Some(socket_path) = socket_path {
        context =
            context.with_named_value("socket_path", socket_path.to_string_lossy().into_owned());
    }
    context
}

pub(super) fn status_bar_lines(
    session: &Session,
    options: &OptionStore,
    columns: u16,
    attached_count: usize,
    status_lines: u16,
) -> Vec<FormattedLine> {
    status_bar_lines_with_pane_title(
        session,
        options,
        columns,
        status_lines,
        StatusLineContext {
            attached_count,
            ..StatusLineContext::default()
        },
    )
}

pub(super) fn status_message_y(
    session: &Session,
    options: &OptionStore,
    geometry: StatusGeometry,
) -> Option<u16> {
    let line = status_message_line_index(session.name(), options, geometry.status_lines);
    geometry.status_line_y(line)
}

#[derive(Clone, Copy, Default)]
struct StatusLineContext<'a> {
    attached_count: usize,
    pane_title: Option<&'a str>,
    state: Option<&'a HandlerState>,
    key_table: Option<&'a str>,
    socket_path: Option<&'a Path>,
}

fn status_bar_lines_with_pane_title(
    session: &Session,
    options: &OptionStore,
    columns: u16,
    status_lines: u16,
    context: StatusLineContext<'_>,
) -> Vec<FormattedLine> {
    (0..status_lines)
        .map(|line| status_bar_line_at(session, options, columns, context, line))
        .collect()
}

fn status_bar_line_at(
    session: &Session,
    options: &OptionStore,
    columns: u16,
    context: StatusLineContext<'_>,
    line_index: u16,
) -> FormattedLine {
    if line_index == 0 {
        return status_bar_line_with_pane_title(session, options, columns, context);
    }

    let width = usize::from(columns);
    let utf8_config = Utf8Config::from_options(options);
    let session_name = session.name();
    let base_style = resolved_status_style(options, session_name);
    let mut runtime = RuntimeFormatContext::new(active_format_context(
        session,
        context.attached_count,
        context.key_table,
        context.socket_path,
    ))
    .with_options(options)
    .with_session(session)
    .with_window(session.active_window_index(), session.window());
    if let Some(state) = context.state {
        runtime = runtime.with_state(state);
    }
    if let Some(pane) = session.window().active_pane() {
        runtime = runtime.with_pane(pane);
    }
    if let Some(pane_title) = context.pane_title {
        runtime = runtime.with_named_value("pane_title", pane_title);
    }

    render_status_format_line_at(
        session_name,
        options,
        &runtime,
        &base_style,
        width,
        &utf8_config,
        usize::from(line_index),
    )
    .unwrap_or_else(|| format_draw_line("", &base_style, width, &utf8_config))
}

pub(super) fn status_message_line_index(
    session_name: &rmux_proto::SessionName,
    options: &OptionStore,
    status_lines: u16,
) -> u16 {
    if status_lines == 0 {
        return 0;
    }
    options
        .resolve(Some(session_name), OptionName::MessageLine)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0)
        .min(status_lines.saturating_sub(1))
}

fn status_bar_line_with_pane_title(
    session: &Session,
    options: &OptionStore,
    columns: u16,
    context: StatusLineContext<'_>,
) -> FormattedLine {
    let width = usize::from(columns);
    let utf8_config = Utf8Config::from_options(options);
    let session_name = session.name();
    let base_style = resolved_status_style(options, session_name);
    let mut runtime = RuntimeFormatContext::new(active_format_context(
        session,
        context.attached_count,
        context.key_table,
        context.socket_path,
    ))
    .with_options(options)
    .with_session(session)
    .with_window(session.active_window_index(), session.window());
    // Threading the handler state in lets runtime variables such as
    // #{pane_in_mode} (the copy-mode indicator) resolve in the status bar.
    if let Some(state) = context.state {
        runtime = runtime.with_state(state);
    }
    if let Some(pane) = session.window().active_pane() {
        runtime = runtime.with_pane(pane);
    }
    if let Some(pane_title) = context.pane_title {
        runtime = runtime.with_named_value("pane_title", pane_title);
    }
    if let Some(line) = render_explicit_status_format_line(
        session_name,
        options,
        &runtime,
        &base_style,
        width,
        &utf8_config,
    ) {
        return line;
    }

    let left_template = options
        .resolve(Some(session_name), OptionName::StatusLeft)
        .unwrap_or_default();
    let right_template = options
        .resolve(Some(session_name), OptionName::StatusRight)
        .unwrap_or_default();
    let left_limit = option_usize(options, session_name, OptionName::StatusLeftLength);
    let right_limit = option_usize(options, session_name, OptionName::StatusRightLength);
    let status_job_ttl = status_job_cache_ttl(options, session_name);
    let left = sanitize_status_text(tmux_truncate_to_width(
        &render_status_template_jobs(left_template, &runtime, status_job_ttl),
        left_limit.min(width),
        &utf8_config,
    ));
    let left_width = tmux_text_width(&left, &utf8_config);
    let right_room = width.saturating_sub(left_width);
    let right = sanitize_status_text(tmux_truncate_to_width(
        &render_status_template_jobs(right_template, &runtime, status_job_ttl),
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
        context.state,
        context.socket_path,
        &base_style,
        context.attached_count,
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

fn render_status_template_jobs(
    template: &str,
    runtime: &RuntimeFormatContext<'_>,
    cache_ttl: Duration,
) -> String {
    let profile = if template.contains("#(") {
        runtime.status_job_profile()
    } else {
        None
    };
    render_status_template_jobs_with_profile(template, runtime, profile.as_ref(), cache_ttl)
}

pub(super) fn render_status_template_jobs_with_profile<V>(
    template: &str,
    variables: &V,
    profile: Option<&crate::terminal::TerminalProfile>,
    cache_ttl: Duration,
) -> String
where
    V: rmux_core::formats::FormatVariables + ?Sized,
{
    render_template_with_status_jobs(
        template,
        profile,
        cache_ttl,
        |command| render_runtime_template(command, variables, true),
        |prepared| render_runtime_template_preserving_jobs(prepared, variables, true),
    )
}

fn status_job_cache_ttl(options: &OptionStore, session_name: &rmux_proto::SessionName) -> Duration {
    let seconds = option_usize(options, session_name, OptionName::StatusInterval).max(1);
    Duration::from_secs(u64::try_from(seconds).unwrap_or(u64::MAX))
}

fn status_window_format_body(
    session: &Session,
    options: &OptionStore,
    state: Option<&HandlerState>,
    socket_path: Option<&Path>,
    base_style: &Style,
    attached_count: usize,
) -> String {
    let session_name = session.name();
    let active_window = session.active_window_index();
    let last_window = session.last_window_index();
    let status_job_ttl = status_job_cache_ttl(options, session_name);
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
        if let Some(socket_path) = socket_path {
            context =
                context.with_named_value("socket_path", socket_path.to_string_lossy().into_owned());
        }
        if let Some(pane) = window.active_pane() {
            context = context.with_window_pane(window, pane);
        }
        let mut runtime = RuntimeFormatContext::new(context)
            .with_options(options)
            .with_session(session)
            .with_window(*window_index, window);
        if let Some(state) = state {
            runtime = runtime.with_state(state);
        }
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
        rendered.push_str(&sanitize_status_text(render_status_template_jobs(
            format,
            &runtime,
            status_job_ttl,
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
            let rendered_separator =
                render_status_template_jobs(separator, &runtime, status_job_ttl);
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
    {
        if !colour_inherits_base(colour) {
            style.cell.fg = colour;
        }
    }
    if let Some(colour) =
        parse_option_colour(options.resolve(Some(session_name), OptionName::StatusBg))
    {
        if !colour_inherits_base(colour) {
            style.cell.bg = colour;
        }
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
