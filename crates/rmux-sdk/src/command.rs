//! SDK command DTOs.
//!
//! Commands in this module are inert, serializable SDK value objects. They do
//! not send IPC, start daemons, probe endpoints, or parse tmux command strings.
//! Use typed facade operations or [`crate::Rmux::cmd`] for execution.

use serde::{Deserialize, Serialize};

use crate::{AttachSessionSpec, NewSessionSpec, RefreshClientSpec, RmuxEndpoint, SplitSpec};

/// Captured result from the official SDK command escape hatch.
///
/// [`crate::Rmux::cmd`] runs the `rmux` binary with an endpoint selector
/// injected by the SDK. Non-zero process exits are returned here rather than
/// converted into SDK errors so callers can implement tmux-style command
/// wrappers with precise stdout, stderr, and status handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CommandRun {
    /// Captured stdout bytes.
    pub stdout: Vec<u8>,
    /// Captured stderr bytes.
    pub stderr: Vec<u8>,
    /// Process exit code, or `None` when the process terminated without a
    /// platform exit code.
    pub exit: Option<i32>,
}

/// An inert, typed SDK command payload.
///
/// Serde preserves the original typed variant names and binary indices 1–4.
/// The former raw `Request` name and index 0 are rejected during deserialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RmuxCommandKind {
    /// A `new-session` SDK spec.
    NewSession(NewSessionSpec),
    /// An `attach-session` SDK spec.
    AttachSession(AttachSessionSpec),
    /// A `split-window` SDK spec.
    SplitWindow(SplitSpec),
    /// A `refresh-client` SDK spec.
    RefreshClient(RefreshClientSpec),
}

impl RmuxCommandKind {
    /// Returns the stable public command name for this value object.
    #[must_use]
    pub fn command_name(&self) -> &'static str {
        match self {
            Self::NewSession(_) => "new-session",
            Self::AttachSession(_) => "attach-session",
            Self::SplitWindow(_) => "split-window",
            Self::RefreshClient(_) => "refresh-client",
        }
    }
}

impl From<NewSessionSpec> for RmuxCommandKind {
    fn from(value: NewSessionSpec) -> Self {
        Self::NewSession(value)
    }
}

impl From<AttachSessionSpec> for RmuxCommandKind {
    fn from(value: AttachSessionSpec) -> Self {
        Self::AttachSession(value)
    }
}

impl From<SplitSpec> for RmuxCommandKind {
    fn from(value: SplitSpec) -> Self {
        Self::SplitWindow(value)
    }
}

impl From<RefreshClientSpec> for RmuxCommandKind {
    fn from(value: RefreshClientSpec) -> Self {
        Self::RefreshClient(value)
    }
}

/// Endpoint-scoped SDK command DTO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RmuxCommand {
    /// Endpoint selector associated with this command.
    #[serde(default)]
    pub endpoint: RmuxEndpoint,
    /// Inert command payload.
    pub command: RmuxCommandKind,
}

impl RmuxCommand {
    /// Creates a command using the platform default endpoint selector.
    #[must_use]
    pub fn new(command: impl Into<RmuxCommandKind>) -> Self {
        Self {
            endpoint: RmuxEndpoint::default(),
            command: command.into(),
        }
    }

    /// Creates a command using an explicit endpoint selector.
    #[must_use]
    pub fn with_endpoint(endpoint: RmuxEndpoint, command: impl Into<RmuxCommandKind>) -> Self {
        Self {
            endpoint,
            command: command.into(),
        }
    }

    /// Returns the stable public command name for the command payload.
    #[must_use]
    pub fn command_name(&self) -> &'static str {
        self.command.command_name()
    }
}

impl From<RmuxCommandKind> for RmuxCommand {
    fn from(value: RmuxCommandKind) -> Self {
        Self::new(value)
    }
}

impl From<NewSessionSpec> for RmuxCommand {
    fn from(value: NewSessionSpec) -> Self {
        Self::new(value)
    }
}

impl From<AttachSessionSpec> for RmuxCommand {
    fn from(value: AttachSessionSpec) -> Self {
        Self::new(value)
    }
}

impl From<SplitSpec> for RmuxCommand {
    fn from(value: SplitSpec) -> Self {
        Self::new(value)
    }
}

impl From<RefreshClientSpec> for RmuxCommand {
    fn from(value: RefreshClientSpec) -> Self {
        Self::new(value)
    }
}

mod serde_kind;
