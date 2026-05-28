use serde::{Deserialize, Serialize};

use super::CommandOutput;
use crate::SessionName;

/// Response payload for `kill-server`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillServerResponse;

/// Response payload for internal daemon status inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatusResponse {
    /// RMUX package version of the running daemon.
    pub rmux_version: String,
    /// Detached RPC wire version used by the running daemon.
    pub wire_version: u32,
    /// Number of sessions currently owned by the daemon.
    pub session_count: usize,
    /// Number of attach/control clients or detached RPC requests currently active.
    pub client_count: usize,
}

/// Response payload for internal idle-only daemon shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShutdownIfIdleResponse {
    /// Whether shutdown was queued by the daemon.
    pub shutdown: bool,
    /// Number of sessions observed when the daemon made the decision.
    pub session_count: usize,
    /// Number of attach/control clients or detached RPC requests observed when deciding.
    pub client_count: usize,
}

/// Response payload for `lock-server`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockServerResponse;

/// Response payload for `lock-session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockSessionResponse {
    /// The session whose attached clients were considered for locking.
    pub target: SessionName,
}

/// Response payload for `lock-client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockClientResponse {
    /// The client identifier that was targeted.
    pub target_client: String,
}

/// Response payload for `server-access`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerAccessResponse {
    /// Expanded stdout for `server-access`.
    pub output: CommandOutput,
}

impl ServerAccessResponse {
    /// Builds a response with no stdout payload.
    #[must_use]
    pub const fn no_output(output: CommandOutput) -> Self {
        Self { output }
    }

    /// Returns the stdout payload for `server-access`.
    #[must_use]
    pub const fn command_output(&self) -> &CommandOutput {
        &self.output
    }
}
