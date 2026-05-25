//! Browser-visible pane sharing helpers.

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::time::Duration;

use rmux_proto::{
    CreateWebShareRequest, ListWebSharesRequest, LookupWebShareRequest, PaneTargetRef, Request,
    Response, StopAllWebSharesRequest, StopWebShareRequest, WebShareConfigRequest,
    WebShareListener, WebShareRequest, WebShareResponse, WebShareScope, WebShareUrlOptions,
    WebTerminalPalette, WebTerminalTheme, CAPABILITY_WEB_SHARE,
};

use crate::handles::{Pane, Rmux, Session};
use crate::transport::TransportClient;
use crate::{Result, RmuxError};

/// Builder for creating one browser-visible pane or session share.
pub struct WebShareBuilder<'a> {
    transport: &'a TransportClient,
    scope: WebShareScope,
    frontend_url: Option<String>,
    public_base_url: Option<String>,
    ttl_seconds: Option<u64>,
    max_readers: Option<u16>,
    url_options: WebShareUrlOptions,
    require_pin: bool,
    terminal_theme: Option<WebTerminalTheme>,
    terminal_palette: Option<WebTerminalPalette>,
    writable: bool,
    controls: bool,
}

impl<'a> WebShareBuilder<'a> {
    pub(crate) fn new(transport: &'a TransportClient, scope: WebShareScope) -> Self {
        Self {
            transport,
            scope,
            frontend_url: None,
            public_base_url: None,
            ttl_seconds: None,
            max_readers: None,
            url_options: WebShareUrlOptions::default(),
            require_pin: false,
            terminal_theme: None,
            terminal_palette: None,
            writable: false,
            controls: false,
        }
    }

    /// Sets the maximum lifetime for the share.
    #[must_use]
    pub fn ttl(mut self, duration: Duration) -> Self {
        self.ttl_seconds = Some(whole_seconds_ceil(duration));
        self
    }

    /// Sets the maximum number of concurrent read-only clients.
    #[must_use]
    pub const fn max_readers(mut self, max_readers: u16) -> Self {
        self.max_readers = Some(max_readers);
        self
    }

    /// Sets the browser frontend URL used for this share.
    #[must_use]
    pub fn frontend_url(mut self, url: impl Into<String>) -> Self {
        self.frontend_url = Some(url.into());
        self
    }

    /// Sets the public tunnel origin used by the frontend.
    #[must_use]
    pub fn tunnel_url(mut self, url: impl Into<String>) -> Self {
        self.public_base_url = Some(url.into());
        self
    }

    /// Sets the public WS origin used by the hosted frontend.
    #[must_use]
    pub fn public_url(mut self, url: impl Into<String>) -> Self {
        self.public_base_url = Some(url.into());
        self
    }

    /// Hides the browser navigation bar in generated share URLs.
    #[must_use]
    pub const fn no_navbar(mut self) -> Self {
        self.url_options.no_navbar = true;
        self
    }

    /// Suppresses the client-side privacy/disclaimer toast in generated share URLs.
    #[must_use]
    pub const fn no_disclaimer(mut self) -> Self {
        self.url_options.no_disclaimer = true;
        self
    }

    /// Requires an out-of-band pairing code in addition to the URL secret.
    #[must_use]
    pub const fn pin(mut self) -> Self {
        self.require_pin = true;
        self
    }

    /// Alias for [`Self::pin`].
    #[must_use]
    pub const fn pairing_code(self) -> Self {
        self.pin()
    }

    /// Sets the initial browser terminal theme for generated share URLs.
    #[must_use]
    pub const fn theme(mut self, theme: WebTerminalTheme) -> Self {
        self.terminal_theme = Some(theme);
        self
    }

    /// Alias for [`Self::theme`].
    #[must_use]
    pub const fn terminal_theme(self, theme: WebTerminalTheme) -> Self {
        self.theme(theme)
    }

