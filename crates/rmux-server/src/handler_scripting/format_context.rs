use std::path::{Path, PathBuf};

use rmux_core::{
    command_parser::CommandParser,
    formats::{
        FormatContext, FormatVariables, FORMAT_VARIABLES,
        TMUX_FORMAT_TABLE_NAMES as RMUX_FORMAT_TABLE_NAMES,
    },
};
use rmux_proto::{RmuxError, Target};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::{session_not_found, HandlerState};

pub(super) fn collect_parse_time_values(
    format_context: &dyn FormatVariables,
) -> Vec<(String, String)> {
    let mut values = Vec::new();

    if let Some(value) = format_context.format_value_by_name("current_file") {
        values.push(("current_file".to_owned(), value));
    }

    for variable in FORMAT_VARIABLES {
        if let Some(value) = format_context.format_value(variable) {
            values.push((variable.name().to_owned(), value));
        }
    }

    for name in RMUX_FORMAT_TABLE_NAMES {
        if let Some(value) = format_context.format_value_by_name(name) {
            values.push(((*name).to_owned(), value));
        }
    }

    values
}

pub(super) fn parser_with_parse_time_context(
    mut parser: CommandParser,
    format_context: &dyn FormatVariables,
) -> CommandParser {
    for (name, value) in collect_parse_time_values(format_context) {
        parser = parser.with_format_value(&name, value);
    }

    parser
}

pub(in super::super) fn format_context_for_target<'a>(
    state: &'a HandlerState,
    target: &Target,
    attached_count: usize,
) -> Result<RuntimeFormatContext<'a>, RmuxError> {
    let session_name = target.session_name();
    let session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| session_not_found(session_name))?;
    let active_window = session.active_window_index();
    let last_window = session.last_window_index();

    match target {
        Target::Session(_) => {
            let window = session.window();
            let mut context = FormatContext::from_session(session)
                .with_session_attached(attached_count)
                .with_window(active_window, window, true, false);
            if let Some(pane) = window.active_pane() {
                context = context.with_window_pane(window, pane);
            }
            let mut runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_session(session)
                .with_window(active_window, window);
            if let Some(pane) = window.active_pane() {
                runtime = runtime.with_pane(pane);
            }
            Ok(runtime)
        }
        Target::Window(target) => {
            let window_index = target.window_index();
            let window = session.window_at(window_index).ok_or_else(|| {
                RmuxError::invalid_target(
                    target.to_string(),
                    "window index does not exist in session",
                )
            })?;
            let mut context = FormatContext::from_session(session)
                .with_session_attached(attached_count)
                .with_window(
                    window_index,
                    window,
                    window_index == active_window,
                    Some(window_index) == last_window,
                );
            if let Some(pane) = window.active_pane() {
                context = context.with_window_pane(window, pane);
            }
            let mut runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_session(session)
                .with_window(window_index, window);
            if let Some(pane) = window.active_pane() {
                runtime = runtime.with_pane(pane);
            }
            Ok(runtime)
        }
        Target::Pane(target) => {
            let window_index = target.window_index();
            let pane_index = target.pane_index();
            let window = session.window_at(window_index).ok_or_else(|| {
                RmuxError::invalid_target(
                    format!("{}:{window_index}", target.session_name()),
                    "window index does not exist in session",
                )
            })?;
            let pane = window.pane(pane_index).ok_or_else(|| {
                RmuxError::invalid_target(
                    target.to_string(),
                    "pane index does not exist in session",
                )
            })?;
            let context = FormatContext::from_session(session)
                .with_session_attached(attached_count)
                .with_window(
                    window_index,
                    window,
                    window_index == active_window,
                    Some(window_index) == last_window,
                )
                .with_pane(pane, pane_index == window.active_pane_index());
            Ok(RuntimeFormatContext::new(context)
                .with_state(state)
                .with_session(session)
                .with_window(window_index, window)
                .with_pane(pane))
        }
    }
}

pub(in super::super) fn with_server_format_values<'a>(
    context: RuntimeFormatContext<'a>,
    socket_path: &Path,
) -> RuntimeFormatContext<'a> {
    context.with_named_value("socket_path", socket_path.to_string_lossy().into_owned())
}

pub(in super::super) fn global_format_context<'a>(
    state: &'a HandlerState,
    socket_path: &Path,
) -> RuntimeFormatContext<'a> {
    with_server_format_values(
        RuntimeFormatContext::new(FormatContext::new()).with_state(state),
        socket_path,
    )
}

pub(in super::super) fn format_context_for_target_with_server_values<'a>(
    state: &'a HandlerState,
    target: &Target,
    attached_count: usize,
    socket_path: &Path,
) -> Result<RuntimeFormatContext<'a>, RmuxError> {
    format_context_for_target(state, target, attached_count)
        .map(|context| with_server_format_values(context, socket_path))
}

pub(in super::super) fn render_start_directory_template(
    state: &HandlerState,
    target: &Target,
    attached_count: usize,
    start_directory: Option<PathBuf>,
) -> Result<Option<PathBuf>, RmuxError> {
    let Some(start_directory) = start_directory else {
        return Ok(None);
    };
    let template = start_directory.as_os_str().to_string_lossy();
    if !template.contains("#{") {
        return Ok(Some(start_directory));
    }

    let context = format_context_for_target(state, target, attached_count)?;
    Ok(Some(PathBuf::from(render_runtime_template(
        &template, &context, false,
    ))))
}
