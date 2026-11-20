use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rmux_proto::{
    CreateWebShareRequest, Request, Response, WebShareRequest, WebShareResponse, WebShareScope,
    WebShareUrlOptions,
};

use crate::handles::{Pane, Session};
use crate::transport::TransportClient;
use crate::{Result, RmuxError, WebTerminalPalette, WebTerminalTheme};

use super::{require_web_share, unexpected_response, WebShareHandle};

/// Builder for creating one browser-visible pane or session share.
pub struct WebShareBuilder<'a> {
    transport: &'a TransportClient,
    scope: WebShareScope,
    pane: Option<&'a Pane>,
    frontend_url: Option<String>,
    public_base_url: Option<String>,
    tunnel_provider: Option<String>,
    ttl_seconds: Option<u64>,
    expires_at_unix: Option<u64>,
    max_operators: Option<u16>,
    max_spectators: Option<u16>,
    url_options: WebShareUrlOptions,
    require_pin: bool,
    operator_pin: Option<String>,
    spectator_pin: Option<String>,
    terminal_theme: Option<WebTerminalTheme>,
    terminal_palette: Option<WebTerminalPalette>,
    operator: bool,
    spectator: bool,
    kill_session_on_expire: bool,
}

impl<'a> WebShareBuilder<'a> {
    pub(crate) fn new(transport: &'a TransportClient, scope: WebShareScope) -> Self {
        Self {
            transport,
            scope,
            pane: None,
            frontend_url: None,
            public_base_url: None,
            tunnel_provider: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_operators: None,
            max_spectators: None,
            url_options: WebShareUrlOptions::default(),
            require_pin: true,
            operator_pin: None,
            spectator_pin: None,
            terminal_theme: None,
            terminal_palette: None,
            operator: true,
            spectator: true,
            kill_session_on_expire: false,
        }
    }

    fn new_pane(pane: &'a Pane) -> Self {
        let mut builder = Self::new(
            pane.transport(),
            WebShareScope::Pane(pane.proto_target_ref()),
        );
        builder.pane = Some(pane);
        builder
    }

    /// Sets the maximum lifetime for the share.
    #[must_use]
    pub fn ttl(mut self, duration: Duration) -> Self {
        self.ttl_seconds = Some(whole_seconds_ceil(duration));
        self.expires_at_unix = None;
        self
    }

    /// Sets an absolute expiration time for the share.
    pub fn expires_at(mut self, deadline: SystemTime) -> Result<Self> {
        self.expires_at_unix = Some(system_time_to_unix(deadline)?);
        self.ttl_seconds = None;
        Ok(self)
    }

    /// Sets the maximum number of concurrent spectator clients.
    #[must_use]
    pub const fn max_spectators(mut self, max_spectators: u16) -> Self {
        self.max_spectators = Some(max_spectators);
        self
    }

    /// Sets the maximum number of concurrent operator clients.
    #[must_use]
    pub const fn max_operators(mut self, max_operators: u16) -> Self {
        self.max_operators = Some(max_operators);
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
        self.tunnel_provider = None;
        self
    }

