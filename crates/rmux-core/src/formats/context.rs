use rmux_proto::TerminalSize;

use super::variable::FormatVariable;
use super::{bool_value, window_raw_flags};
use crate::{Pane, PaneId, Session, Window, WindowId};

/// The default `list-windows` line format.
pub const DEFAULT_LIST_WINDOWS_FORMAT: &str =
    "#{window_index}: #{window_name}#{window_raw_flags} (#{window_panes} panes) [#{window_width}x#{window_height}] [layout #{window_layout}] #{window_id}#{?window_active, (active),}";

/// The default `list-windows -a` line format.
pub const DEFAULT_LIST_WINDOWS_ALL_FORMAT: &str =
    "#{session_name}:#{window_index}: #{window_name}#{window_raw_flags} (#{window_panes} panes) [#{window_width}x#{window_height}] ";

/// The default `list-sessions` line format.
pub const DEFAULT_LIST_SESSIONS_FORMAT: &str =
    "#{session_name}: #{session_windows} windows (created #{t:session_created})#{?session_grouped, (group #{session_group}),}#{?session_attached, (attached),}";

/// The default `list-panes` line format for a single window.
pub const DEFAULT_LIST_PANES_WINDOW_FORMAT: &str =
    "#{pane_index}: [#{pane_width}x#{pane_height}] [history #{history_size}/#{history_limit}, #{history_bytes} bytes] #{pane_id}#{?pane_active, (active),}#{?pane_dead, (dead),}";

/// The default `list-panes -s` line format for a single session.
pub const DEFAULT_LIST_PANES_SESSION_FORMAT: &str =
    "#{window_index}.#{pane_index}: [#{pane_width}x#{pane_height}] [history #{history_size}/#{history_limit}, #{history_bytes} bytes] #{pane_id}#{?pane_active, (active),}#{?pane_dead, (dead),}";

/// The default `list-panes -a` line format.
pub const DEFAULT_LIST_PANES_ALL_FORMAT: &str =
    "#{session_name}:#{window_index}.#{pane_index}: [#{pane_width}x#{pane_height}] [history #{history_size}/#{history_limit}, #{history_bytes} bytes] #{pane_id}#{?pane_active, (active),}#{?pane_dead, (dead),}";

/// Backward-compatible alias for callers that need the fully qualified form.
pub const DEFAULT_LIST_PANES_FORMAT: &str = DEFAULT_LIST_PANES_ALL_FORMAT;

/// The default `display-message` format.
pub const DEFAULT_DISPLAY_MESSAGE_FORMAT: &str =
    "[#{session_name}] #{window_index}:#{window_name}, current pane #{pane_index} - (%H:%M %d-%b-%y)";

/// A source of values for supported format variables.
pub trait FormatVariables {
    /// Resolves a supported format variable to its rendered value.
    fn format_value(&self, variable: FormatVariable) -> Option<String>;

    /// Resolves an arbitrary string-keyed variable name.
    ///
    /// The default implementation delegates to the enum-based lookup. Runtime
    /// implementors can override this to support `@user` options and the full
    /// dynamic variable inventory.
    fn format_value_by_name(&self, name: &str) -> Option<String> {
        FormatVariable::from_name(name).and_then(|v| self.format_value(v))
    }

    /// Resolves an arbitrary string-keyed variable name for a path modifier,
    /// keeping absence apart from a value that is present and empty.
    ///
    /// `#{b:…}` and `#{d:…}` need that difference: the engine measured keeps an
    /// absent value absent and returns before either modifier runs, while a
    /// value that is really there - even one holding nothing - goes through
    /// them, which is why an option set to the empty string answers `.` and an
    /// option that is not set answers nothing at all. Everything else in this
    /// crate keeps reading [`Self::format_value_by_name`], whose rendering of a
    /// legacy empty value is unchanged.
    ///
    /// The default delegates to that same lookup, so an implementor that has no
    /// such distinction to make behaves exactly as it did. A runtime that does -
    /// one whose registry answers a known name with a historical empty value -
    /// overrides this to report only what it really found.
    fn format_path_value_by_name(&self, name: &str) -> Option<String> {
        self.format_value_by_name(name)
    }

