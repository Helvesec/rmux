use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::time::Duration;

use rmux_proto::{
    CreateWebShareRequest, Request, Response, WebShareRequest, WebShareResponse, WebShareScope,
    WebShareUrlOptions, WebTerminalPalette, WebTerminalTheme,
};

use crate::handles::{Pane, Session};
use crate::transport::TransportClient;
use crate::Result;

use super::{require_web_share, unexpected_response, WebShareHandle};

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

fn whole_seconds_ceil(duration: Duration) -> u64 {
    if duration.is_zero() {
        0
    } else {
        duration
            .as_secs()
            .saturating_add(u64::from(duration.subsec_nanos() > 0))
    }
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
