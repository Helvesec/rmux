//! Shared protocol value types.
//!
//! Identity newtypes (`SessionName`, `SessionId`, `WindowId`, `PaneId`)
//! are defined exactly once in [`crate::identity`]; this module
//! re-exports `SessionName` so legacy import paths continue to resolve.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub use crate::identity::SessionName;
use crate::RmuxError;
pub use rmux_types::TerminalSize;

#[path = "types/hooks.rs"]
mod hooks;
#[path = "types/options.rs"]
mod options;

pub use hooks::{HookLifecycle, HookName};
pub use options::{OptionName, SetOptionMode};

/// Stable identifier for one pane-output subscription on a live server
/// connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneOutputSubscriptionId(u64);

impl PaneOutputSubscriptionId {
    /// Wraps a raw subscription identifier.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw subscription identifier.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
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
