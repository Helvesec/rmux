//! Shared protocol value types.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

use crate::RmuxError;
pub use rmux_types::TerminalSize;

#[path = "types/hooks.rs"]
mod hooks;
#[path = "types/options.rs"]
mod options;

pub use hooks::{HookLifecycle, HookName};
pub use options::{OptionName, SetOptionMode};

/// A validated RMUX session name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SessionName(String);

impl SessionName {
    /// Validates and stores a session name using tmux-compatible rewriting.
    pub fn new(value: impl Into<String>) -> Result<Self, RmuxError> {
        let value = value.into();

        if value.is_empty() {
            return Err(RmuxError::EmptySessionName);
        }

        Ok(Self(sanitize_session_name(value.as_bytes())))
    }

    /// Returns the sanitized validated session name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the wrapper and returns the sanitized string.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

fn sanitize_session_name(input: &[u8]) -> String {
    let mut sanitized = String::with_capacity(input.len());
    for &byte in input {
        let rewritten = match byte {
            b':' | b'.' => b'_',
            other => other,
        };
        push_session_name_byte(rewritten, &mut sanitized);
    }
    sanitized
}

fn push_session_name_byte(byte: u8, output: &mut String) {
    if (0x20..=0x7e).contains(&byte) && byte != b'\\' {
        output.push(char::from(byte));
        return;
    }

    match byte {
        b'\0' => output.push_str("\\000"),
        b'\x07' => output.push_str("\\a"),
        b'\x08' => output.push_str("\\b"),
        b'\t' => output.push_str("\\t"),
        b'\n' => output.push_str("\\n"),
        b'\x0b' => output.push_str("\\v"),
        b'\x0c' => output.push_str("\\f"),
        b'\r' => output.push_str("\\r"),
        b'\\' => output.push_str("\\\\"),
        _ => {
            output.push('\\');
            output.push(char::from(b'0' + ((byte >> 6) & 0x7)));
            output.push(char::from(b'0' + ((byte >> 3) & 0x7)));
            output.push(char::from(b'0' + (byte & 0x7)));
        }
    }
}

impl AsRef<str> for SessionName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for SessionName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SessionName {
    type Err = RmuxError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for SessionName {
    type Error = RmuxError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for SessionName {
    type Error = RmuxError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for SessionName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A parsed exact target.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Target {
    /// A session target in the form `session-name`.
    Session(SessionName),
    /// A window target in the form `session-name:window-index`.
    Window(WindowTarget),
    /// A pane target in the form `session-name:window-index.pane-index`.
    Pane(PaneTarget),
}

impl Target {
    /// Parses the exact detached target forms supported by the detached server.
    pub fn parse(value: &str) -> Result<Self, RmuxError> {
        if let Some((session_name, tail)) = value.split_once(':') {
            let session_name = SessionName::new(session_name.to_owned())?;

            if !tail.is_empty() && tail.chars().all(|character| character.is_ascii_digit()) {
                let window_index = parse_window_index(value, tail)?;
                return Ok(Self::Window(WindowTarget::with_window(
                    session_name,
                    window_index,
                )));
            }

            if let Some((window_index, pane_index)) = tail.split_once('.') {
                let window_index = parse_window_index(value, window_index)?;
                let pane_index = parse_pane_index(value, pane_index)?;
                return Ok(Self::Pane(PaneTarget::with_window(
                    session_name,
                    window_index,
                    pane_index,
                )));
            }

            return Err(RmuxError::invalid_target(
                value,
                "targets must match 'session', 'session:window', or 'session:window.pane'",
            ));
        }

        Ok(Self::Session(SessionName::new(value.to_owned())?))
    }

    /// Returns the session name addressed by the target.
    #[must_use]
    pub fn session_name(&self) -> &SessionName {
        match self {
            Self::Session(session_name) => session_name,
            Self::Window(target) => target.session_name(),
            Self::Pane(target) => target.session_name(),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(session_name) => session_name.fmt(formatter),
            Self::Window(target) => target.fmt(formatter),
            Self::Pane(target) => target.fmt(formatter),
        }
    }
}

impl FromStr for Target {
    type Err = RmuxError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// A validated window target.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WindowTarget {
    session_name: SessionName,
    window_index: u32,
}

impl WindowTarget {
    /// Creates a V1-compatible window target for window `0`.
    #[must_use]
    pub const fn new(session_name: SessionName) -> Self {
        Self::with_window(session_name, 0)
    }

    /// Creates a window target for the provided window index.
    #[must_use]
    pub const fn with_window(session_name: SessionName, window_index: u32) -> Self {
        Self {
            session_name,
            window_index,
        }
    }

    /// Returns the session name component.
    #[must_use]
    pub const fn session_name(&self) -> &SessionName {
        &self.session_name
    }

    /// Returns the addressed window index.
    #[must_use]
    pub const fn window_index(&self) -> u32 {
        self.window_index
    }
}

impl fmt::Display for WindowTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.session_name, self.window_index)
    }
}

/// A validated pane target.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneTarget {
    session_name: SessionName,
    window_index: u32,
    pane_index: u32,
}

impl PaneTarget {
    /// Creates a V1-compatible pane target anchored to window `0`.
    #[must_use]
    pub const fn new(session_name: SessionName, pane_index: u32) -> Self {
        Self::with_window(session_name, 0, pane_index)
    }

