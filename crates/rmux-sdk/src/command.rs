//! SDK command DTOs.
//!
//! Commands in this module are serializable value objects. They can be mapped
//! to `rmux-proto` requests, but they do not send IPC, start daemons, probe
//! endpoints, or parse tmux command strings.

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

/// A detached command payload accepted by the SDK.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RmuxCommandKind {
    /// A protocol request supplied directly by the caller.
    Request(rmux_proto::Request),
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
            Self::Request(request) => request.command_name(),
            Self::NewSession(_) => "new-session",
            Self::AttachSession(_) => "attach-session",
            Self::SplitWindow(_) => "split-window",
            Self::RefreshClient(_) => "refresh-client",
        }
    }

    /// Converts this value object into the corresponding protocol request.
    #[must_use]
    pub fn into_request(self) -> rmux_proto::Request {
        self.into()
    }
}

impl From<rmux_proto::Request> for RmuxCommandKind {
    fn from(value: rmux_proto::Request) -> Self {
        Self::Request(value)
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

impl From<RmuxCommandKind> for rmux_proto::Request {
    fn from(value: RmuxCommandKind) -> Self {
        match value {
            RmuxCommandKind::Request(request) => request,
            RmuxCommandKind::NewSession(spec) => Self::NewSessionExt(spec.into()),
            RmuxCommandKind::AttachSession(spec) => Self::AttachSessionExt2(spec.into()),
            RmuxCommandKind::SplitWindow(spec) => Self::SplitWindowExt(spec.into()),
            RmuxCommandKind::RefreshClient(spec) => Self::RefreshClient(spec.into()),
        }
    }
}

impl From<NewSessionSpec> for rmux_proto::Request {
    fn from(value: NewSessionSpec) -> Self {
        RmuxCommandKind::from(value).into()
    }
}

impl From<AttachSessionSpec> for rmux_proto::Request {
    fn from(value: AttachSessionSpec) -> Self {
        RmuxCommandKind::from(value).into()
    }
}

impl From<SplitSpec> for rmux_proto::Request {
    fn from(value: SplitSpec) -> Self {
        RmuxCommandKind::from(value).into()
    }
}

impl From<RefreshClientSpec> for rmux_proto::Request {
    fn from(value: RefreshClientSpec) -> Self {
        RmuxCommandKind::from(value).into()
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

    /// Converts the command payload into its protocol request.
    #[must_use]
    pub fn into_request(self) -> rmux_proto::Request {
        self.command.into_request()
    }
}

impl From<RmuxCommandKind> for RmuxCommand {
    fn from(value: RmuxCommandKind) -> Self {
        Self::new(value)
    }
}

impl From<rmux_proto::Request> for RmuxCommand {
    fn from(value: rmux_proto::Request) -> Self {
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

impl From<RmuxCommand> for rmux_proto::Request {
    fn from(value: RmuxCommand) -> Self {
        value.into_request()
    }
}