    /// Uses the owner's captured terminal palette when available.
    #[must_use]
    pub const fn user_theme(self) -> Self {
        self.theme(WebTerminalTheme::User)
    }

    /// Uses the bundled light browser terminal palette.
    #[must_use]
    pub const fn light_theme(self) -> Self {
        self.theme(WebTerminalTheme::Light)
    }

    /// Uses the bundled dark browser terminal palette.
    #[must_use]
    pub const fn dark_theme(self) -> Self {
        self.theme(WebTerminalTheme::Dark)
    }

    /// Supplies a captured terminal palette for the browser "User" theme.
    #[must_use]
    pub fn terminal_palette(mut self, palette: WebTerminalPalette) -> Self {
        self.terminal_palette = Some(palette);
        self
    }

    /// Enables the single-operator writable URL.
    #[must_use]
    pub const fn writable(mut self) -> Self {
        self.writable = true;
        self
    }

    /// Enables remote rmux controls for session shares.
    ///
    /// Controls require a session share and imply writable operator access.
    #[must_use]
    pub const fn controls(mut self) -> Self {
        self.writable = true;
        self.controls = true;
        self
    }

    /// Keeps the share read-only.
    #[must_use]
    pub const fn read_only(mut self) -> Self {
        self.writable = false;
        self.controls = false;
        self
    }

    async fn run(self) -> Result<WebShareHandle> {
        require_web_share(self.transport).await?;
        let response = self
            .transport
            .request(Request::WebShare(WebShareRequest::Create(
                CreateWebShareRequest {
                    scope: self.scope,
                    public_base_url: self.public_base_url,
                    frontend_url: self.frontend_url,
                    ttl_seconds: self.ttl_seconds,
                    max_readers: self.max_readers,
                    url_options: WebShareUrlOptions {
                        terminal_theme: self.terminal_theme,
                        ..self.url_options
                    },
                    require_pin: self.require_pin,
                    terminal_palette: self.terminal_palette.map(Box::new),
                    writable: self.writable,
                    controls: self.controls,
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
    fn new(transport: TransportClient, created: rmux_proto::WebShareCreatedResponse) -> Self {
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
    fn new(transport: TransportClient, summary: WebShareSummary) -> Self {
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
        match &self.summary.scope {
            WebShareScope::Pane(target) => Some(target),
            WebShareScope::Session(_) => None,
        }
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
    /// Starts a web-share builder for this session.
    #[must_use]
    pub fn share(&self) -> WebShareBuilder<'_> {
        WebShareBuilder::new(
            self.transport(),
            WebShareScope::Session(self.name().clone()),
        )
    }
}

impl Pane {
    /// Starts a web-share builder for this pane.
    #[must_use]
    pub fn share(&self) -> WebShareBuilder<'_> {
        WebShareBuilder::new(
            self.transport(),
            WebShareScope::Pane(self.proto_target_ref()),
        )
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
    url.split_once("key=")
        .map(|(_, key)| key.split('&').next().unwrap_or(key))
}

fn whole_seconds_ceil(duration: Duration) -> u64 {
    if duration.is_zero() {
        0
    } else {
        duration
            .as_secs()
            .saturating_add(u64::from(duration.subsec_nanos() > 0))
    }
}

fn unexpected_response(operation: &str, response: Response) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
        "rmux daemon sent `{}` response for {operation}",
        response.command_name()
    )))
}

#[cfg(test)]
mod tests {
    use super::whole_seconds_ceil;
    use std::time::Duration;

    #[test]
    fn ttl_ceil_rejects_only_explicit_zero_later() {
        assert_eq!(whole_seconds_ceil(Duration::ZERO), 0);
        assert_eq!(whole_seconds_ceil(Duration::from_millis(1)), 1);
        assert_eq!(whole_seconds_ceil(Duration::from_secs(3)), 3);
        assert_eq!(whole_seconds_ceil(Duration::new(3, 1)), 4);
    }
}
