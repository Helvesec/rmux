use serde::{Deserialize, Serialize};

use crate::SessionName;

/// Request payload for `kill-server`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillServerRequest;

/// Request payload for internal daemon status inspection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatusRequest;

/// Request payload for internal idle-only daemon shutdown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShutdownIfIdleRequest;

/// Request payload for `lock-server`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockServerRequest;

/// Request payload for `lock-session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockSessionRequest {
    /// The exact target session name.
    pub target: SessionName,
}

/// Request payload for `lock-client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockClientRequest {
    /// The target client identifier or `=`.
    pub target_client: String,
}

/// Request payload for `server-access`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerAccessRequest {
    /// Whether the user should be added or updated.
    #[serde(default)]
    pub add: bool,
    /// Whether the user should be denied and disconnected.
    #[serde(default)]
    pub deny: bool,
    /// Whether the current ACL should be listed.
    #[serde(default)]
    pub list: bool,
    /// Whether the resulting entry should be read-only.
    #[serde(default)]
    pub read_only: bool,
    /// Whether the resulting entry should be read-write.
    #[serde(default)]
    pub write: bool,
    /// The optional username argument.
    #[serde(default)]
    pub user: Option<String>,
}
