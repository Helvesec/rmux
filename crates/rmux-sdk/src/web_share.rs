//! Browser-visible pane sharing helpers.

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::time::Duration;

use rmux_proto::{
    CreateWebShareRequest, ListWebSharesRequest, LookupWebShareRequest, PaneTarget, PaneTargetRef,
    Request, Response, StopAllWebSharesRequest, StopWebShareRequest, WebShareConfigRequest,
    WebShareListener, WebShareRequest, WebShareResponse, CAPABILITY_WEB_SHARE,
};

use crate::handles::{Pane, Rmux, Session};
use crate::transport::TransportClient;
use crate::{Result, RmuxError};

/// Builder for creating one browser-visible pane share.
pub struct WebShareBuilder<'a> {
    transport: &'a TransportClient,
    target: PaneTargetRef,
    public_base_url: Option<String>,
    ttl_seconds: Option<u64>,
    max_viewers: Option<u16>,
    writable: bool,
}

impl<'a> WebShareBuilder<'a> {
    pub(crate) fn new(transport: &'a TransportClient, target: PaneTargetRef) -> Self {
        Self {
            transport,
            target,
            public_base_url: None,
            ttl_seconds: None,
            max_viewers: None,
            writable: false,
        }
    }

    /// Sets the maximum lifetime for the share.
    #[must_use]
    pub fn ttl(mut self, duration: Duration) -> Self {
        self.ttl_seconds = Some(duration.as_secs());
        self
    }

    /// Sets the maximum number of concurrent read-only viewers.
    #[must_use]
    pub const fn max_viewers(mut self, max_viewers: u16) -> Self {
        self.max_viewers = Some(max_viewers);
        self
    }

    /// Sets the public origin used when generating browser URLs.
    #[must_use]
    pub fn public_url(mut self, url: impl Into<String>) -> Self {
        self.public_base_url = Some(url.into());
        self
    }

    /// Enables the single-operator writable URL.
    #[must_use]
    pub const fn writable(mut self) -> Self {
        self.writable = true;
        self
    }

    /// Keeps the share read-only.
    #[must_use]
    pub const fn read_only(mut self) -> Self {
        self.writable = false;
        self
    }

    async fn run(self) -> Result<WebShareHandle> {
        require_web_share(self.transport).await?;
        let response = self
            .transport
            .request(Request::WebShare(WebShareRequest::Create(
                CreateWebShareRequest {
                    target: self.target,
                    public_base_url: self.public_base_url,
                    ttl_seconds: self.ttl_seconds,
                    max_viewers: self.max_viewers,
                    writable: self.writable,
                },
            )))
            .await?;
        match response {
            Response::WebShare(WebShareResponse::Created(created)) => {
                Ok(WebShareHandle::new(self.transport.clone(), created))
            }
            Response::Error(error) => Err(error.into()),
            response => Err(unexpected_response("web-share create", response)),
        }
    }
}

impl<'a> IntoFuture for WebShareBuilder<'a> {
    type Output = Result<WebShareHandle>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.run())
    }
}

/// A share handle returned by a create operation.
#[derive(Clone)]
pub struct WebShareHandle {
    transport: TransportClient,
    id: String,
    target: PaneTargetRef,
    viewer_url: String,
    operator_url: Option<String>,
    expires_at_unix: Option<u64>,
    max_viewers: u16,
    writable: bool,
}

impl WebShareHandle {
    fn new(transport: TransportClient, created: rmux_proto::WebShareCreatedResponse) -> Self {
        Self {
            transport,
            id: created.share_id,
            target: created.target,
            viewer_url: created.viewer_url,
            operator_url: created.operator_url,
            expires_at_unix: created.expires_at_unix,
            max_viewers: created.max_viewers,
            writable: created.writable,
        }
    }

    /// Returns the opaque share id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the pane target resolved by the daemon at create time.
    #[must_use]
    pub const fn target(&self) -> &PaneTargetRef {
        &self.target
    }

    /// Returns whether this share minted an operator URL.
    #[must_use]
    pub const fn writable(&self) -> bool {
        self.writable
    }

    /// Returns the read-only viewer URL.
    #[must_use]
    pub fn viewer_url(&self) -> &str {
        &self.viewer_url
    }