    /// Spawns a named daemon-side tunnel preset for this share.
    #[must_use]
    pub fn tunnel_provider(mut self, provider: impl Into<String>) -> Self {
        self.tunnel_provider = Some(provider.into());
        self.public_base_url = None;
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

    /// Hides the live connected browser count in generated share URLs.
    #[must_use]
    pub const fn hide_viewers(mut self) -> Self {
        self.url_options.show_viewers = false;
        self
    }

    /// Shows the live connected browser count in generated share URLs.
    #[must_use]
    pub const fn show_viewers(mut self) -> Self {
        self.url_options.show_viewers = true;
        self
    }

    /// Alias for [`Self::show_viewers`].
    #[must_use]
    pub const fn show_viewer_count(self) -> Self {
        self.show_viewers()
    }

    /// Disables the out-of-band pairing code.
    #[must_use]
    pub const fn no_pin(mut self) -> Self {
        self.require_pin = false;
        self
    }

    /// Requires the out-of-band pairing code.
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

    /// Supplies the 6-digit operator pairing PIN instead of generating one.
    #[must_use]
    pub fn operator_pin(mut self, pin: impl Into<String>) -> Self {
        self.operator_pin = Some(pin.into());
        self
    }

    /// Supplies the 6-digit spectator pairing PIN instead of generating one.
    #[must_use]
    pub fn spectator_pin(mut self, pin: impl Into<String>) -> Self {
        self.spectator_pin = Some(pin.into());
        self
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

    /// Mints only the operator URL.
    #[must_use]
    pub const fn operator_only(mut self) -> Self {
        self.operator = true;
        self.spectator = false;
        self
    }

    /// Mints only the spectator URL.
    #[must_use]
    pub const fn spectator_only(mut self) -> Self {
        self.operator = false;
        self.spectator = true;
        self
    }

    /// Kills the target session when this share expires.
    ///
    /// The daemon rejects this option for pane shares.
    #[must_use]
    pub const fn kill_session_on_expire(mut self, enabled: bool) -> Self {
        self.kill_session_on_expire = enabled;
        self
    }

    async fn run(mut self) -> Result<WebShareHandle> {
        let operation_pane = self.pane.map(Pane::begin_operation_handle);
        let transport = operation_pane.as_ref().map_or_else(
            || self.transport.begin_operation(),
            |pane| pane.transport().clone(),
        );
        require_web_share(&transport).await?;
        if let Some(pane) = operation_pane {
            self.scope = WebShareScope::Pane(pane.required_resolved_proto_target_ref().await?);
        }
        let controls = self.operator && matches!(&self.scope, WebShareScope::Session(_));
        let response = transport
            .request(Request::WebShare(Box::new(WebShareRequest::Create(
                CreateWebShareRequest {
                    scope: self.scope,
                    public_base_url: self.public_base_url,
                    tunnel_provider: self.tunnel_provider,
                    frontend_url: self.frontend_url,
                    ttl_seconds: self.ttl_seconds,
                    expires_at_unix: self.expires_at_unix,
                    max_spectators: self.max_spectators,
                    max_operators: self.max_operators,
                    url_options: WebShareUrlOptions {
                        terminal_theme: self.terminal_theme.map(theme_to_proto),
                        ..self.url_options
                    },
                    require_pin: self.require_pin,
                    operator_pin: self.operator_pin,
                    spectator_pin: self.spectator_pin,
                    terminal_palette: self
                        .terminal_palette
                        .map(|palette| Box::new(palette_to_proto(palette))),
                    operator: self.operator,
                    spectator: self.spectator,
                    controls,
                    kill_session_on_expire: self.kill_session_on_expire,
                },
            ))))
            .await?;
        match response {
            Response::WebShare(response) => match *response {
                WebShareResponse::Created(created) => Ok(WebShareHandle::new(transport, created)),
                other => Err(unexpected_response(
                    "web-share create",
                    Response::WebShare(Box::new(other)),
                )),
            },
            Response::Error(error) => Err(RmuxError::from_proto(error.error)),
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
        WebShareBuilder::new_pane(self)
    }
}

fn theme_to_proto(theme: WebTerminalTheme) -> rmux_proto::WebTerminalTheme {
    match theme {
        WebTerminalTheme::User => rmux_proto::WebTerminalTheme::User,
        WebTerminalTheme::Light => rmux_proto::WebTerminalTheme::Light,
        WebTerminalTheme::Dark => rmux_proto::WebTerminalTheme::Dark,
    }
}

fn palette_to_proto(palette: WebTerminalPalette) -> rmux_proto::WebTerminalPalette {
    rmux_proto::WebTerminalPalette {
        foreground: palette.foreground,
        background: palette.background,
        cursor: palette.cursor,
        ansi: palette.ansi,
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

fn system_time_to_unix(value: SystemTime) -> Result<u64> {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| {
            RmuxError::from_proto(rmux_proto::RmuxError::Server(
                "web-share expiration must not be before the Unix epoch".to_owned(),
            ))
        })
}

#[cfg(test)]
mod tests;
