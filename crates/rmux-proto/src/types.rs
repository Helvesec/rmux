//! Shared protocol value types.
//!
//! Identity newtypes (`SessionName`, `SessionId`, `WindowId`, `PaneId`)
//! are defined exactly once in [`crate::identity`]; this module
//! re-exports `SessionName` so legacy import paths continue to resolve.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub use crate::identity::SessionName;
use crate::{PaneId, RmuxError};
pub use rmux_types::{TerminalGeometry, TerminalPixels, TerminalSize};

mod hooks;
mod options;

pub use hooks::{HookLifecycle, HookName};
pub use options::{OptionName, SetOptionMode};

mod ids;
mod layout;

pub use ids::{PaneOutputSubscriptionId, PaneStateSubscriptionId, SdkWaitId, SdkWaitOwnerId};
pub use layout::{LayoutName, ResizePaneAdjustment, ResizePaneRelativeDirection, SplitDirection};

/// Explicit process launch mode for daemon-owned pane processes.
///
/// This is distinct from the legacy `command: Option<Vec<String>>` fields on
/// some request DTOs. Legacy command fields preserve tmux-compatible behavior
/// where a single string runs through `$SHELL -c`; this enum records caller
/// intent directly so SDK `spawn(argv)` can remain argv-based even for a
/// single program name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ProcessCommand {
    /// Execute the program directly with the supplied argv vector.
    ///
    /// On Windows, `.bat` and `.cmd` targets are rejected because `cmd.exe`
    /// cannot preserve arbitrary argv boundaries. Use [`Self::Shell`] when
    /// shell interpretation is intentional, or target a native executable.
    Argv(Vec<String>),
    /// Execute command text through the configured shell.
    Shell(String),
}

impl ProcessCommand {
    /// Converts a legacy command vector into the historical tmux-compatible
    /// launch mode.
    #[must_use]
    pub fn from_legacy_command(command: Option<&[String]>) -> Option<Self> {
        match command {
            Some([single]) => Some(Self::Shell(single.clone())),
            Some(argv) if !argv.is_empty() => Some(Self::Argv(argv.to_vec())),
            _ => None,
        }
    }

    /// Returns a redaction/display-friendly command vector.
    ///
    /// Shell commands are represented as a one-element vector to preserve the
    /// existing `pane_start_command` encoding shape.
    #[must_use]
    pub fn display_command(&self) -> Vec<String> {
        match self {
            Self::Argv(argv) => argv.clone(),
            Self::Shell(command) => vec![command.clone()],
        }
    }

    /// Returns true when the command contains no executable work.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Argv(argv) => argv.is_empty() || argv.first().is_some_and(String::is_empty),
            Self::Shell(command) => command.is_empty(),
        }
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
    ///
    /// The session part is lookup text for an existing session, so it is read
    /// as a canonical name: a `\\` typed in the target matches the `\\` that
    /// [`SessionName::new`] stored for a literal backslash, as in tmux.
    pub fn parse(value: &str) -> Result<Self, RmuxError> {
        if let Some((session_name, tail)) = value.split_once(':') {
            let session_name = SessionName::from_canonical(session_name)
                .map_err(|_| RmuxError::EmptySessionName)?;

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

        Ok(Self::Session(
            SessionName::from_canonical(value).map_err(|_| RmuxError::EmptySessionName)?,
        ))
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

/// Pane selector for SDK operations that can address either a display slot
/// or a stable pane identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PaneTargetRef {
    /// Existing slot-based selector.
    Slot(PaneTarget),
    /// Stable pane id scoped by session name.
    Id {
        /// Exact session name component.
        session_name: SessionName,
        /// Stable pane identity within one daemon lifetime.
        pane_id: PaneId,
    },
}

impl PaneTargetRef {
    /// Creates a selector for an existing slot target.
    #[must_use]
    pub const fn slot(target: PaneTarget) -> Self {
        Self::Slot(target)
    }

    /// Creates a selector for a stable pane id in a session.
    #[must_use]
    pub const fn by_id(session_name: SessionName, pane_id: PaneId) -> Self {
        Self::Id {
            session_name,
            pane_id,
        }
    }

    /// Returns the session name component.
    #[must_use]
    pub const fn session_name(&self) -> &SessionName {
        match self {
            Self::Slot(target) => target.session_name(),
            Self::Id { session_name, .. } => session_name,
        }
    }

    /// Returns the stable pane identity when this selector is id-based.
    #[must_use]
    pub const fn pane_id(&self) -> Option<PaneId> {
        match self {
            Self::Slot(_) => None,
            Self::Id { pane_id, .. } => Some(*pane_id),
        }
    }
}

impl From<PaneTarget> for PaneTargetRef {
    fn from(value: PaneTarget) -> Self {
        Self::Slot(value)
    }
}

impl fmt::Display for PaneTargetRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Slot(target) => target.fmt(formatter),
            Self::Id {
                session_name,
                pane_id,
            } => write!(formatter, "{session_name}:{pane_id}"),
        }
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
