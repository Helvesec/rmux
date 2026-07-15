use rmux_proto::{PaneTargetRef, WebShareScope};

use crate::handles::Rmux;
use crate::transport::TransportClient;
use crate::Result;

use super::{
    list_web_shares, lookup_summary, stop_all_web_shares, stop_web_share, token_from_url,
    web_config, WebConfigInfo, WebShareSummary,
};

/// A share handle returned by a create operation.
///
/// Dropping this handle does not stop the daemon-side share. The share remains
/// active until its TTL expires, the shared pane or session goes away, or
/// [`Self::stop`] is called explicitly.
///
/// Cloned handles point at the same daemon share. Stopping one clone invalidates
/// the share for every other clone.
#[derive(Clone)]
pub struct WebShareHandle {
    transport: TransportClient,
    id: String,
    scope: WebShareScope,
    spectator_url: Option<String>,
    operator_url: Option<String>,
    expires_at_unix: Option<u64>,
    operator_pairing_code: Option<String>,
    spectator_pairing_code: Option<String>,
    max_operators: Option<u16>,
    max_spectators: Option<u16>,
    operator: bool,
    spectator: bool,
    kill_session_on_expire: bool,
}

impl WebShareHandle {
    pub(crate) fn new(
        transport: TransportClient,
        created: rmux_proto::WebShareCreatedResponse,
    ) -> Self {
        let transport = transport.reusable();
        Self {
            transport,
            id: created.share_id,
            scope: created.scope,
            spectator_url: created.spectator_url,
            operator_url: created.operator_url,
            expires_at_unix: created.expires_at_unix,
            operator_pairing_code: created.operator_pairing_code,
            spectator_pairing_code: created.spectator_pairing_code,
            max_operators: created.max_operators,
            max_spectators: created.max_spectators,
            operator: created.operator,
            spectator: created.spectator,
            kill_session_on_expire: created.kill_session_on_expire,
        }
    }

    /// Returns the opaque share id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the pane or session scope resolved by the daemon at create time.
    #[must_use]
    pub const fn scope(&self) -> &WebShareScope {
        &self.scope
    }

    /// Returns the pane target when this is a single-pane share.
    #[must_use]
    pub fn pane_target(&self) -> Option<&PaneTargetRef> {
        match &self.scope {
            WebShareScope::Pane(target) => Some(target),
            WebShareScope::Session(_) => None,
        }
    }

    /// Returns whether this share minted an operator URL.
    #[must_use]
    pub const fn operator(&self) -> bool {
        self.operator
    }

    /// Returns whether this share minted a spectator URL.
    #[must_use]
    pub const fn spectator(&self) -> bool {
        self.spectator
    }

    /// Returns whether this share kills its target session on expiry.
    #[must_use]
    pub const fn kill_session_on_expire(&self) -> bool {
        self.kill_session_on_expire
    }

    /// Returns the spectator browser URL.
    #[must_use]
    pub fn spectator_url(&self) -> Option<&str> {
        self.spectator_url.as_deref()
    }

    /// Returns the spectator capability token carried in the browser URL, when present.
    #[must_use]
    pub fn spectator_token(&self) -> Option<&str> {
        self.spectator_url.as_deref().and_then(token_from_url)
    }

    /// Returns the privileged operator URL, when this share has an operator.
    #[must_use]
    pub fn operator_url(&self) -> Option<&str> {
        self.operator_url.as_deref()
    }

    /// Returns the operator capability token carried in the operator URL, when present.
    #[must_use]
    pub fn operator_token(&self) -> Option<&str> {
        self.operator_url.as_deref().and_then(token_from_url)
    }

    /// Returns the out-of-band operator pairing code required by this share.
    #[must_use]
    pub fn operator_pairing_code(&self) -> Option<&str> {
        self.operator_pairing_code.as_deref()
    }

    /// Returns the out-of-band spectator pairing code required by this share.
    #[must_use]
    pub fn spectator_pairing_code(&self) -> Option<&str> {
        self.spectator_pairing_code.as_deref()
    }

    /// Returns the effective cap for concurrent spectator clients, when capped.
    #[must_use]
    pub const fn max_spectators(&self) -> Option<u16> {
        self.max_spectators
    }

    /// Returns the effective cap for concurrent operator clients, when capped.
    #[must_use]
    pub const fn max_operators(&self) -> Option<u16> {
        self.max_operators
    }

