use super::registry::{format_variable_descriptor, FormatVariableDescriptor, FormatVariableKey};

/// The closed set of enum-backed format variables supported by RMUX.
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

/// A supported enum-backed format variable.
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
    /// Parses a supported enum-backed format variable name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        format_variable_descriptor(name).and_then(FormatVariableDescriptor::format_variable)
    }

    /// Returns the canonical supported variable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.key().name()
    }

    /// Returns the registry key for this enum-backed variable.
    #[must_use]
    pub const fn key(self) -> FormatVariableKey {
        match self {
            Self::SessionName => FormatVariableKey::SessionName,
            Self::SessionWindows => FormatVariableKey::SessionWindows,
            Self::SessionAttached => FormatVariableKey::SessionAttached,
            Self::SessionWidth => FormatVariableKey::SessionWidth,
            Self::SessionHeight => FormatVariableKey::SessionHeight,
            Self::WindowIndex => FormatVariableKey::WindowIndex,
            Self::WindowId => FormatVariableKey::WindowId,
            Self::WindowName => FormatVariableKey::WindowName,
            Self::WindowRawFlags => FormatVariableKey::WindowRawFlags,
            Self::WindowPanes => FormatVariableKey::WindowPanes,
            Self::WindowWidth => FormatVariableKey::WindowWidth,
            Self::WindowHeight => FormatVariableKey::WindowHeight,
            Self::WindowLayout => FormatVariableKey::WindowLayout,
            Self::WindowActive => FormatVariableKey::WindowActive,
            Self::WindowLastFlag => FormatVariableKey::WindowLastFlag,
            Self::PaneIndex => FormatVariableKey::PaneIndex,
            Self::PaneId => FormatVariableKey::PaneId,
            Self::PaneActive => FormatVariableKey::PaneActive,
            Self::PaneWidth => FormatVariableKey::PaneWidth,
            Self::PaneHeight => FormatVariableKey::PaneHeight,
        }
    }
}
