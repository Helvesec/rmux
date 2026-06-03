use std::sync::OnceLock;

use crate::pane_terminals::HandlerState;
use chrono::Local;
use rmux_core::formats::{
    expand_time_tokens, render_template, FormatContext, FormatVariable, FormatVariables,
};
use rmux_core::{BufferStore, EnvironmentStore, OptionStore, Pane, Session, SessionStore, Window};
use rmux_proto::OptionName;
use rmux_proto::{SessionName, TerminalSize};

static SERVER_START_TIME: OnceLock<i64> = OnceLock::new();

#[path = "format_runtime/geometry.rs"]
mod geometry;
#[path = "format_runtime/index.rs"]
mod index;
#[path = "format_runtime/loops.rs"]
mod loops;
#[path = "format_runtime/path.rs"]
mod path;
#[path = "format_runtime/process.rs"]
mod process;
#[path = "format_runtime/variables.rs"]
mod variables;
#[path = "format_runtime/window.rs"]
mod window;

pub(crate) struct RuntimeFormatContext<'a> {
    base: FormatContext,
    state: Option<&'a HandlerState>,
    options: Option<&'a OptionStore>,
    environment: Option<&'a EnvironmentStore>,
    session_store: Option<&'a SessionStore>,
    buffers: Option<&'a BufferStore>,
    session: Option<&'a Session>,
    window_index: Option<u32>,
    window: Option<&'a Window>,
    pane: Option<&'a Pane>,
    client_size: Option<TerminalSize>,
    hide_session_size: bool,
    use_unclipped_geometry: bool,
}

impl<'a> RuntimeFormatContext<'a> {
    pub(crate) fn new(base: FormatContext) -> Self {
        Self {
            base,
            state: None,
            options: None,
            environment: None,
            session_store: None,
            buffers: None,
            session: None,
            window_index: None,
            window: None,
            pane: None,
            client_size: None,
            hide_session_size: false,
            use_unclipped_geometry: false,
        }
    }

    pub(crate) fn with_state(mut self, state: &'a HandlerState) -> Self {
        self.options = Some(&state.options);
        self.environment = Some(&state.environment);
        self.session_store = Some(&state.sessions);
        self.buffers = Some(&state.buffers);
        self.state = Some(state);
        self
    }

    pub(crate) fn with_options(mut self, options: &'a OptionStore) -> Self {
        self.options = Some(options);
        self
    }

    pub(crate) fn with_session(mut self, session: &'a Session) -> Self {
        self.session = Some(session);
        self
    }

    pub(crate) fn with_window(mut self, window_index: u32, window: &'a Window) -> Self {
        self.window_index = Some(window_index);
        self.window = Some(window);
        self
    }

    pub(crate) fn with_pane(mut self, pane: &'a Pane) -> Self {
        self.pane = Some(pane);
        self
    }

    pub(crate) fn with_client_size(mut self, client_size: TerminalSize) -> Self {
        self.client_size = Some(client_size);
        self
    }

    pub(crate) fn without_session_size(mut self) -> Self {
        self.hide_session_size = true;
        self
    }

    pub(crate) fn with_unclipped_geometry(mut self) -> Self {
        self.use_unclipped_geometry = true;
        self
    }

    pub(crate) fn with_named_value(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.base = self.base.with_named_value(name, value);
        self
    }

    fn option_store(&self) -> Option<&OptionStore> {
        self.options
    }

    fn environment_store(&self) -> Option<&EnvironmentStore> {
        self.environment
    }

    fn session_name(&self) -> Option<&SessionName> {
        self.session.map(Session::name)
    }

