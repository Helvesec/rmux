use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{PaneTargetRef, SessionName};

/// Request payload for the `web-share` command family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebShareRequest {
    /// Create a new browser-visible share.
    Create(CreateWebShareRequest),
    /// List active web shares.
    List(ListWebSharesRequest),
    /// Stop one active web share.
    Stop(StopWebShareRequest),
    /// Stop every active web share.
    StopAll(StopAllWebSharesRequest),
    /// Lookup one active web share without exposing access keys.
    Lookup(LookupWebShareRequest),
    /// Return the daemon web-share listener configuration.
    Config(WebShareConfigRequest),
}

/// Request payload for `web-share`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateWebShareRequest {
    /// Browser-visible scope exposed by this share.
    pub scope: WebShareScope,
    /// Optional public WS origin forwarded to the daemon.
    #[serde(default)]
    pub public_base_url: Option<String>,
    /// Optional named tunnel preset spawned by the daemon.
    #[serde(default)]
    pub tunnel_provider: Option<String>,
    /// Optional browser frontend URL used for this share.
    #[serde(default)]
    pub frontend_url: Option<String>,
    /// Optional maximum share lifetime in seconds.
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
    /// Optional absolute expiration timestamp as UNIX seconds.
    #[serde(default)]
    pub expires_at_unix: Option<u64>,
    /// Optional cap for concurrent spectator clients.
    #[serde(default)]
    pub max_spectators: Option<u16>,
    /// Optional cap for concurrent operator clients.
    #[serde(default)]
    pub max_operators: Option<u16>,
    /// Presentation options encoded into generated spectator URLs.
    #[serde(default)]
    pub url_options: WebShareUrlOptions,
    /// Whether clients must provide the out-of-band pairing code during auth.
    #[serde(default = "default_true")]
    pub require_pin: bool,
    /// Optional operator pairing code supplied by the caller.
    #[serde(default)]
    pub operator_pin: Option<String>,
    /// Optional spectator pairing code supplied by the caller.
    #[serde(default)]
    pub spectator_pin: Option<String>,
    /// Terminal palette captured by the CLI for browser-side "User" theme.
    #[serde(default)]
    pub terminal_palette: Option<Box<WebTerminalPalette>>,
    /// Whether an operator URL should be minted.
    #[serde(default = "default_true")]
    pub operator: bool,
    /// Whether a spectator URL should be minted.
    #[serde(default = "default_true")]
    pub spectator: bool,
    /// Internal capability bit; the daemon derives it for operator session shares.
    #[serde(default)]
    pub controls: bool,
    /// Whether the target session should be killed when this share expires.
    #[serde(default)]
    pub kill_session_on_expire: bool,
}

/// Browser-visible scope exposed by a web share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebShareScope {
    /// Expose exactly one pane.
    Pane(PaneTargetRef),
    /// Expose an attached-client view of one session.
    Session(SessionName),
}

impl WebShareScope {
    /// Returns true when this share exposes one pane.
    #[must_use]
    pub const fn is_pane(&self) -> bool {
        matches!(self, Self::Pane(_))
    }

    /// Returns true when this share exposes one session.
    #[must_use]
    pub const fn is_session(&self) -> bool {
        matches!(self, Self::Session(_))
    }
}

impl fmt::Display for WebShareScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pane(target) => target.fmt(formatter),
            Self::Session(session_name) => session_name.fmt(formatter),
        }
    }
}

/// Browser presentation options for generated web-share URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebShareUrlOptions {
    /// Hide the share navigation bar for this generated URL.
    #[serde(default)]
    pub no_navbar: bool,
    /// Suppress the client-side privacy/disclaimer toast.
    #[serde(default)]
    pub no_disclaimer: bool,
    /// Show the live connected browser count in generated URLs.
    #[serde(default = "default_show_viewers")]
    pub show_viewers: bool,
    /// Optional initial terminal theme for generated spectator URLs.
    #[serde(default)]
    pub terminal_theme: Option<WebTerminalTheme>,
}

impl Default for WebShareUrlOptions {
    fn default() -> Self {
        Self {
            no_navbar: false,
            no_disclaimer: false,
            show_viewers: true,
            terminal_theme: None,
        }
    }
}

const fn default_show_viewers() -> bool {
    true
}

const fn default_true() -> bool {
    true
}

/// Initial terminal theme selected by the share URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WebTerminalTheme {
    /// Use the owner's captured terminal palette when available.
    User,
    /// Use the bundled light browser terminal palette.
    Light,
    /// Use the bundled dark browser terminal palette.
    Dark,
}

impl WebTerminalTheme {
    /// Returns the URL fragment value for this terminal theme.
    #[must_use]
    pub const fn as_url_value(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// Browser terminal palette captured from the local terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebTerminalPalette {
    /// Default foreground color as `#rrggbb`.
    pub foreground: String,
    /// Default background color as `#rrggbb`.
    pub background: String,
    /// Cursor color as `#rrggbb`.
    pub cursor: String,
    /// ANSI 0-15 palette colors as `#rrggbb`.
    pub ansi: [String; 16],
}

/// Request payload for `web-share -l`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListWebSharesRequest;

/// Request payload for `web-share -K <id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopWebShareRequest {
    /// Share identifier returned by creation.
    pub share_id: String,
}

/// Request payload for `web-share -X`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopAllWebSharesRequest;

/// Request payload for SDK/browser lookup of share metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookupWebShareRequest {
    /// Share identifier to inspect.
    pub share_id: String,
}

/// Request payload for daemon web-share listener configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebShareConfigRequest;
