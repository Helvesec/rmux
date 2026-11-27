//! SDK type vocabulary.
//!
//! Identity newtypes are defined exactly once in `rmux-types`. This module
//! re-exports the four authoritative identity types (`SessionName`,
//! `SessionId`, `WindowId`, `PaneId`) so SDK users never have to depend on
//! `rmux-core`, `rmux-server`, `rmux-client`, or `rmux-pty` to obtain
//! them. The SDK does not redeclare these newtypes; `rmux-types` is the
//! single public home for the identity vocabulary.

use std::{fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

pub use rmux_types::{PaneId, SessionId, SessionName, SessionNameError, WindowId};

/// Selects the daemon endpoint resolution strategy used by the SDK.
///
/// `Default` defers to SDK runtime discovery, falling through to platform
/// defaults resolved through the existing RMUX IPC/OS layer when no SDK
/// environment override is accepted. The explicit variants carry
/// caller-supplied paths/names and bypass the auto-discovery allowlist while
/// still preserving the daemon's own permission and symlink checks.
///
/// Marked `#[non_exhaustive]` because additional transports (such as TCP
/// or test-harness in-memory pipes) are anticipated in later steps and
/// must be addable without breaking downstream pattern matches.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RmuxEndpoint {
    /// Resolve through SDK discovery, falling through to the platform default.
    #[default]
    Default,
    /// Use an explicit Unix domain socket path.
    UnixSocket(PathBuf),
    /// Use an explicit Windows named pipe identifier.
    WindowsPipe(String),
}

impl RmuxEndpoint {
    /// Returns `true` when this endpoint defers to platform default
    /// resolution.
    #[must_use]
    pub fn is_default(&self) -> bool {
        matches!(self, Self::Default)
    }
}

/// Terminal geometry carried by SDK value objects.
///
/// This is an inert DTO. Converting it to the protocol type does not inspect
/// the caller's terminal or normalize zero dimensions.
/// Transport conversions are crate-private; construct SDK sizes with [`Self::new`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TerminalSizeSpec {
    /// Requested terminal columns.
    pub cols: u16,
    /// Requested terminal rows.
    pub rows: u16,
}

impl TerminalSizeSpec {
    /// Creates a terminal-size DTO from explicit column and row counts.
    #[must_use]
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }

    pub(crate) fn into_proto(self) -> rmux_proto::TerminalSize {
        rmux_proto::TerminalSize {
            cols: self.cols,
            rows: self.rows,
        }
    }

    pub(crate) fn from_proto(value: rmux_proto::TerminalSize) -> Self {
        Self {
            cols: value.cols,
            rows: value.rows,
        }
    }
}

#[cfg(test)]
#[path = "types/size_tests.rs"]
mod size_tests;

#[cfg(test)]
#[path = "types/target_tests.rs"]
mod target_tests;

/// Exact window selector used by SDK DTOs.
///
/// This type stores already-structured target components. It deliberately
/// performs no tmux target parsing or server-side lookup.
/// Display uses `session:window`; transport conversions are SDK-private.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WindowRef {
    /// Exact session name component.
    pub session_name: SessionName,
    /// Exact window index component.
    pub window_index: u32,
}

impl WindowRef {
    /// Creates a window selector from explicit target components.
    #[must_use]
    pub fn new(session_name: SessionName, window_index: u32) -> Self {
        Self {
            session_name,
            window_index,
        }
    }

    /// Creates a selector for window index `0` in the given session.
    #[must_use]
    pub fn first(session_name: SessionName) -> Self {
        Self::new(session_name, 0)
    }

    pub(crate) fn to_proto(&self) -> rmux_proto::WindowTarget {
        self.clone().into_proto()
    }

    pub(crate) fn into_proto(self) -> rmux_proto::WindowTarget {
        rmux_proto::WindowTarget::with_window(self.session_name, self.window_index)
    }

    pub(crate) fn from_proto(value: rmux_proto::WindowTarget) -> Self {
        Self {
            session_name: value.session_name().clone(),
            window_index: value.window_index(),
        }
    }
}

impl fmt::Display for WindowRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.session_name, self.window_index)
    }
}

/// Exact pane selector used by SDK DTOs.
///
/// This type stores already-structured target components. It deliberately
/// performs no tmux target parsing or server-side lookup.
/// Display uses `session:window.pane`; transport conversions are SDK-private.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneRef {
    /// Exact session name component.
    pub session_name: SessionName,
    /// Exact window index component.
    pub window_index: u32,
    /// Exact pane index component.
    pub pane_index: u32,
}

impl PaneRef {
    /// Creates a pane selector from explicit target components.
    #[must_use]
    pub fn new(session_name: SessionName, window_index: u32, pane_index: u32) -> Self {
        Self {
            session_name,
            window_index,
            pane_index,
        }
    }

    /// Creates a selector in window index `0`.
    #[must_use]
    pub fn in_first_window(session_name: SessionName, pane_index: u32) -> Self {
        Self::new(session_name, 0, pane_index)
    }

    pub(crate) fn to_proto(&self) -> rmux_proto::PaneTarget {
        self.clone().into_proto()
    }

    pub(crate) fn into_proto(self) -> rmux_proto::PaneTarget {
        rmux_proto::PaneTarget::with_window(self.session_name, self.window_index, self.pane_index)
    }

    pub(crate) fn from_proto(value: rmux_proto::PaneTarget) -> Self {
        Self {
            session_name: value.session_name().clone(),
            window_index: value.window_index(),
            pane_index: value.pane_index(),
        }
    }
}

impl fmt::Display for PaneRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}.{}",
            self.session_name, self.window_index, self.pane_index
        )
    }
}

/// Exact target selector used by SDK DTOs that accept multiple target forms.
///
/// The enum stores structured components only; it does not parse tmux target
/// strings or resolve ambiguous targets.
/// Display uses `session`, `session:window` or `session:window.pane`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TargetRef {
    /// Exact session target.
    Session(SessionName),
    /// Exact window target.
    Window(WindowRef),
    /// Exact pane target.
    Pane(PaneRef),
}

impl fmt::Display for TargetRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(session_name) => session_name.fmt(formatter),
            Self::Window(target) => target.fmt(formatter),
            Self::Pane(target) => target.fmt(formatter),
        }
    }
}