    /// Reads or starts a nonblocking runtime job. Commands are already expanded
    /// in the current evaluation context; the default has no execution authority.
    fn format_job(&self, _source: &str, _command: &str) -> Option<String> {
        None
    }

    /// Checks whether a window or session name exists for the `N:` modifier.
    ///
    /// `scope` is `None` or `Some('w')` for windows and `Some('s')` for
    /// sessions.
    fn format_name_exists(&self, _scope: Option<char>, _name: &str) -> Option<bool> {
        None
    }

    /// Searches the current runtime content for the `C:` modifier.
    ///
    /// Implementors that do not have a pane screen should return `None`.
    fn format_search(&self, _options: &str, _pattern: &str) -> Option<String> {
        None
    }

    /// Expands runtime loop modifiers such as `S`, `W`, and `P`.
    ///
    /// The default implementation leaves these modifiers unsupported.
    fn format_loop(
        &self,
        _scope: char,
        _body: &str,
        _current_body: Option<&str>,
        _count_only: bool,
    ) -> Option<String> {
        None
    }

    /// Expands a runtime loop modifier, preserving the modifier arguments.
    ///
    /// Existing implementors that do not care about loop sort arguments can
    /// keep overriding [`Self::format_loop`].
    fn format_loop_with_args(
        &self,
        scope: char,
        _arguments: &[String],
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
    ) -> Option<String> {
        self.format_loop(scope, body, current_body, count_only)
    }

    /// Expands a runtime loop while allowing the loop implementation to render
    /// each body through the caller's active expansion state.
    ///
    /// The default keeps existing loop implementations unchanged. Runtime
    /// implementations that need a distinct variable context per item can
    /// invoke `expand` with that context; doing so preserves recursion limits
    /// across nested loop bodies.
    fn format_loop_with_args_and_expand(
        &self,
        scope: char,
        arguments: &[String],
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
        _expand: &mut dyn FnMut(&str, &dyn FormatVariables) -> String,
    ) -> Option<String> {
        self.format_loop_with_args(scope, arguments, body, current_body, count_only)
    }
}

/// Format values populated from session, window, pane, and server-side context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormatContext {
    session_name: Option<String>,
    session_windows: Option<usize>,
    session_attached: Option<usize>,
    session_size: Option<TerminalSize>,
    window_index: Option<u32>,
    window_id: Option<WindowId>,
    window_name: Option<String>,
    window_panes: Option<usize>,
    window_size: Option<TerminalSize>,
    window_layout: Option<String>,
    window_active: Option<bool>,
    window_last_flag: Option<bool>,
    pane_index: Option<u32>,
    pane_id: Option<PaneId>,
    pane_active: Option<bool>,
    pane_size: Option<TerminalSize>,
    named_values: Vec<(String, String)>,
}

impl FormatContext {
    /// Creates an empty format context.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a format context populated from a session.
    #[must_use]
    pub fn from_session(session: &Session) -> Self {
        Self::new().with_session(session)
    }

    /// Populates session-level variables from a session.
    #[must_use]
    pub fn with_session(mut self, session: &Session) -> Self {
        self.session_name = Some(session.name().to_string());
        self.session_windows = Some(session.windows().len());
        self.session_size = Some(session.window().size());
        self
    }

    /// Populates session variables through a borrowed canonical session view.
    #[must_use]
    pub fn with_managed_session(mut self, session: crate::ManagedSessionView<'_>) -> Self {
        self.session_name = Some(session.name().to_string());
        self.session_windows = Some(session.windows().count());
        self.session_size = Some(session.window().size());
        self
    }

