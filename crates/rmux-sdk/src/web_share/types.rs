use rmux_proto::{PaneTargetRef, WebShareListener, WebShareScope};

/// Redacted metadata for an active browser-visible pane or session share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebShareSummary {
    /// Opaque share id.
    pub id: String,
    /// Shared pane or session scope.
    pub scope: WebShareScope,
    /// Redacted spectator URL, if available.
    pub spectator_url_redacted: Option<String>,
    /// Whether this share has an operator URL.
    pub operator: bool,
    /// Whether this share has a spectator URL.
    pub spectator: bool,
    /// Active spectator client count.
    pub active_spectators: u16,
    /// Active operator client count.
    pub active_operators: u16,
    /// Maximum spectator clients allowed, when capped.
    pub max_spectators: Option<u16>,
    /// Maximum operator clients allowed, when capped.
    pub max_operators: Option<u16>,
    /// Expiration timestamp in UNIX seconds.
    pub expires_at_unix: Option<u64>,
    /// Whether the daemon kills the target session when this share expires.
    pub kill_session_on_expire: bool,
}

impl WebShareSummary {
    /// Returns the pane target when this is a single-pane share.
    #[must_use]
    pub fn pane_target(&self) -> Option<&PaneTargetRef> {
        match &self.scope {
            WebShareScope::Pane(target) => Some(target),
            WebShareScope::Session(_) => None,
        }
    }
}

impl From<rmux_proto::WebShareSummary> for WebShareSummary {
    fn from(value: rmux_proto::WebShareSummary) -> Self {
        Self {
            id: value.share_id,
            scope: value.scope,
            spectator_url_redacted: value.spectator_url,
            operator: value.operator,
            spectator: value.spectator,
            active_spectators: value.active_spectators,
            active_operators: value.active_operators,
            max_spectators: value.max_spectators,
            max_operators: value.max_operators,
            expires_at_unix: value.expires_at_unix,
            kill_session_on_expire: value.kill_session_on_expire,
        }
    }
}

/// Web-share listener configuration reported by the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebConfigInfo {
    /// Listener host.
    pub host: String,
    /// Listener port.
    pub port: u16,
    /// Origin used by the web-share frontend.
    pub frontend_origin: String,
}

impl From<WebShareListener> for WebConfigInfo {
    fn from(value: WebShareListener) -> Self {
        Self {
            host: value.host,
            port: value.port,
            frontend_origin: value.frontend_origin,
        }
    }
}
