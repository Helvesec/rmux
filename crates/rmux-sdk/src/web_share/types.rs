use rmux_proto::{PaneTargetRef, WebShareListener, WebShareScope};

/// Redacted metadata for an active browser-visible pane or session share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebShareSummary {
    /// Opaque share id.
    pub id: String,
    /// Shared pane or session scope.
    pub scope: WebShareScope,
    /// Redacted read-only URL, if available.
    pub read_url_redacted: Option<String>,
    /// Whether this share has an operator URL.
    pub writable: bool,
    /// Whether this share grants remote rmux controls.
    pub controls: bool,
    /// Active read-only client count.
    pub active_readers: u16,
    /// Maximum read-only clients allowed.
    pub max_readers: u16,
    /// Whether the single operator slot is occupied.
    pub operator_connected: bool,
    /// Expiration timestamp in UNIX seconds.
    pub expires_at_unix: Option<u64>,
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
            read_url_redacted: value.read_url,
            writable: value.writable,
            controls: value.controls,
            active_readers: value.active_readers,
            max_readers: value.max_readers,
            operator_connected: value.operator_connected,
            expires_at_unix: value.expires_at_unix,
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
