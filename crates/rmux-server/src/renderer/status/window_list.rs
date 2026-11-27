//! Window list segment of the status line: per-window format bodies, window
//! status styles, the plain-run fallback and `status-justify` alignment.

use std::path::Path;

use rmux_core::{
    formats::{FormatContext, FormatVariables},
    style::Style,
    OptionStore,
};
use rmux_proto::OptionName;

use crate::client_format::ClientLoopSnapshot;
use crate::format_runtime::RuntimeFormatContext;
use crate::pane_terminals::HandlerState;

use super::super::apply_runtime_style_overlay;
use super::{render_bar_template, status_job_cache_ttl};

pub(super) fn status_window_format_body(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    state: Option<&HandlerState>,
    socket_path: Option<&Path>,
    base_style: &Style,
    attached_count: usize,
    clients: (
        Option<&crate::client_format::ClientFormatBindings>,
        Option<&[ClientLoopSnapshot]>,
    ),
) -> String {
    let (client, client_loop_snapshots) = clients;
    let session_name = session.name();
    let active_window = session.active_window_index();
    let last_window = session.last_window_index();
    let status_job_ttl = status_job_cache_ttl(options, session_name);
    let mut rendered = String::new();
    let windows = session.windows().collect::<Vec<_>>();

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

        let mut context = FormatContext::new()
            .with_managed_session(session)
            .with_window(*window_index, window, active, last);
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
            runtime = runtime.with_state(state).with_options(options);
        }
        if let Some(pane) = window.active_pane() {
            runtime = runtime.with_pane(pane);
        }
        if let Some(client) = client {
            runtime = client.apply(runtime);
        }
        if let Some(client_loop_snapshots) = client_loop_snapshots {
            runtime = runtime.with_client_loop_snapshots(client_loop_snapshots);
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
        rendered.push_str(&render_bar_template(format, &runtime, status_job_ttl, true));
        rendered.push_str("#[norange list=on default]");

        if position + 1 != windows.len() {
            let separator = options
                .resolve_for_window(
                    session_name,
                    *window_index,
                    OptionName::WindowStatusSeparator,
                )
                .unwrap_or(" ");
            let rendered_separator = render_bar_template(separator, &runtime, status_job_ttl, true);
            if !rendered_separator.is_empty() {
                rendered.push_str(&rendered_separator);
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

pub(super) fn status_justify(value: Option<&str>) -> StatusJustify {
    match value {
        Some("right") => StatusJustify::Right,
        Some("centre" | "center") => StatusJustify::Centre,
        Some("absolute-centre") => StatusJustify::AbsoluteCentre,
        _ => StatusJustify::Left,
    }
}

/// The alignment this value asks the drawing engine for. `absolute-centre`
/// keeps its own name so the list is centred on the whole line rather than on
/// the room left between the two components (tmux.1:5041-5046); the engine
/// already knows it (`StyleAlign::AbsoluteCentre`).
pub(super) fn status_justify_name(value: StatusJustify) -> &'static str {
    match value {
        StatusJustify::Left => "left",
        StatusJustify::Centre => "centre",
        StatusJustify::Right => "right",
        StatusJustify::AbsoluteCentre => "absolute-centre",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusJustify {
    Left,
    Centre,
    Right,
    AbsoluteCentre,
}