    fn pane_history_size(&self) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        let stats = self.state?.pane_history_stats(session.name(), pane.id())?;
        Some(stats.size.to_string())
    }

    fn pane_history_limit(&self) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        let stats = self.state?.pane_history_stats(session.name(), pane.id())?;
        Some(stats.limit.to_string())
    }

    fn pane_history_bytes(&self) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        let stats = self.state?.pane_history_stats(session.name(), pane.id())?;
        Some(stats.bytes.to_string())
    }

    fn pane_lifecycle(&self) -> Option<&crate::pane_terminals::PaneLifecycleState> {
        let pane = self.pane?;
        self.state?.pane_lifecycle(pane.id())
    }

    fn pane_start_command(&self) -> Option<String> {
        self.pane_lifecycle()?.encoded_command()
    }

    fn pane_start_path(&self) -> Option<String> {
        self.pane_lifecycle()?
            .working_directory()
            .map(|path| path.to_string_lossy().into_owned())
    }

    fn pane_lifecycle_generation(&self) -> Option<String> {
        Some(self.pane_lifecycle()?.generation.to_string())
    }

    fn pane_lifecycle_revision(&self) -> Option<String> {
        Some(self.pane_lifecycle()?.revision.to_string())
    }

    fn pane_output_sequence(&self) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        self.state?
            .pane_output_sequence(session.name(), pane.id())
            .or_else(|| self.pane_lifecycle().map(|state| state.output_sequence))
            .map(|sequence| sequence.to_string())
    }

    fn pane_cursor_position(&self) -> Option<(u32, u32)> {
        let session = self.session?;
        let pane = self.pane?;
        self.state?
            .pane_copy_mode_render_screen(session.name(), pane.id())
            .or_else(|| self.state?.pane_render_screen(session.name(), pane.id()))
            .map(|screen| screen.cursor_position())
    }

    fn pane_screen_mode(&self) -> Option<u32> {
        let session = self.session?;
        let pane = self.pane?;
        Some(
            self.state?
                .pane_screen_state(session.name(), pane.id())?
                .mode,
        )
    }

    fn pane_alternate_on(&self) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        let state = self.state?.pane_screen_state(session.name(), pane.id())?;
        Some(bool_string(state.alternate_on))
    }

    fn pane_title(&self) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        let state = self.state?.pane_screen_state(session.name(), pane.id())?;
        Some(state.title)
    }

    fn pane_screen_path(&self) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        let state = self.state?.pane_screen_state(session.name(), pane.id())?;
        path::pane_path_from_osc7(&state.path)
    }

    fn automatic_window_name(&self) -> Option<String> {
        let state = self.state?;
        let session_name = self.session_name()?;
        let window_index = self.window_index?;
        let window = self.window?;
        let tracked = state.tracks_auto_named_window(session_name, window_index);
        if !crate::automatic_rename::window_allows_automatic_rename(
            &state.options,
            session_name,
            window_index,
            window,
            tracked,
        ) {
            return None;
        }

        let template = state
            .options
            .resolve_for_window(
                session_name,
                window_index,
                OptionName::AutomaticRenameFormat,
            )
            .unwrap_or_default();
        if template.is_empty() {
            return None;
        }

        let rendered =
            render_runtime_template(template, &AutoRenameFormatContext { inner: self }, false);
        (!rendered.is_empty()).then_some(rendered)
    }

    fn window_name(&self) -> Option<String> {
        self.automatic_window_name()
            .or_else(|| self.base.format_value(FormatVariable::WindowName))
    }

    fn rendered_window_name(&self, window_index: u32, window: &'a Window) -> Option<String> {
        let state = self.state?;
        let session = self.session?;
        let mut context = FormatContext::from_session(session)
            .with_session_attached(self.session_attached_count())
            .with_window(
                window_index,
                window,
                window_index == session.active_window_index(),
                Some(window_index) == session.last_window_index(),
            );
        if let Some(pane) = window.active_pane() {
            context = context.with_window_pane(window, pane);
        }
        let runtime = RuntimeFormatContext::new(context)
            .with_state(state)
            .with_session(session)
            .with_window(window_index, window);
        let runtime = if let Some(pane) = window.active_pane() {
            runtime.with_pane(pane)
        } else {
            runtime
        };
        runtime.window_name()
    }

    fn pane_pid(&self) -> Option<String> {
        let session_name = self.session_name()?;
        let window_index = self.window_index?;
        let pane = self.pane?;
        self.state?
            .pane_pid_in_window(session_name, window_index, pane.index())
            .ok()
            .map(|pid| pid.to_string())
    }

    #[cfg(unix)]
    fn pane_tty(&self) -> Option<String> {
        let session_name = self.session_name()?;
        let window_index = self.window_index?;
        let pane = self.pane?;
        self.state?
            .pane_tty_path_in_window(session_name, window_index, pane.index())
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
    }

    #[cfg(windows)]
    fn pane_tty(&self) -> Option<String> {
        None
    }

    fn pane_exit_metadata(&self) -> Option<crate::pane_terminals::PaneExitMetadata> {
        let session = self.session?;
        let pane = self.pane?;
        self.state?.pane_exit_metadata(session.name(), pane.id())
    }

    fn pane_dead(&self) -> bool {
        self.pane_exit_metadata().is_some()
    }

    fn pane_dead_signal(&self) -> Option<String> {
        self.pane_exit_metadata()
            .and_then(|metadata| metadata.signal.map(|signal| signal.to_string()))
            .or_else(|| Some(String::new()))
    }

    fn pane_dead_status(&self) -> Option<String> {
        self.pane_exit_metadata()
            .and_then(|metadata| metadata.status.map(|status| status.to_string()))
            .or_else(|| Some(String::new()))
    }

    fn pane_dead_time(&self) -> Option<String> {
        self.pane_exit_metadata()
            .map(|metadata| metadata.time.to_string())
            .or_else(|| Some(String::new()))
    }

    fn pane_mode_flag(&self, bit: u32) -> Option<String> {
        self.pane_screen_mode()
            .map(|mode| bool_string(mode & bit != 0))
    }

    fn pane_copy_mode_summary(&self) -> Option<crate::copy_mode::CopyModeSummary> {
        let session = self.session?;
        let pane = self.pane?;
        self.state?
            .pane_copy_mode_summary(session.name(), pane.id())
    }

    fn pane_mode_name(&self) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        self.state?
            .pane_mode_name(session.name(), pane.id())
            .map(str::to_owned)
    }

    fn pane_marked(&self) -> Option<String> {
        let session = self.session?;
        let window_index = self.window_index?;
        let pane = self.pane?;
        Some(bool_string(self.state.is_some_and(|state| {
            state.pane_is_marked(&rmux_proto::PaneTarget::with_window(
                session.name().clone(),
                window_index,
                pane.index(),
            ))
        })))
    }

    fn pane_in_mode(&self) -> bool {
        let Some(session) = self.session else {
            return false;
        };
        let Some(pane) = self.pane else {
            return false;
        };
        self.state
            .is_some_and(|state| state.pane_in_mode(session.name(), pane.id()))
    }

    fn marked_pane_set(&self) -> bool {
        self.state
            .and_then(|state| state.marked_pane_target())
            .is_some()
    }

    fn session_marked(&self) -> bool {
        self.session_name().is_some_and(|session_name| {
            self.state
                .is_some_and(|state| state.session_has_marked_pane(session_name))
        })
    }

    fn window_marked(&self) -> bool {
        self.session_name()
            .zip(self.window_index)
            .is_some_and(|(session_name, window_index)| {
                self.state
                    .is_some_and(|state| state.window_has_marked_pane(session_name, window_index))
            })
    }

    fn option_value_by_name(&self, name: &str) -> Option<String> {
        let store = self.option_store()?;

        match (self.session_name(), self.window_index, self.pane) {
            (Some(session_name), Some(window_index), Some(pane)) => {
                store.resolve_name_for_pane_format(session_name, window_index, pane.index(), name)
            }
            (Some(session_name), Some(window_index), None) => {
                store.resolve_name_for_window_format(session_name, window_index, name)
            }
            (Some(session_name), None, _) => {
                store.resolve_name_for_format(Some(session_name), name)
            }
            (None, _, _) => store.resolve_name_for_format(None, name),
        }
    }

    fn environment_value_by_name(&self, name: &str) -> Option<String> {
        self.environment_store()
            .and_then(|environment| environment.resolve(self.session_name(), name))
            .map(str::to_owned)
    }

    fn buffer_head(&self) -> Option<(String, Vec<u8>)> {
        let buffers = self.buffers?;
        let (name, content) = buffers.show(None).ok()?;
        Some((name.to_owned(), content.to_vec()))
    }
}

