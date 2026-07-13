use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rmux_proto::{
    CreateWebShareRequest, Request, Response, WebShareRequest, WebShareResponse, WebShareScope,
    WebShareUrlOptions, WebTerminalPalette, WebTerminalTheme,
};

use crate::handles::{Pane, Session};
use crate::transport::TransportClient;
use crate::{Result, RmuxError};

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
        require_web_share(self.transport).await?;
        if let Some(pane) = self.pane {
            self.scope = WebShareScope::Pane(pane.required_resolved_proto_target_ref().await?);
        }
        let controls = self.operator && matches!(&self.scope, WebShareScope::Session(_));
        let response = self
            .transport
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
                        terminal_theme: self.terminal_theme,
                        ..self.url_options
                    },
                    require_pin: self.require_pin,
                    operator_pin: self.operator_pin,
                    spectator_pin: self.spectator_pin,
                    terminal_palette: self.terminal_palette.map(Box::new),
                    operator: self.operator,
                    spectator: self.spectator,
                    controls,
                    kill_session_on_expire: self.kill_session_on_expire,
                },
            ))))
            .await?;
        match response {
            Response::WebShare(response) => match *response {
                WebShareResponse::Created(created) => {
                    Ok(WebShareHandle::new(self.transport.clone(), created))
                }
                other => Err(unexpected_response(
                    "web-share create",
                    Response::WebShare(Box::new(other)),
                )),
            },
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
        WebShareBuilder::new_pane(self)
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
            RmuxError::protocol(rmux_proto::RmuxError::Server(
                "web-share expiration must not be before the Unix epoch".to_owned(),
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::{system_time_to_unix, whole_seconds_ceil, WebShareBuilder};
    use crate::transport::TransportClient;
    use crate::{Pane, PaneRef, RmuxEndpoint};
    use rmux_proto::{
        encode_frame, CommandOutput, ErrorResponse, FrameDecoder, HandshakeResponse,
        ListPanesResponse, PaneId, PaneTargetRef, Request, Response, SessionName, WebShareRequest,
        WebShareScope,
    };
    use std::time::{Duration, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    async fn read_request(stream: &mut DuplexStream) -> Request {
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0_u8; 1024];
        loop {
            if let Some(request) = decoder
                .next_frame::<Request>()
                .expect("request frame decodes")
            {
                return request;
            }
            let read = stream.read(&mut buffer).await.expect("read request");
            assert_ne!(read, 0, "client closed before request");
            decoder.push_bytes(&buffer[..read]);
        }
    }

    async fn write_response(stream: &mut DuplexStream, response: Response) {
        let frame = encode_frame(&response).expect("response encodes");
        stream.write_all(&frame).await.expect("write response");
        stream.flush().await.expect("flush response");
    }

    #[test]
    fn ttl_ceil_rejects_only_explicit_zero_later() {
        assert_eq!(whole_seconds_ceil(Duration::ZERO), 0);
        assert_eq!(whole_seconds_ceil(Duration::from_millis(1)), 1);
        assert_eq!(whole_seconds_ceil(Duration::from_secs(3)), 3);
        assert_eq!(whole_seconds_ceil(Duration::new(3, 1)), 4);
    }

    #[test]
    fn system_time_to_unix_returns_seconds() {
        assert_eq!(
            system_time_to_unix(UNIX_EPOCH + Duration::from_secs(42)).expect("valid deadline"),
            42
        );
    }

    #[test]
    fn system_time_to_unix_rejects_pre_epoch_deadlines() {
        let error = system_time_to_unix(UNIX_EPOCH - Duration::from_secs(1))
            .expect_err("pre-epoch deadline must be rejected locally");
        assert!(error
            .to_string()
            .contains("web-share expiration must not be before the Unix epoch"));
    }

    #[tokio::test]
    async fn positive_compat_aliases_restore_default_web_share_choices() {
        let (client, _server) = tokio::io::duplex(64);
        let transport = TransportClient::spawn(client);
        let scope = WebShareScope::Session(SessionName::new("alpha").expect("valid session"));
        let builder = WebShareBuilder::new(&transport, scope)
            .hide_viewers()
            .show_viewer_count()
            .no_pin()
            .pairing_code();

        assert!(builder.url_options.show_viewers);
        assert!(builder.require_pin);
    }

    #[tokio::test]
    async fn pane_share_resolves_visible_slot_to_stable_id_before_create() {
        let alpha = SessionName::new("alpha").expect("valid session");
        let (client, mut server) = tokio::io::duplex(8192);
        let transport = TransportClient::spawn(client);
        let pane = Pane::new(
            PaneRef::new(alpha.clone(), 1, 3),
            RmuxEndpoint::Default,
            None,
            transport,
        );

        let share = tokio::spawn(async move { pane.share().no_pin().await });

        assert!(matches!(
            read_request(&mut server).await,
            Request::Handshake(_)
        ));
        let mut handshake = HandshakeResponse::current();
        if !handshake
            .capabilities
            .iter()
            .any(|capability| capability == rmux_proto::CAPABILITY_WEB_SHARE)
        {
            handshake
                .capabilities
                .push(rmux_proto::CAPABILITY_WEB_SHARE.to_owned());
        }
        write_response(&mut server, Response::Handshake(handshake)).await;

        match read_request(&mut server).await {
            Request::ListPanes(request) => {
                assert_eq!(request.target, alpha);
                assert_eq!(request.target_window_index, Some(1));
            }
            request => panic!("expected visible slot resolution, got {request:?}"),
        }
        write_response(
            &mut server,
            Response::ListPanes(ListPanesResponse {
                output: CommandOutput::from_stdout("1:3:%7\n"),
            }),
        )
        .await;

        match read_request(&mut server).await {
            Request::WebShare(request) => match *request {
                WebShareRequest::Create(request) => assert_eq!(
                    request.scope,
                    WebShareScope::Pane(PaneTargetRef::by_id(
                        SessionName::new("alpha").expect("valid session"),
                        PaneId::new(7),
                    ))
                ),
                request => panic!("expected web-share create, got {request:?}"),
            },
            request => panic!("expected web-share request, got {request:?}"),
        }
        write_response(
            &mut server,
            Response::Error(ErrorResponse {
                error: rmux_proto::RmuxError::Server("stop after target assertion".to_owned()),
            }),
        )
        .await;

        let result = share.await.expect("share task completes");
        let Err(error) = result else {
            panic!("stub server must stop the share after observing the request");
        };
        assert!(error.to_string().contains("stop after target assertion"));
    }
}
