use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use rmux_core::{
    command_parser::CommandParser,
    formats::{
        format_variable_descriptor, tmux_format_variable_descriptors, FormatContext,
        FormatVariables, FORMAT_VARIABLES,
    },
};
use rmux_proto::{RmuxError, Target};

use crate::client_format::ClientLoopSnapshot;
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::{session_not_found, HandlerState};
use crate::pane_transcript::PaneTranscriptReach;
use rmux_core::Pane;

/// The parse-time values of one format context, captured at most once and only
/// for a source that can read them.
///
/// A parser reads these values only to expand `%if` and `%elif` conditions,
/// and capturing them costs one by-name read per table name. Every caller
/// reaches them through [`Self::for_source`] or [`Self::seed_parser`], which
/// name the source the values are for, so no caller can capture for a source
/// that cannot read them.
pub(super) struct ParseTimeFormats<'c> {
    context: &'c dyn FormatVariables,
    captured: OnceCell<Vec<(String, String)>>,
}

impl<'c> ParseTimeFormats<'c> {
    pub(super) fn of(context: &'c dyn FormatVariables) -> Self {
        Self {
            context,
            captured: OnceCell::new(),
        }
    }

    /// Returns the values a parse of `source` can read: every captured value
    /// when it holds a directive, and none, without capturing, otherwise.
    pub(super) fn for_source(&self, source: &str) -> &[(String, String)] {
        if !CommandParser::source_reads_parse_time_formats(source) {
            return &[];
        }
        self.captured
            .get_or_init(|| collect_parse_time_values(self.context))
    }

    /// Adds to `parser` the values a parse of `source` can read.
    pub(super) fn seed_parser(&self, mut parser: CommandParser, source: &str) -> CommandParser {
        for (name, value) in self.for_source(source) {
            parser = parser.with_format_value(name, value.clone());
        }
        parser
    }
}

fn collect_parse_time_values(format_context: &dyn FormatVariables) -> Vec<(String, String)> {
    #[cfg(test)]
    PARSE_TIME_CAPTURES.with(|captures| captures.set(captures.get() + 1));
    let mut values = Vec::new();

    if let Some(value) = format_context.format_value_by_name("current_file") {
        values.push(("current_file".to_owned(), value));
    }

    for variable in FORMAT_VARIABLES {
        let descriptor =
            format_variable_descriptor(variable.name()).expect("FormatVariable has a descriptor");
        if let Some(value) = format_context.format_value(variable) {
            values.push((descriptor.name().to_owned(), value));
        }
    }

    for descriptor in tmux_format_variable_descriptors() {
        if let Some(value) = format_context.format_value_by_name(descriptor.name()) {
            values.push((descriptor.name().to_owned(), value));
        }
    }

    values
}

#[cfg(test)]
thread_local! {
    static PARSE_TIME_CAPTURES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts the parse-time captures made on this thread, by any path, so a test
/// can prove that a caller skipped one.
#[cfg(test)]
pub(super) fn parse_time_captures_on_this_thread() -> usize {
    PARSE_TIME_CAPTURES.with(std::cell::Cell::get)
}

/// The format context of `target` for a path that only reaches its pane:
/// hooks, timers, footers, prompts and the commands that act on the pane.
pub(in super::super) fn format_context_for_target<'a>(
    state: &'a HandlerState,
    target: &Target,
    attached_count: usize,
) -> Result<RuntimeFormatContext<'a>, RmuxError> {
    format_context_reaching_target(state, target, attached_count, PaneTranscriptReach::Reached)
}

/// [`format_context_for_target`] for a command whose format reads the pane its
/// target names, as display-message does: if-shell -F and run-shell. Under a
/// request boundary a poisoned transcript answers "pane %N unavailable".
pub(in super::super) fn format_context_for_named_target<'a>(
    state: &'a HandlerState,
    target: &Target,
    attached_count: usize,
    socket_path: &Path,
) -> Result<RuntimeFormatContext<'a>, RmuxError> {
    format_context_reaching_target(state, target, attached_count, PaneTranscriptReach::Named)
        .map(|context| with_server_format_values(context, socket_path))
}

fn with_target_pane<'a>(
    runtime: RuntimeFormatContext<'a>,
    pane: &'a Pane,
    reach: PaneTranscriptReach,
) -> RuntimeFormatContext<'a> {
    match reach {
        PaneTranscriptReach::Named => runtime.with_named_pane(pane),
        PaneTranscriptReach::Reached => runtime.with_pane(pane),
    }
}

fn format_context_reaching_target<'a>(
    state: &'a HandlerState,
    target: &Target,
    attached_count: usize,
    reach: PaneTranscriptReach,
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
            let mut context = FormatContext::new()
                .with_managed_session(session)
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
                runtime = with_target_pane(runtime, pane, reach);
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
            let mut context = FormatContext::new()
                .with_managed_session(session)
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
                runtime = with_target_pane(runtime, pane, reach);
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
            let context = FormatContext::new()
                .with_managed_session(session)
                .with_session_attached(attached_count)
                .with_window(
                    window_index,
                    window,
                    window_index == active_window,
                    Some(window_index) == last_window,
                )
                .with_pane(pane, pane_index == window.active_pane_index());
            let runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_session(session)
                .with_window(window_index, window);
            Ok(with_target_pane(runtime, pane, reach))
        }
    }
}

pub(in super::super) fn with_server_format_values<'a>(
    context: RuntimeFormatContext<'a>,
    socket_path: &Path,
) -> RuntimeFormatContext<'a> {
    context.with_named_value("socket_path", socket_path.to_string_lossy().into_owned())
}