    /// Returns the viewer key carried in the viewer URL, when present.
    #[must_use]
    pub fn viewer_key(&self) -> Option<&str> {
        key_from_url(&self.viewer_url)
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

    /// Returns the effective viewer cap.
    #[must_use]
    pub const fn max_viewers(&self) -> u16 {
        self.max_viewers
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

    /// Returns the current number of read-only viewers.
    pub async fn viewers_active(&self) -> Result<u16> {
        Ok(self.summary().await?.active_viewers)
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
    fn new(transport: TransportClient, summary: WebShareSummary) -> Self {
        Self { transport, summary }
    }

    /// Returns the opaque share id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.summary.id
    }

    /// Returns the pane target resolved by the daemon at create time.
    #[must_use]
    pub const fn target(&self) -> &PaneTargetRef {
        &self.summary.target
    }

    /// Returns whether this share has an operator URL.
    #[must_use]
    pub const fn writable(&self) -> bool {
        self.summary.writable
    }

    /// Returns the redacted viewer URL, when available.
    #[must_use]
    pub fn viewer_url_redacted(&self) -> Option<&str> {
        self.summary.viewer_url_redacted.as_deref()
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

/// Redacted metadata for an active browser-visible pane share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebShareSummary {
    /// Opaque share id.
    pub id: String,
    /// Shared pane target.
    pub target: PaneTargetRef,
    /// Redacted viewer URL, if available.
    pub viewer_url_redacted: Option<String>,
    /// Whether this share has an operator URL.
    pub writable: bool,
    /// Active read-only viewer count.
    pub active_viewers: u16,
    /// Maximum read-only viewers allowed.
    pub max_viewers: u16,
    /// Whether the single operator slot is occupied.
    pub operator_connected: bool,
    /// Expiration timestamp in UNIX seconds.
    pub expires_at_unix: Option<u64>,
}

impl From<rmux_proto::WebShareSummary> for WebShareSummary {
    fn from(value: rmux_proto::WebShareSummary) -> Self {
        Self {
            id: value.share_id,
            target: value.target,
            viewer_url_redacted: value.viewer_url,
            writable: value.writable,
            active_viewers: value.active_viewers,
            max_viewers: value.max_viewers,
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
    /// Origin used by the embedded frontend.
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

impl Session {
    /// Starts a web-share builder for this session's first pane.
    #[must_use]
    pub fn share(&self) -> WebShareBuilder<'_> {
        WebShareBuilder::new(
            self.transport(),
            PaneTargetRef::slot(PaneTarget::new(self.name().clone(), 0)),
        )
    }
}

impl Pane {
    /// Starts a web-share builder for this pane.
    #[must_use]
    pub fn share(&self) -> WebShareBuilder<'_> {
        WebShareBuilder::new(self.transport(), self.proto_target_ref())
    }
}

async fn list_web_shares(transport: &TransportClient) -> Result<Vec<WebShareSummary>> {
    require_web_share(transport).await?;
    let response = transport
        .request(Request::WebShare(WebShareRequest::List(
            ListWebSharesRequest,
        )))
        .await?;
    match response {
        Response::WebShare(WebShareResponse::List(response)) => {
            Ok(response.shares.into_iter().map(Into::into).collect())
        }
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share list", response)),
    }
}

async fn stop_web_share(transport: &TransportClient, id: &str) -> Result<bool> {
    require_web_share(transport).await?;
    let response = transport
        .request(Request::WebShare(WebShareRequest::Stop(
            StopWebShareRequest {
                share_id: id.to_owned(),
            },
        )))
        .await?;
    match response {
        Response::WebShare(WebShareResponse::Stopped(response)) => Ok(response.stopped),
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share stop", response)),
    }
}

async fn stop_all_web_shares(transport: &TransportClient) -> Result<usize> {
    require_web_share(transport).await?;
    let response = transport
        .request(Request::WebShare(WebShareRequest::StopAll(
            StopAllWebSharesRequest,
        )))
        .await?;
    match response {
        Response::WebShare(WebShareResponse::StoppedAll(response)) => {
            Ok(usize::try_from(response.stopped).unwrap_or(usize::MAX))
        }
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share stop-all", response)),
    }
}

async fn lookup_summary(transport: &TransportClient, id: &str) -> Result<WebShareSummary> {
    require_web_share(transport).await?;
    let response = transport
        .request(Request::WebShare(WebShareRequest::Lookup(
            LookupWebShareRequest {
                share_id: id.to_owned(),
            },
        )))
        .await?;
    match response {
        Response::WebShare(WebShareResponse::Lookup(response)) => {
            response.share.map(Into::into).ok_or_else(|| {
                RmuxError::protocol(rmux_proto::RmuxError::Server(
                    "web share not found".to_owned(),
                ))
            })
        }
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share lookup", response)),
    }
}

async fn web_config(transport: &TransportClient) -> Result<WebConfigInfo> {
    require_web_share(transport).await?;
    let response = transport
        .request(Request::WebShare(WebShareRequest::Config(
            WebShareConfigRequest,
        )))
        .await?;
    match response {
        Response::WebShare(WebShareResponse::Config(response)) => Ok(response.listener.into()),
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share config", response)),
    }
}

async fn require_web_share(transport: &TransportClient) -> Result<()> {
    crate::capabilities::require(transport, &[CAPABILITY_WEB_SHARE]).await
}

fn key_from_url(url: &str) -> Option<&str> {
    url.split_once("?key=")
        .map(|(_, key)| key.split('&').next().unwrap_or(key))
}

fn unexpected_response(operation: &str, response: Response) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
        "rmux daemon sent `{}` response for {operation}",
        response.command_name()
    )))
}