    /// Creates a pane target for the provided window and pane indices.
    #[must_use]
    pub const fn with_window(
        session_name: SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Self {
        Self {
            session_name,
            window_index,
            pane_index,
        }
    }

    /// Returns the session name component.
    #[must_use]
    pub const fn session_name(&self) -> &SessionName {
        &self.session_name
    }

    /// Returns the addressed window index.
    #[must_use]
    pub const fn window_index(&self) -> u32 {
        self.window_index
    }

    /// Returns the pane index component.
    #[must_use]
    pub const fn pane_index(&self) -> u32 {
        self.pane_index
    }
}

impl fmt::Display for PaneTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}.{}",
            self.session_name, self.window_index, self.pane_index
        )
    }
}

/// A global-or-session selector used by detached mutations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeSelector {
    /// Global scope.
    Global,
    /// Session-local scope.
    Session(SessionName),
    /// Window-local scope.
    Window(WindowTarget),
    /// Pane-local scope.
    Pane(PaneTarget),
}

/// Explicit option mutation scope for the open option model.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OptionScopeSelector {
    /// Server-global options.
    ServerGlobal,
    /// Session-global options.
    SessionGlobal,
    /// Window-global options.
    WindowGlobal,
    /// Session-local options.
    Session(SessionName),
    /// Window-local options.
    Window(WindowTarget),
    /// Pane-local options.
    Pane(PaneTarget),
}

/// The detached layout name subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutName {
    /// The required `main-vertical` layout.
    MainVertical,
    /// Internal `split-window -h` geometry using a top main pane.
    MainHorizontal,
    /// The tmux `even-horizontal` left-to-right layout.
    EvenHorizontal,
    /// The tmux `even-vertical` top-to-bottom layout.
    EvenVertical,
    /// The tmux `tiled` grid layout.
    Tiled,
    /// The tmux `main-horizontal-mirrored` layout.
    MainHorizontalMirrored,
    /// The tmux `main-vertical-mirrored` layout.
    MainVerticalMirrored,
}

impl fmt::Display for LayoutName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MainVertical => formatter.write_str("main-vertical"),
            Self::MainHorizontal => formatter.write_str("main-horizontal"),
            Self::EvenHorizontal => formatter.write_str("even-horizontal"),
            Self::EvenVertical => formatter.write_str("even-vertical"),
            Self::Tiled => formatter.write_str("tiled"),
            Self::MainHorizontalMirrored => formatter.write_str("main-horizontal-mirrored"),
            Self::MainVerticalMirrored => formatter.write_str("main-vertical-mirrored"),
        }
    }
}

impl FromStr for LayoutName {
    type Err = RmuxError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "main-vertical" => Ok(Self::MainVertical),
            "main-horizontal" => Ok(Self::MainHorizontal),
            "even-horizontal" => Ok(Self::EvenHorizontal),
            "even-vertical" => Ok(Self::EvenVertical),
            "tiled" => Ok(Self::Tiled),
            "main-horizontal-mirrored" => Ok(Self::MainHorizontalMirrored),
            "main-vertical-mirrored" => Ok(Self::MainVerticalMirrored),
            _ => Err(RmuxError::Server(format!("unknown layout: {value}"))),
        }
    }
}

/// The split orientation accepted by `split-window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SplitDirection {
    /// Split into left and right panes.
    #[default]
    Vertical,
    /// Split into top and bottom panes, matching tmux `split-window -h`.
    Horizontal,
}

/// The detached resize semantics supported in V1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResizePaneAdjustment {
    /// Sets the absolute pane width in columns.
    AbsoluteWidth {
        /// The requested pane width in columns.
        columns: u16,
    },
    /// Sets the absolute pane height in rows.
    AbsoluteHeight {
        /// The requested pane height in rows.
        rows: u16,
    },
    /// Toggles zoom for the targeted pane's window.
    Zoom,
    /// Shrinks the pane height upward by a relative amount.
    Up {
        /// The requested row delta.
        cells: u16,
    },
    /// Grows the pane height downward by a relative amount.
    Down {
        /// The requested row delta.
        cells: u16,
    },
    /// Shrinks the pane width leftward by a relative amount.
    Left {
        /// The requested column delta.
        cells: u16,
    },
    /// Grows the pane width rightward by a relative amount.
    Right {
        /// The requested column delta.
        cells: u16,
    },
    /// Resolves the target and reports success without changing layout.
    NoOp,
}

fn parse_pane_index(target: &str, pane_index: &str) -> Result<u32, RmuxError> {
    if pane_index.is_empty() {
        return Err(RmuxError::invalid_target(
            target,
            "pane index must be an unsigned integer",
        ));
    }

    pane_index
        .parse::<u32>()
        .map_err(|_| RmuxError::invalid_target(target, "pane index must be an unsigned integer"))
}

fn parse_window_index(target: &str, window_index: &str) -> Result<u32, RmuxError> {
    if window_index.is_empty() {
        return Err(RmuxError::invalid_target(
            target,
            "window index must be an unsigned integer",
        ));
    }

    window_index
        .parse::<u32>()
        .map_err(|_| RmuxError::invalid_target(target, "window index must be an unsigned integer"))
}

#[cfg(test)]
mod tests;
