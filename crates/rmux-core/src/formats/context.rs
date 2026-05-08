use rmux_proto::TerminalSize;

use super::{bool_value, window_raw_flags};
use crate::{Pane, PaneId, Session, Window, WindowId};

/// The default `list-windows` line format.
pub const DEFAULT_LIST_WINDOWS_FORMAT: &str =
    "#{window_index}: #{window_name}#{window_raw_flags} (#{window_panes} panes) [#{window_width}x#{window_height}]";

/// The default `list-sessions` line format.
pub const DEFAULT_LIST_SESSIONS_FORMAT: &str =
    "#{session_name}: #{session_windows} windows (created #{t:session_created})#{?session_grouped, (group #{session_group}),}#{?session_attached, (attached),}";

/// The default `list-panes` line format.
pub const DEFAULT_LIST_PANES_FORMAT: &str =
    "#{session_name}:#{window_index}.#{pane_index}: [#{pane_width}x#{pane_height}] [history #{history_size}/#{history_limit}, #{history_bytes} bytes] #{pane_id}#{?pane_active, (active),}#{?pane_dead, (dead),}";

/// The default `display-message` format.
pub const DEFAULT_DISPLAY_MESSAGE_FORMAT: &str =
    "[#{session_name}] #{window_index}:#{window_name}, current pane #{pane_index} - (%H:%M %d-%b-%y)";

/// The frozen tmux `format_table[]` variable inventory from the reference source.
pub const TMUX_FORMAT_TABLE_NAMES: [&str; 192] = [
    "active_window_index",
    "alternate_on",
    "alternate_saved_x",
    "alternate_saved_y",
    "bracket_paste_flag",
    "buffer_created",
    "buffer_full",
    "buffer_mode_format",
    "buffer_name",
    "buffer_sample",
    "buffer_size",
    "client_activity",
    "client_cell_height",
    "client_cell_width",
    "client_control_mode",
    "client_created",
    "client_discarded",
    "client_flags",
    "client_height",
    "client_key_table",
    "client_last_session",
    "client_mode_format",
    "client_name",
    "client_pid",
    "client_prefix",
    "client_readonly",
    "client_session",
    "client_termfeatures",
    "client_termname",
    "client_termtype",
    "client_theme",
    "client_tty",
    "client_uid",
    "client_user",
    "client_utf8",
    "client_width",
    "client_written",
    "config_files",
    "cursor_blinking",
    "cursor_character",
    "cursor_colour",
    "cursor_flag",
    "cursor_shape",
    "cursor_very_visible",
    "cursor_x",
    "cursor_y",
    "history_all_bytes",
    "history_bytes",
    "history_limit",
    "history_size",
    "host",
    "host_short",
    "insert_flag",
    "keypad_cursor_flag",
    "keypad_flag",
    "last_window_index",
    "loop_last_flag",
    "mouse_all_flag",
    "mouse_any_flag",
    "mouse_button_flag",
    "mouse_hyperlink",
    "mouse_line",
    "mouse_pane",
    "mouse_sgr_flag",
    "mouse_standard_flag",
    "mouse_status_line",
    "mouse_status_range",
    "mouse_utf8_flag",
    "mouse_word",
    "mouse_x",
    "mouse_y",
    "next_session_id",
    "origin_flag",
    "pane_active",
    "pane_at_bottom",
    "pane_at_left",
    "pane_at_right",
    "pane_at_top",
    "pane_bg",
    "pane_bottom",
    "pane_current_command",
    "pane_current_path",
    "pane_dead",
    "pane_dead_signal",
    "pane_dead_status",
    "pane_dead_time",
    "pane_fg",
    "pane_flags",
    "pane_floating_flag",
    "pane_format",
    "pane_height",
    "pane_id",
    "pane_in_mode",
    "pane_index",
    "pane_input_off",
    "pane_key_mode",
    "pane_last",
    "pane_left",
    "pane_marked",
    "pane_marked_set",
    "pane_mode",
    "pane_path",
    "pane_pb_progress",
    "pane_pb_state",
    "pane_pid",
    "pane_pipe",
    "pane_pipe_pid",
    "pane_right",
    "pane_search_string",
    "pane_start_command",
    "pane_start_path",
    "pane_synchronized",
    "pane_tabs",
    "pane_title",
    "pane_top",
    "pane_tty",
    "pane_unseen_changes",
    "pane_width",
    "pane_zoomed_flag",
    "pid",
    "scroll_region_lower",
    "scroll_region_upper",
    "server_sessions",
    "session_active",
    "session_activity",
    "session_activity_flag",
    "session_alert",
    "session_alerts",
    "session_attached",
    "session_attached_list",
    "session_bell_flag",
    "session_created",
    "session_format",
    "session_group",
    "session_group_attached",
    "session_group_attached_list",
    "session_group_list",
    "session_group_many_attached",
    "session_group_size",
    "session_grouped",
    "session_id",
    "session_last_attached",
    "session_many_attached",
    "session_marked",
    "session_name",
    "session_path",
    "session_silence_flag",
    "session_stack",
    "session_windows",
    "sixel_support",
    "socket_path",
    "start_time",
    "synchronized_output_flag",
    "tree_mode_format",
    "uid",
    "user",
    "version",
    "window_active",
    "window_active_clients",
    "window_active_clients_list",
    "window_active_sessions",
    "window_active_sessions_list",
    "window_activity",
    "window_activity_flag",
    "window_bell_flag",
    "window_bigger",
    "window_cell_height",
    "window_cell_width",
    "window_end_flag",
    "window_flags",
    "window_format",
    "window_height",
    "window_id",
    "window_index",
    "window_last_flag",
    "window_layout",
    "window_linked",
    "window_linked_sessions",
    "window_linked_sessions_list",
    "window_marked_flag",
    "window_name",
    "window_offset_x",
    "window_offset_y",
    "window_panes",
    "window_raw_flags",
    "window_silence_flag",
    "window_stack_index",
    "window_start_flag",
    "window_visible_layout",
    "window_width",
    "window_zoomed_flag",
    "wrap_flag",
];

