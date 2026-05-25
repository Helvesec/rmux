use serde::{Deserialize, Serialize};

use crate::PaneTargetRef;

/// Request payload for the `web-share` command family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebShareRequest {
    /// Create a new browser-visible pane share.
    Create(CreateWebShareRequest),
    /// List active pane shares.
    List(ListWebSharesRequest),
    /// Stop one active pane share.
    Stop(StopWebShareRequest),
    /// Stop every active pane share.
    StopAll(StopAllWebSharesRequest),
    /// Lookup one active pane share without exposing access keys.
    Lookup(LookupWebShareRequest),
    /// Return the daemon web-share listener configuration.
    Config(WebShareConfigRequest),
}

/// Request payload for `web-share`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateWebShareRequest {
    /// The exact pane slot or stable pane id to expose.
    pub target: PaneTargetRef,
    /// Optional public WS origin forwarded to the daemon.
    #[serde(default)]
    pub public_base_url: Option<String>,
    /// Optional browser frontend URL used for this share.
    #[serde(default)]
    pub frontend_url: Option<String>,
    /// Optional maximum share lifetime in seconds.
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
    /// Optional viewer cap for this share.
    #[serde(default)]
    pub max_viewers: Option<u16>,
    /// Presentation options encoded into generated viewer URLs.
    #[serde(default)]
    pub url_options: WebShareUrlOptions,
    /// Whether clients must provide the out-of-band pairing code during auth.
    #[serde(default)]
    pub require_pin: bool,
    /// Whether an operator URL should be minted.
    #[serde(default)]
    pub writable: bool,
}

/// Browser presentation options for generated web-share URLs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebShareUrlOptions {
    /// Hide the share navigation bar for this generated URL.
    #[serde(default)]
    pub no_navbar: bool,
    /// Suppress the client-side privacy/disclaimer toast.
    #[serde(default)]
    pub no_disclaimer: bool,
}

/// Request payload for `web-share -l`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListWebSharesRequest;

/// Request payload for `web-share -K <id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopWebShareRequest {
    /// Share identifier returned by creation.
    pub share_id: String,
}

/// Request payload for `web-share -X`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopAllWebSharesRequest;

/// Request payload for SDK/browser lookup of share metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookupWebShareRequest {
    /// Share identifier to inspect.
    pub share_id: String,
}

/// Request payload for daemon web-share listener configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebShareConfigRequest;
