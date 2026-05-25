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
    /// Optional browser frontend URL used for this share.
    #[serde(default)]
    pub frontend_url: Option<String>,
    /// Optional maximum share lifetime in seconds.
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
    /// Optional cap for concurrent read-only clients.
    #[serde(default)]
    pub max_readers: Option<u16>,
    /// Presentation options encoded into generated read-only URLs.
    #[serde(default)]
    pub url_options: WebShareUrlOptions,
    /// Whether clients must provide the out-of-band pairing code during auth.
    #[serde(default)]
    pub require_pin: bool,
    /// Terminal palette captured by the CLI for browser-side "User" theme.
    #[serde(default)]
    pub terminal_palette: Option<Box<WebTerminalPalette>>,
    /// Whether an operator URL should be minted.
    #[serde(default)]
    pub writable: bool,
    /// Whether writable session shares may execute whitelisted rmux controls.
    #[serde(default)]
    pub controls: bool,
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebShareUrlOptions {
    /// Hide the share navigation bar for this generated URL.
    #[serde(default)]
    pub no_navbar: bool,
    /// Suppress the client-side privacy/disclaimer toast.
    #[serde(default)]
    pub no_disclaimer: bool,
    /// Optional initial terminal theme for generated read-only URLs.
    #[serde(default)]
    pub terminal_theme: Option<WebTerminalTheme>,
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
