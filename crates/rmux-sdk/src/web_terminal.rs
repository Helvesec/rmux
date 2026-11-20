use serde::{Deserialize, Serialize};

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
