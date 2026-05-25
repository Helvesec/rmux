use serde::{Deserialize, Serialize};

use super::CommandOutput;
use crate::PaneTargetRef;

/// Response payload for the `web-share` command family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebShareResponse {
    /// A share was created and access URLs are available to the caller.
    Created(WebShareCreatedResponse),
    /// Active shares were listed.
    List(WebShareListResponse),
    /// One share was stopped.
    Stopped(WebShareStoppedResponse),
    /// Every active share was stopped.
    StoppedAll(WebShareStoppedAllResponse),
    /// One active share was looked up without exposing access keys.
    Lookup(WebShareLookupResponse),
    /// Listener configuration was returned.
    Config(WebShareConfigResponse),
}

impl WebShareResponse {
    /// Returns command stdout for CLI-facing web-share responses.
    #[must_use]
    pub fn command_output(&self) -> Option<&CommandOutput> {
        match self {
            Self::Created(response) => Some(&response.output),
            Self::List(response) => Some(&response.output),
            Self::Stopped(response) => Some(&response.output),
            Self::StoppedAll(response) => Some(&response.output),
            Self::Lookup(response) => Some(&response.output),
            Self::Config(response) => Some(&response.output),
        }
    }
}

/// Success payload for creating a browser-visible pane share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebShareCreatedResponse {
    /// Opaque share identifier.
    pub share_id: String,
    /// Shared pane selector.
    pub target: PaneTargetRef,
    /// Browser URL for read-only viewers.
    pub viewer_url: String,
    /// Browser URL for the single writable operator, when requested.
    #[serde(default)]
    pub operator_url: Option<String>,
    /// Expiration timestamp as UNIX seconds, when a TTL was requested.
    #[serde(default)]
    pub expires_at_unix: Option<u64>,
    /// Effective viewer cap.
    pub max_viewers: u16,
    /// Whether an operator URL was minted.
    pub writable: bool,
    /// CLI stdout rendering.
    pub output: CommandOutput,
}

/// Success payload for listing active browser-visible pane shares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebShareListResponse {
    /// Redacted active share rows.
    pub shares: Vec<WebShareSummary>,
    /// CLI stdout rendering.
    pub output: CommandOutput,
}

/// Success payload for stopping one active pane share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebShareStoppedResponse {
    /// Requested share identifier.
    pub share_id: String,
    /// Whether the share existed and was removed.
    pub stopped: bool,
    /// CLI stdout rendering.
    pub output: CommandOutput,
}

/// Success payload for stopping every active pane share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebShareStoppedAllResponse {
    /// Number of removed shares.
    pub stopped: u32,
    /// CLI stdout rendering.
    pub output: CommandOutput,
}

/// Success payload for looking up one active share without exposing keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebShareLookupResponse {
    /// Redacted share metadata, if the share exists.
    #[serde(default)]
    pub share: Option<WebShareSummary>,
    /// CLI stdout rendering.
    pub output: CommandOutput,
}

/// Success payload for daemon web-share listener configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebShareConfigResponse {
    /// Current listener binding.
    pub listener: WebShareListener,
    /// CLI stdout rendering.
    pub output: CommandOutput,
}

/// Redacted metadata for an active browser-visible pane share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebShareSummary {
    /// Opaque share identifier.
    pub share_id: String,
    /// Shared pane selector.
    pub target: PaneTargetRef,
    /// Redacted viewer URL, when available for display.
    #[serde(default)]
    pub viewer_url: Option<String>,
    /// Whether an operator URL exists for this share.
    pub writable: bool,
    /// Active read-only viewers.
    pub active_viewers: u16,
    /// Effective viewer cap.
    pub max_viewers: u16,
    /// Whether the single operator slot is currently connected.
    pub operator_connected: bool,
    /// Expiration timestamp as UNIX seconds, when a TTL was requested.
    #[serde(default)]
    pub expires_at_unix: Option<u64>,
}

/// Listener metadata for browser-visible pane shares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebShareListener {
    /// Listener host or IP address.
    pub host: String,
    /// Listener TCP port.
    pub port: u16,
    /// Frontend origin used when generating URLs.
    pub frontend_origin: String,
}
