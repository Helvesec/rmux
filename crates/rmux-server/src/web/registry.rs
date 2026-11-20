use std::sync::atomic::AtomicU64;
use std::sync::Mutex;

use rmux_core::PaneId;
use rmux_proto::{
    CommandOutput, CreateWebShareRequest, ListWebSharesRequest, LookupWebShareRequest,
    StopAllWebSharesRequest, StopWebShareRequest, WebShareConfigRequest, WebShareConfigResponse,
    WebShareListResponse, WebShareListener, WebShareLookupResponse, WebShareResponse,
    WebShareStoppedAllResponse, WebShareStoppedResponse,
};
use rmux_proto::{RmuxError, SessionId, SessionName};
use tokio::sync::watch;
use tracing::info;

mod auth;
mod create;
mod output;
mod state;

use super::auth_wait_limit::{
    AuthWaitLimit, DEFAULT_AUTH_WAIT_LIMIT, DEFAULT_AUTH_WAIT_PER_KEY_LIMIT,
    DEFAULT_AUTH_WAIT_PER_PEER_LIMIT,
};
use super::backoff::AuthBackoff;
use super::connection_limit::{ConnectionLimit, DEFAULT_AUTHENTICATED_CONNECTION_LIMIT};
use super::record::{WebSessionTarget, WebShareRevokeReason, WebShareTarget};
use super::settings::WebShareSettings;
use super::tunnel::TunnelInfo;
pub(crate) use auth::WebShareAuthWaitPermit;
use output::{list_output, lookup_output, stopped_output};
pub(crate) use state::{ExpiredWebShare, WebShareExpiryPoll};
use state::{WebListenerState, WebShareState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WebShareRoleLimits {
    pub(crate) max_spectators: Option<u16>,
    pub(crate) max_operators: Option<u16>,
}

#[derive(Debug)]
pub(crate) struct ResolvedCreateWebShareRequest {
    request: CreateWebShareRequest,
    target: WebShareTarget,
    tunnel: Option<TunnelInfo>,
}

impl ResolvedCreateWebShareRequest {
    pub(crate) fn new(request: CreateWebShareRequest, target: WebShareTarget) -> Self {
        Self {
            request,
            target,
            tunnel: None,
        }
    }

    pub(crate) fn public_base_url(&self) -> Option<&str> {
        self.request.public_base_url.as_deref()
    }

    pub(crate) const fn request(&self) -> &CreateWebShareRequest {
        &self.request
    }

    pub(crate) fn tunnel_provider(&self) -> Option<&str> {
        self.request.tunnel_provider.as_deref()
    }

    pub(crate) fn with_tunnel(mut self, tunnel: TunnelInfo) -> Self {
        self.request.public_base_url = Some(tunnel.public_url.clone());
        self.request.tunnel_provider = None;
        self.tunnel = Some(tunnel);
        self
    }

    pub(crate) fn expiry_kill_target(&self) -> Option<WebSessionTarget> {
        if !self.request.kill_session_on_expire {
            return None;
        }
        match &self.target {
            WebShareTarget::Session(target) => Some(target.clone()),
            WebShareTarget::Pane(_) => None,
        }
    }

    pub(crate) const fn target(&self) -> &WebShareTarget {
        &self.target
    }
}

#[cfg(test)]
impl From<CreateWebShareRequest> for ResolvedCreateWebShareRequest {
    fn from(request: CreateWebShareRequest) -> Self {
        let target = match &request.scope {
            rmux_proto::WebShareScope::Pane(target) => {
                WebShareTarget::pane(target.clone(), rmux_proto::SessionId::new(0))
            }
            rmux_proto::WebShareScope::Session(name) => {
                WebShareTarget::session(name.clone(), rmux_proto::SessionId::new(0))
            }
        };
        Self {
            request,
            target,
            tunnel: None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct WebShareRegistry {
    auth_wait_limit: std::sync::Arc<AuthWaitLimit>,
    backoff: AuthBackoff,
    connection_limit: std::sync::Arc<ConnectionLimit>,
    inner: Mutex<WebShareState>,
    next_id: AtomicU64,
    settings: Mutex<WebShareSettings>,
}

impl Default for WebShareRegistry {
    fn default() -> Self {
        Self::new(WebShareSettings::default())
    }
}

impl WebShareRegistry {
    #[cfg(test)]
    pub(crate) fn new_with_authenticated_connection_limit(max_connections: usize) -> Self {
        Self::new_with_authentication_limits(
            max_connections,
            DEFAULT_AUTH_WAIT_LIMIT,
            DEFAULT_AUTH_WAIT_PER_KEY_LIMIT,
            DEFAULT_AUTH_WAIT_PER_PEER_LIMIT,
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_authentication_limits(
        max_connections: usize,
        max_waiters: usize,
        max_waiters_per_key: usize,
        max_waiters_per_peer: usize,
    ) -> Self {
        Self {
            auth_wait_limit: AuthWaitLimit::new(
                max_waiters,
                max_waiters_per_key,
                max_waiters_per_peer,
            ),
            backoff: AuthBackoff::new(),
            connection_limit: ConnectionLimit::new(max_connections),
            inner: Mutex::new(WebShareState::default()),
            next_id: AtomicU64::new(1),
            settings: Mutex::new(WebShareSettings::default()),
        }
    }

    #[cfg(test)]
    pub(crate) fn authenticated_connection_count(&self) -> usize {
        self.connection_limit.active_count()
    }

    #[cfg(test)]
    pub(crate) fn authentication_wait_count(&self) -> usize {
        self.auth_wait_limit.active_count()
    }

    pub(crate) fn new(settings: WebShareSettings) -> Self {
        Self {
            auth_wait_limit: AuthWaitLimit::new(
                DEFAULT_AUTH_WAIT_LIMIT,
                DEFAULT_AUTH_WAIT_PER_KEY_LIMIT,
                DEFAULT_AUTH_WAIT_PER_PEER_LIMIT,
            ),
            backoff: AuthBackoff::new(),
            connection_limit: ConnectionLimit::new(DEFAULT_AUTHENTICATED_CONNECTION_LIMIT),
            inner: Mutex::new(WebShareState::default()),
            next_id: AtomicU64::new(1),
            settings: Mutex::new(settings),
        }
    }

    pub(crate) fn handle(
        &self,
        request: rmux_proto::WebShareRequest,
    ) -> Result<WebShareResponse, RmuxError> {
        match request {
            rmux_proto::WebShareRequest::Create(_) => Err(RmuxError::Server(
                "web-share create requires a resolved server target".to_owned(),
            )),
            rmux_proto::WebShareRequest::List(request) => {
                Ok(WebShareResponse::List(self.list(request)))
            }
            rmux_proto::WebShareRequest::Stop(request) => {
                Ok(WebShareResponse::Stopped(self.stop(request)))
            }
            rmux_proto::WebShareRequest::StopAll(request) => {
                Ok(WebShareResponse::StoppedAll(self.stop_all(request)))
            }
            rmux_proto::WebShareRequest::Lookup(request) => {
                Ok(WebShareResponse::Lookup(self.lookup(request)))
            }
            rmux_proto::WebShareRequest::Config(request) => {
                self.config(request).map(WebShareResponse::Config)
            }
        }
    }

    pub(crate) fn poll_expiry(&self, share_id: &str) -> WebShareExpiryPoll {
        self.inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .poll_expiry(share_id)
    }

    pub(crate) fn expiry_revoke_receiver(
        &self,
        share_id: &str,
    ) -> Option<watch::Receiver<Option<WebShareRevokeReason>>> {
        self.inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .records
            .get(share_id)
            .map(|record| record.revoke_tx.subscribe())
    }

    pub(crate) fn list(&self, _request: ListWebSharesRequest) -> WebShareListResponse {
        let mut inner = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned");
        inner.prune_expired();
        let shares = inner.summaries();
        WebShareListResponse {
            output: list_output(&shares),
            shares,
        }
    }

    pub(crate) fn stop(&self, request: StopWebShareRequest) -> WebShareStoppedResponse {
        let stopped = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .remove(&request.share_id, WebShareRevokeReason::StoppedByOwner);
        if stopped {
            info!(share_id = %request.share_id, reason = "cli_stop", "web_share_stopped");
        }
        WebShareStoppedResponse {
            output: stopped_output(&request.share_id, stopped),
            share_id: request.share_id,
            stopped,
        }
    }

    pub(crate) fn discard_undelivered(&self, share_id: &str) {
        let removed = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .remove(share_id, WebShareRevokeReason::StoppedByOwner);
        if removed {
            info!(
                share_id,
                reason = "response_undelivered",
                "web_share_stopped"
            );
        }
    }

    pub(crate) fn stop_all(&self, _request: StopAllWebSharesRequest) -> WebShareStoppedAllResponse {
        let stopped = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .clear(WebShareRevokeReason::StoppedByOwner);
        if stopped > 0 {
            info!(stopped, reason = "cli_stop_all", "web_share_stop_all");
        }
        WebShareStoppedAllResponse {
            output: CommandOutput::from_stdout(format!("stopped {stopped}\n")),
            stopped,
        }
    }

    pub(crate) fn remove_targets_for_sessions(&self, sessions: &[(SessionName, SessionId)]) -> u32 {
        if sessions.is_empty() {
            return 0;
        }
        let removed = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .remove_targets_for_sessions(sessions, WebShareRevokeReason::SessionGone);
        if removed > 0 {
            info!(removed, reason = "session_removed", "web_share_pruned");
        }
        removed
    }

    pub(crate) fn remove_targets_for_panes(&self, pane_ids: &[PaneId]) -> u32 {
        if pane_ids.is_empty() {
            return 0;
        }
        let removed = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .remove_targets_for_panes(pane_ids, WebShareRevokeReason::PaneGone);
        if removed > 0 {
            info!(removed, reason = "pane_removed", "web_share_pruned");
        }
        removed
    }

    pub(crate) fn rename_session_targets(
        &self,
        old_name: &SessionName,
        new_name: &SessionName,
        session_id: SessionId,
    ) -> u32 {
        let renamed = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .rename_session_targets(old_name, new_name, session_id);
        if renamed > 0 {
            info!(renamed, reason = "session_renamed", "web_share_retargeted");
        }
        renamed
    }

    pub(crate) fn lookup(&self, request: LookupWebShareRequest) -> WebShareLookupResponse {
        let mut inner = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned");
        inner.prune_expired();
        let share = inner.summary(&request.share_id);
        WebShareLookupResponse {
            output: lookup_output(share.as_ref()),
            share,
        }
    }

    pub(crate) fn config(
        &self,
        _request: WebShareConfigRequest,
    ) -> Result<WebShareConfigResponse, RmuxError> {
        self.require_listener_available()?;
        let listener = self.listener();
        Ok(WebShareConfigResponse {
            output: CommandOutput::from_stdout(format!(
                "{}:{} {}\n",
                listener.host, listener.port, listener.frontend_origin
            )),
            listener,
        })
    }

    pub(crate) fn listener(&self) -> WebShareListener {
        self.settings().listener()
    }

    pub(crate) fn listener_available(&self) -> bool {
        self.inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .listener
            .is_available()
    }

    pub(crate) fn settings(&self) -> WebShareSettings {
        self.settings
            .lock()
            .expect("web-share settings mutex must not be poisoned")
            .clone()
    }

    pub(crate) fn update_listener_port(&self, port: u16) {
        self.settings
            .lock()
            .expect("web-share settings mutex must not be poisoned")
            .set_port(port);
    }

    pub(crate) fn mark_listener_available(&self) {
        self.inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .listener = WebListenerState::Available;
    }

    pub(crate) fn mark_listener_unavailable(&self, reason: impl Into<String>) {
        self.inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .listener = WebListenerState::Unavailable(reason.into());
    }

    fn require_listener_available(&self) -> Result<(), RmuxError> {
        let inner = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned");
        match &inner.listener {
            WebListenerState::Available => Ok(()),
            WebListenerState::Unavailable(reason) => Err(RmuxError::Server(format!(
                "web-share listener unavailable: {reason}"
            ))),
        }
    }
}