struct AutoRenameFormatContext<'a> {
    inner: &'a RuntimeFormatContext<'a>,
}

impl FormatVariables for AutoRenameFormatContext<'_> {
    fn format_value(&self, variable: FormatVariable) -> Option<String> {
        if variable == FormatVariable::WindowName {
            return self.inner.base.format_value(variable);
        }
        self.inner.format_value(variable)
    }

    fn format_loop(&self, scope: char, body: &str, count_only: bool) -> Option<String> {
        self.inner.format_loop(scope, body, count_only)
    }

    fn format_name_exists(&self, scope: Option<char>, name: &str) -> Option<bool> {
        self.inner.format_name_exists(scope, name)
    }

    fn format_value_by_name(&self, name: &str) -> Option<String> {
        if name == "window_name" {
            return self
                .inner
                .base
                .format_value_by_name(name)
                .or_else(|| self.inner.base.format_value(FormatVariable::WindowName));
        }
        self.inner.format_value_by_name(name)
    }
}

pub(crate) fn render_runtime_template<V>(template: &str, variables: &V, expand_time: bool) -> String
where
    V: FormatVariables + ?Sized,
{
    let template = if expand_time {
        expand_time_tokens(template)
    } else {
        template.to_owned()
    };
    render_template(&template, variables)
}

pub(crate) fn render_automatic_window_name(runtime: &RuntimeFormatContext<'_>) -> Option<String> {
    runtime.automatic_window_name()
}

fn bool_string(value: bool) -> String {
    if value {
        "1".to_owned()
    } else {
        "0".to_owned()
    }
}

fn server_start_time() -> i64 {
    *SERVER_START_TIME.get_or_init(|| Local::now().timestamp())
}

#[cfg(test)]
mod tests {
    use super::render_runtime_template;
    use rmux_core::formats::FormatContext;

    #[test]
    fn render_runtime_template_expands_strftime_tokens_before_formats() {
        let rendered = render_runtime_template("%H:%M", &FormatContext::new(), true);
        let bytes = rendered.as_bytes();

        assert_eq!(bytes.len(), 5);
        assert!(bytes[0].is_ascii_digit());
        assert!(bytes[1].is_ascii_digit());
        assert_eq!(bytes[2], b':');
        assert!(bytes[3].is_ascii_digit());
        assert!(bytes[4].is_ascii_digit());
    }

    #[test]
    fn render_runtime_template_keeps_invalid_percent_literals() {
        assert_eq!(
            render_runtime_template("cpu 100%", &FormatContext::new(), true),
            "cpu 100%"
        );
    }
}