    /// Populates the externally tracked attached-client count.
    #[must_use]
    pub const fn with_session_attached(mut self, attached_count: usize) -> Self {
        self.session_attached = Some(attached_count);
        self
    }

    /// Populates window-level variables from a window and its session-local state.
    #[must_use]
    pub fn with_window(
        mut self,
        window_index: u32,
        window: &Window,
        active: bool,
        last: bool,
    ) -> Self {
        self.window_index = Some(window_index);
        self.window_id = Some(window.id());
        self.window_name = window.name().map(str::to_owned);
        self.window_panes = Some(window.pane_count());
        self.window_size = Some(window.size());
        self.window_layout = Some(window.layout_dump());
        self.window_active = Some(active);
        self.window_last_flag = Some(last);
        self
    }

    /// Populates pane-level variables from a pane and its window-local state.
    #[must_use]
    pub fn with_pane(mut self, pane: &Pane, active: bool) -> Self {
        let geometry = pane.geometry();
        self.pane_index = Some(pane.index());
        self.pane_id = Some(pane.id());
        self.pane_active = Some(active);
        self.pane_size = Some(TerminalSize {
            cols: geometry.cols(),
            rows: geometry.rows(),
        });
        self
    }

    /// Populates pane-level variables, deriving active state from the owning window.
    #[must_use]
    pub fn with_window_pane(self, window: &Window, pane: &Pane) -> Self {
        self.with_pane(pane, pane.index() == window.active_pane_index())
    }

    /// Populates an arbitrary named format variable.
    #[must_use]
    pub fn with_named_value(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.named_values.push((name.into(), value.into()));
        self
    }
}

impl FormatVariables for FormatContext {
    fn format_value(&self, variable: FormatVariable) -> Option<String> {
        match variable {
            FormatVariable::SessionName => self.session_name.clone(),
            FormatVariable::SessionWindows => self.session_windows.map(|value| value.to_string()),
            FormatVariable::SessionAttached => self.session_attached.map(|value| value.to_string()),
            FormatVariable::SessionWidth => self.session_size.map(|size| size.cols.to_string()),
            FormatVariable::SessionHeight => self.session_size.map(|size| size.rows.to_string()),
            FormatVariable::WindowIndex => self.window_index.map(|value| value.to_string()),
            FormatVariable::WindowId => self.window_id.map(|value| value.to_string()),
            FormatVariable::WindowName => self.window_name.clone(),
            FormatVariable::WindowRawFlags => {
                if self.window_active.is_some() || self.window_last_flag.is_some() {
                    Some(
                        window_raw_flags(
                            self.window_active.unwrap_or(false),
                            self.window_last_flag.unwrap_or(false),
                        )
                        .to_owned(),
                    )
                } else {
                    None
                }
            }
            FormatVariable::WindowPanes => self.window_panes.map(|value| value.to_string()),
            FormatVariable::WindowWidth => self.window_size.map(|size| size.cols.to_string()),
            FormatVariable::WindowHeight => self.window_size.map(|size| size.rows.to_string()),
            FormatVariable::WindowLayout => self.window_layout.clone(),
            FormatVariable::WindowActive => self.window_active.map(bool_value),
            FormatVariable::WindowLastFlag => self.window_last_flag.map(bool_value),
            FormatVariable::PaneIndex => self.pane_index.map(|value| value.to_string()),
            FormatVariable::PaneId => self.pane_id.map(|value| value.to_string()),
            FormatVariable::PaneActive => self.pane_active.map(bool_value),
            FormatVariable::PaneWidth => self.pane_size.map(|size| size.cols.to_string()),
            FormatVariable::PaneHeight => self.pane_size.map(|size| size.rows.to_string()),
        }
    }

    fn format_value_by_name(&self, name: &str) -> Option<String> {
        self.named_values
            .iter()
            .rev()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.clone())
            .or_else(|| {
                FormatVariable::from_name(name).and_then(|variable| self.format_value(variable))
            })
    }
}
