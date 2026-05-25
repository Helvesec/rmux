use rmux_proto::{PaneTargetRef, WebShareScope};

use crate::handles::Rmux;
use crate::transport::TransportClient;
use crate::Result;

use super::{
    key_from_url, list_web_shares, lookup_summary, stop_all_web_shares, stop_web_share, web_config,
    WebConfigInfo, WebShareSummary,
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
    read_url: String,
    operator_url: Option<String>,
    expires_at_unix: Option<u64>,
    pairing_code: Option<String>,
    max_readers: u16,
    writable: bool,
    controls: bool,
}

impl WebShareHandle {
    pub(crate) fn new(
        transport: TransportClient,
        created: rmux_proto::WebShareCreatedResponse,
    ) -> Self {
        Self {
            transport,
            id: created.share_id,
            scope: created.scope,
            read_url: created.read_url,
            operator_url: created.operator_url,
            expires_at_unix: created.expires_at_unix,
            pairing_code: created.pairing_code,
            max_readers: created.max_readers,
            writable: created.writable,
            controls: created.controls,
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
    pub const fn writable(&self) -> bool {
        self.writable
    }

    /// Returns whether this share grants remote rmux controls.
    #[must_use]
    pub const fn controls(&self) -> bool {
        self.controls
    }

    /// Returns the read-only browser URL.
    #[must_use]
    pub fn read_url(&self) -> &str {
        &self.read_url
    }

    /// Returns the read-only key carried in the browser URL, when present.
    #[must_use]
    pub fn read_key(&self) -> Option<&str> {
        key_from_url(&self.read_url)
    }

    /// Returns the privileged operator URL, when this share is writable.
    #[must_use]
    pub fn operator_url(&self) -> Option<&str> {
        self.operator_url.as_deref()
    }

    /// Returns the operator key carried in the operator URL, when present.
    #[must_use]
    pub fn operator_key(&self) -> Option<&str> {
        self.operator_url.as_deref().and_then(key_from_url)
    }

    /// Returns the out-of-band pairing code required by this share, when requested.
    #[must_use]
    pub fn pairing_code(&self) -> Option<&str> {
        self.pairing_code.as_deref()
    }

    /// Returns the effective cap for concurrent read-only clients.
    #[must_use]
    pub const fn max_readers(&self) -> u16 {
        self.max_readers
    }

    /// Returns the expiration timestamp in UNIX seconds.
    #[must_use]
    pub const fn expires_at_unix(&self) -> Option<u64> {
        self.expires_at_unix
    }

    /// Fetches redacted live metadata for this share.
    pub async fn summary(&self) -> Result<WebShareSummary> {
        lookup_summary(&self.transport, &self.id).await
    }

    /// Returns the current number of read-only clients.
    pub async fn readers_active(&self) -> Result<u16> {
        Ok(self.summary().await?.active_readers)
    }

    /// Returns whether the single operator slot is occupied.
    pub async fn operator_connected(&self) -> Result<bool> {
        Ok(self.summary().await?.operator_connected)
    }

    /// Stops this share on the daemon.
    pub async fn stop(self) -> Result<()> {
        stop_web_share(&self.transport, &self.id).await.map(|_| ())
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
        Self { transport, summary }
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
    pub const fn writable(&self) -> bool {
        self.summary.writable
    }

    /// Returns whether this share grants remote rmux controls.
    #[must_use]
    pub const fn controls(&self) -> bool {
        self.summary.controls
    }

    /// Returns the redacted read-only URL, when available.
    #[must_use]
    pub fn read_url_redacted(&self) -> Option<&str> {
        self.summary.read_url_redacted.as_deref()
    }

    /// Returns the cached summary from the lookup response.
    #[must_use]
    pub const fn cached_summary(&self) -> &WebShareSummary {
        &self.summary
    }

    /// Fetches fresh redacted metadata for this share.
    pub async fn summary(&self) -> Result<WebShareSummary> {
        lookup_summary(&self.transport, &self.summary.id).await
    }

    /// Stops this share on the daemon.
    pub async fn stop(self) -> Result<()> {
        stop_web_share(&self.transport, &self.summary.id)
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