pub(in super::super) fn apply_client_loop_context<'a>(
    mut runtime: RuntimeFormatContext<'a>,
    client_loop_snapshots: Option<&'a [ClientLoopSnapshot]>,
    client_name: Option<&str>,
) -> RuntimeFormatContext<'a> {
    if let Some(client_loop_snapshots) = client_loop_snapshots {
        runtime = runtime.with_client_loop_snapshots(client_loop_snapshots);
        if let Some(client_name) = client_name {
            if let Some(client) = client_loop_snapshots
                .iter()
                .find(|client| client.name() == client_name)
            {
                return client.bindings.apply(runtime);
            }
        }
    }
    if let Some(client_name) = client_name {
        runtime.with_named_value("client_name", client_name.to_owned())
    } else {
        runtime
    }
}

pub(in super::super) fn client_name_for_pid(
    client_loop_snapshots: &[ClientLoopSnapshot],
    pid: u32,
) -> Option<&str> {
    let pid = pid.to_string();
    client_loop_snapshots
        .iter()
        .find(|client| client.bindings.pid == pid)
        .map(ClientLoopSnapshot::name)
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

/// Expands one command argument as a format against `target`, as tmux's
/// `format_single_from_target` does for an `args_*_and_expand` value: no
/// strftime pass, with the requesting client's loop context.
pub(in super::super) fn render_target_argument(
    state: &HandlerState,
    target: &Target,
    attached_count: usize,
    argument: &str,
    client_loop_snapshots: Option<&[ClientLoopSnapshot]>,
    client_name: Option<&str>,
) -> Result<String, RmuxError> {
    if !argument.contains('#') {
        return Ok(argument.to_owned());
    }
    let context = format_context_for_target(state, target, attached_count)?;
    let context = apply_client_loop_context(context, client_loop_snapshots, client_name);
    Ok(render_runtime_template(argument, &context, false))
}

pub(in super::super) fn render_start_directory_template(
    state: &HandlerState,
    target: &Target,
    attached_count: usize,
    start_directory: Option<PathBuf>,
    client_loop_snapshots: Option<&[ClientLoopSnapshot]>,
    client_name: Option<&str>,
) -> Result<Option<PathBuf>, RmuxError> {
    let Some(start_directory) = start_directory else {
        return Ok(None);
    };
    let template = start_directory.as_os_str().to_string_lossy();
    if !template.contains("#{") {
        return Ok(Some(start_directory));
    }

    let context = format_context_for_target(state, target, attached_count)?;
    let context = apply_client_loop_context(context, client_loop_snapshots, client_name);
    Ok(Some(PathBuf::from(render_runtime_template(
        &template, &context, false,
    ))))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::client_format::{ClientFormatBindings, ClientLoopSnapshot};
    use crate::pane_terminals::HandlerState;
    use rmux_core::formats::{FormatVariable, FormatVariables};
    use rmux_proto::{SessionName, Target, TerminalSize};

    use super::{collect_parse_time_values, render_start_directory_template};

    struct ParseTimeValues;

    impl FormatVariables for ParseTimeValues {
        fn format_value(&self, variable: FormatVariable) -> Option<String> {
            match variable {
                FormatVariable::SessionWidth => Some("api-width".to_owned()),
                FormatVariable::WindowName => Some("api-window".to_owned()),
                _ => None,
            }
        }

        fn format_value_by_name(&self, name: &str) -> Option<String> {
            match name {
                "active_window_index" => Some("3".to_owned()),
                "command" => Some("display-message".to_owned()),
                "current_file" => Some("source.conf".to_owned()),
                "window_name" => Some("table-window".to_owned()),
                _ => None,
            }
        }
    }

    #[test]
    fn parse_time_capture_uses_registered_inventory_with_dynamic_boundaries() {
        let values = collect_parse_time_values(&ParseTimeValues);

        assert_eq!(
            values
                .first()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            Some(("current_file", "source.conf"))
        );
        assert!(values.contains(&("session_width".to_owned(), "api-width".to_owned())));
        assert!(values.contains(&("active_window_index".to_owned(), "3".to_owned())));
        assert!(!values.iter().any(|(name, _)| name == "command"));

        let window_values = values
            .iter()
            .filter(|(name, _)| name == "window_name")
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>();
        assert_eq!(window_values, ["api-window", "table-window"]);
    }

    #[test]
    fn reconstructed_start_directory_keeps_client_loop_context() {
        let mut state = HandlerState::default();
        let session_name = SessionName::new("start-directory-context").expect("valid session name");
        state
            .sessions
            .create_session(session_name.clone(), TerminalSize { cols: 80, rows: 24 })
            .expect("session creates");
        let snapshots = [ClientLoopSnapshot {
            bindings: ClientFormatBindings {
                name: "client-context".to_owned(),
                session: session_name.to_string(),
                ..ClientFormatBindings::default()
            },
            order: 1,
            activity_at: crate::client_activity::ClientActivityTimestamp::from_parts(1, 0),
        }];

        let rendered = render_start_directory_template(
            &state,
            &Target::Session(session_name),
            1,
            Some(PathBuf::from("root/#{client_name}/#{L:#{client_name}}")),
            Some(&snapshots),
            Some("client-context"),
        )
        .expect("start directory renders")
        .expect("start directory is present");

        assert_eq!(
            rendered,
            PathBuf::from("root/client-context/client-context")
        );
    }
}