    /// Returns the expiration timestamp in UNIX seconds.
    #[must_use]
    pub const fn expires_at_unix(&self) -> Option<u64> {
        self.expires_at_unix
    }

    /// Fetches redacted live metadata for this share.
    pub async fn summary(&self) -> Result<WebShareSummary> {
        let transport = self.transport.begin_operation();
        lookup_summary(&transport, &self.id).await
    }

    /// Returns the current number of spectator clients.
    pub async fn spectators_active(&self) -> Result<u16> {
        Ok(self.summary().await?.active_spectators)
    }

    /// Returns the current number of operator clients.
    pub async fn operators_active(&self) -> Result<u16> {
        Ok(self.summary().await?.active_operators)
    }

    /// Stops this share on the daemon.
    pub async fn stop(self) -> Result<()> {
        let transport = self.transport.begin_operation();
        stop_web_share(&transport, &self.id).await.map(|_| ())
    }
}

/// Lookup handle for a share that may not have been created by this client.
#[derive(Clone)]
pub struct WebShareLookup {
    transport: TransportClient,
    summary: WebShareSummary,
}

impl WebShareLookup {
    pub(crate) fn new(transport: TransportClient, summary: WebShareSummary) -> Self {
        Self {
            transport: transport.reusable(),
            summary,
        }
    }

    /// Returns the opaque share id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.summary.id
    }

    /// Returns the pane or session scope resolved by the daemon at create time.
    #[must_use]
    pub const fn scope(&self) -> &WebShareScope {
        &self.summary.scope
    }

    /// Returns the pane target when this is a single-pane share.
    #[must_use]
    pub fn pane_target(&self) -> Option<&PaneTargetRef> {
        self.summary.pane_target()
    }

    /// Returns whether this share has an operator URL.
    #[must_use]
    pub const fn operator(&self) -> bool {
        self.summary.operator
    }

    /// Returns whether this share has a spectator URL.
    #[must_use]
    pub const fn spectator(&self) -> bool {
        self.summary.spectator
    }

    /// Returns the redacted spectator URL, when available.
    #[must_use]
    pub fn spectator_url_redacted(&self) -> Option<&str> {
        self.summary.spectator_url_redacted.as_deref()
    }

    /// Returns the cached summary from the lookup response.
    #[must_use]
    pub const fn cached_summary(&self) -> &WebShareSummary {
        &self.summary
    }

    /// Fetches fresh redacted metadata for this share.
    pub async fn summary(&self) -> Result<WebShareSummary> {
        let transport = self.transport.begin_operation();
        lookup_summary(&transport, &self.summary.id).await
    }

    /// Stops this share on the daemon.
    pub async fn stop(self) -> Result<()> {
        let transport = self.transport.begin_operation();
        stop_web_share(&transport, &self.summary.id)
            .await
            .map(|_| ())
    }
}

impl Rmux {
    /// Lists active web shares.
    pub async fn list_web_shares(&self) -> Result<Vec<WebShareSummary>> {
        let transport = self
            .connect_transport_for_operation(self.resolved_timeout(None))
            .await?;
        list_web_shares(&transport).await
    }

    /// Stops one web share by id and returns whether it existed.
    pub async fn stop_web_share(&self, id: &str) -> Result<bool> {
        let transport = self
            .connect_transport_for_operation(self.resolved_timeout(None))
            .await?;
        stop_web_share(&transport, id).await
    }

    /// Stops every active web share and returns the number stopped.
    pub async fn stop_all_web_shares(&self) -> Result<usize> {
        let transport = self
            .connect_transport_for_operation(self.resolved_timeout(None))
            .await?;
        stop_all_web_shares(&transport).await
    }

    /// Looks up one web share without exposing access keys.
    pub async fn web_share_by_id(&self, id: &str) -> Result<WebShareLookup> {
        let transport = self
            .connect_transport_for_operation(self.resolved_timeout(None))
            .await?;
        let summary = lookup_summary(&transport, id).await?;
        Ok(WebShareLookup::new(transport, summary))
    }

    /// Returns the active daemon web-share listener configuration.
    pub async fn web_config(&self) -> Result<WebConfigInfo> {
        let transport = self
            .connect_transport_for_operation(self.resolved_timeout(None))
            .await?;
        web_config(&transport).await
    }
}