/// Frozen tmux `FORMAT_TABLE_TIME` variable names.
pub const TMUX_TIME_FORMAT_VARIABLE_NAMES: [&str; 9] = [
    "buffer_created",
    "client_activity",
    "client_created",
    "pane_dead_time",
    "session_activity",
    "session_created",
    "session_last_attached",
    "start_time",
    "window_activity",
];

/// The closed set of format variables supported by RMUX.
pub const FORMAT_VARIABLES: [FormatVariable; 20] = [
    FormatVariable::SessionName,
    FormatVariable::SessionWindows,
    FormatVariable::SessionAttached,
    FormatVariable::SessionWidth,
    FormatVariable::SessionHeight,
    FormatVariable::WindowIndex,
    FormatVariable::WindowId,
    FormatVariable::WindowName,
    FormatVariable::WindowRawFlags,
    FormatVariable::WindowPanes,
    FormatVariable::WindowWidth,
    FormatVariable::WindowHeight,
    FormatVariable::WindowLayout,
    FormatVariable::WindowActive,
    FormatVariable::WindowLastFlag,
    FormatVariable::PaneIndex,
    FormatVariable::PaneId,
    FormatVariable::PaneActive,
    FormatVariable::PaneWidth,
    FormatVariable::PaneHeight,
];

/// A supported format variable name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormatVariable {
    /// The session name.
    SessionName,
    /// The number of windows in the session.
    SessionWindows,
    /// The number of clients attached to the session.
    SessionAttached,
    /// The session width in columns.
    SessionWidth,
    /// The session height in rows.
    SessionHeight,
    /// The window index within the session.
    WindowIndex,
    /// The stable window identifier, rendered with an `@` prefix.
    WindowId,
    /// The window name, or an empty string when unnamed.
    WindowName,
    /// The derived active or last-window marker.
    WindowRawFlags,
    /// The number of panes in the window.
    WindowPanes,
    /// The window width in columns.
    WindowWidth,
    /// The window height in rows.
    WindowHeight,
    /// The window layout name.
    WindowLayout,
    /// Whether the window is active, rendered as `1` or `0`.
    WindowActive,
    /// Whether the window is the last window, rendered as `1` or `0`.
    WindowLastFlag,
    /// The pane index within the window.
    PaneIndex,
    /// The stable pane identifier, rendered with a `%` prefix.
    PaneId,
    /// Whether the pane is active, rendered as `1` or `0`.
    PaneActive,
    /// The pane width in columns.
    PaneWidth,
    /// The pane height in rows.
    PaneHeight,
}

impl FormatVariable {
    /// Parses a supported format variable name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "session_name" => Self::SessionName,
            "session_windows" => Self::SessionWindows,
            "session_attached" => Self::SessionAttached,
            "session_width" => Self::SessionWidth,
            "session_height" => Self::SessionHeight,
            "window_index" => Self::WindowIndex,
            "window_id" => Self::WindowId,
            "window_name" => Self::WindowName,
            "window_raw_flags" => Self::WindowRawFlags,
            "window_panes" => Self::WindowPanes,
            "window_width" => Self::WindowWidth,
            "window_height" => Self::WindowHeight,
            "window_layout" => Self::WindowLayout,
            "window_active" => Self::WindowActive,
            "window_last_flag" => Self::WindowLastFlag,
            "pane_index" => Self::PaneIndex,
            "pane_id" => Self::PaneId,
            "pane_active" => Self::PaneActive,
            "pane_width" => Self::PaneWidth,
            "pane_height" => Self::PaneHeight,
            _ => return None,
        })
    }

    /// Returns the canonical supported variable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SessionName => "session_name",
            Self::SessionWindows => "session_windows",
            Self::SessionAttached => "session_attached",
            Self::SessionWidth => "session_width",
            Self::SessionHeight => "session_height",
            Self::WindowIndex => "window_index",
            Self::WindowId => "window_id",
            Self::WindowName => "window_name",
            Self::WindowRawFlags => "window_raw_flags",
            Self::WindowPanes => "window_panes",
            Self::WindowWidth => "window_width",
            Self::WindowHeight => "window_height",
            Self::WindowLayout => "window_layout",
            Self::WindowActive => "window_active",
            Self::WindowLastFlag => "window_last_flag",
            Self::PaneIndex => "pane_index",
            Self::PaneId => "pane_id",
            Self::PaneActive => "pane_active",
            Self::PaneWidth => "pane_width",
            Self::PaneHeight => "pane_height",
        }
    }
}

/// Returns whether a variable name is part of the supported format inventory.
#[must_use]
pub fn is_known_format_variable_name(name: &str) -> bool {
    FormatVariable::from_name(name).is_some()
        || TMUX_FORMAT_TABLE_NAMES.binary_search(&name).is_ok()
}

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

    /// Checks whether a window or session name exists for the `N:` modifier.
    ///
    /// `scope` is `None` or `Some('w')` for windows and `Some('s')` for
    /// sessions.
    fn format_name_exists(&self, _scope: Option<char>, _name: &str) -> Option<bool> {
        None
    }

    /// Expands runtime loop modifiers such as `S`, `W`, and `P`.
    ///
    /// The default implementation leaves these modifiers unsupported.
    fn format_loop(&self, _scope: char, _body: &str, _count_only: bool) -> Option<String> {
        None
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
