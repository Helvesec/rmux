//! Canonical identity newtypes shared across the RMUX workspace.
//!
//! `rmux-types` is the single public home for the identity vocabulary
//! (`SessionName`, `SessionId`, `WindowId`, `PaneId`). Other crates,
//! including `rmux-core`, `rmux-server`, and `rmux-sdk`, must re-export
//! these types rather than declaring their own. Allocation, lookup, and
//! resolution remain in `rmux-core::session`; the types defined here
//! describe identity values, not the policy that issues them.

use std::fmt;
use std::str::FromStr;

#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer};

/// A session name was empty.
///
/// This transport-independent validation error is returned by every session-name
/// constructor. Protocol and SDK boundaries map it to their existing diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionNameError;

impl fmt::Display for SessionNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid session name: session names must be non-empty")
    }
}

impl std::error::Error for SessionNameError {}

/// A validated RMUX session name.
///
/// Empty strings are rejected. `:` and `.` characters are rewritten to `_`
/// to keep names safe for use inside exact target syntax (`session`,
/// `session:window`, `session:window.pane`). Non-printable characters are
/// rendered using tmux's `vis`-style escape sequences so display output is
/// always single-line and non-controlling. Constructors escape literal
/// backslashes; serde deserialization preserves canonical backslashes while
/// applying the same colon, dot, control and Unicode format-character rules.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct SessionName(String);

impl SessionName {
    /// Validates and stores a session name using tmux-compatible rewriting.
    ///
    /// Returns [`SessionNameError`] for an empty value. This constructor escapes
    /// literal backslashes. Pass canonical names as values, or ingest their
    /// display text with [`Self::from_canonical`].
    pub fn new(value: impl Into<String>) -> Result<Self, SessionNameError> {
        let value = value.into();

        if value.is_empty() {
            return Err(SessionNameError);
        }

        Ok(Self(sanitize_session_name(&value)))
    }

    /// Validates a canonical name read from a daemon inventory, a serialized
    /// value or target text that looks up an existing session.
    ///
    /// Preserves existing backslash escapes while applying the same colon, dot,
    /// control and unsafe Unicode rewriting as deserialization. Returns
    /// [`SessionNameError`] for an empty value. Use [`Self::new`] for raw input.
    pub fn from_canonical(value: impl Into<String>) -> Result<Self, SessionNameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SessionNameError);
        }
        Ok(Self(sanitize_session_name_with_backslash_policy(
            &value, false,
        )))
    }

    /// Returns the sanitized validated session name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the wrapper and returns the sanitized string.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

fn sanitize_session_name(input: &str) -> String {
    sanitize_session_name_with_backslash_policy(input, true)
}

fn sanitize_session_name_with_backslash_policy(input: &str, escape_backslash: bool) -> String {
    let mut sanitized = String::with_capacity(input.len());
    for character in input.chars() {
        let rewritten = match character {
            ':' | '.' => '_',
            other => other,
        };
        push_session_name_character(rewritten, escape_backslash, &mut sanitized);
    }
    sanitized
}

fn push_session_name_character(character: char, escape_backslash: bool, output: &mut String) {
    match character {
        '\0' => output.push_str("\\000"),
        '\x07' => output.push_str("\\a"),
        '\x08' => output.push_str("\\b"),
        '\t' => output.push_str("\\t"),
        '\n' => output.push_str("\\n"),
        '\x0b' => output.push_str("\\v"),
        '\x0c' => output.push_str("\\f"),
        '\r' => output.push_str("\\r"),
        '\\' if escape_backslash => output.push_str("\\\\"),
        control if control.is_control() => {
            let value = control as u32;
            output.push('\\');
            output.push(char::from(b'0' + ((value >> 6) & 0x7) as u8));
            output.push(char::from(b'0' + ((value >> 3) & 0x7) as u8));
            output.push(char::from(b'0' + (value & 0x7) as u8));
        }
        // Line/paragraph separators, bidi overrides and zero-width marks are NOT
        // Unicode Cc controls, so `is_control()` misses them, yet they still break
        // the single-line, non-controlling display invariant (and bidi overrides
        // can spoof the rendered name). Escape each UTF-8 byte octally — the same
        // `\NNN` form as the control arm and tmux's byte-oriented vis.
        format_char if is_display_unsafe_format_char(format_char) => {
            let mut buffer = [0_u8; 4];
            for byte in format_char.encode_utf8(&mut buffer).bytes() {
                output.push('\\');
                output.push(char::from(b'0' + ((byte >> 6) & 0x7)));
                output.push(char::from(b'0' + ((byte >> 3) & 0x7)));
                output.push(char::from(b'0' + (byte & 0x7)));
            }
        }
        _ => {
            output.push(character);
        }
    }
}

/// Non-`Cc` code points that still violate the single-line, non-controlling
/// session-name invariant: line/paragraph separators, bidi embeddings, overrides
/// and isolates, and zero-width / invisible formatting marks.
fn is_display_unsafe_format_char(character: char) -> bool {
    matches!(
        character as u32,
        0x00AD               // SOFT HYPHEN
            | 0x061C         // ARABIC LETTER MARK
            | 0x200B..=0x200F // ZWSP, ZWNJ, ZWJ, LRM, RLM
            | 0x2028         // LINE SEPARATOR
            | 0x2029         // PARAGRAPH SEPARATOR
            | 0x202A..=0x202E // LRE, RLE, PDF, LRO, RLO (bidi embeddings/overrides)
            | 0x2066..=0x2069 // LRI, RLI, FSI, PDI (bidi isolates)
            | 0xFEFF         // ZERO WIDTH NO-BREAK SPACE / BOM
    )
}

impl AsRef<str> for SessionName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for SessionName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SessionName {
    type Err = SessionNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for SessionName {
    type Error = SessionNameError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for SessionName {
    type Error = SessionNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

#[cfg(feature = "serde")]
impl<'de> Deserialize<'de> for SessionName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_canonical(value).map_err(serde::de::Error::custom)
    }
}

/// Stable per-server session identity (`$N`).
///
/// `SessionId` is the numeric identity rendered as `$N` by tmux-compatible
/// formats. Allocation lives in `rmux-core::session::SessionStore`; the
/// type defined here is the storable, transferable identity value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct SessionId(u32);

impl SessionId {
    /// Wraps a raw stable session identity.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw stable session identity.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "${}", self.0)
    }
}

impl From<SessionId> for u32 {
    fn from(value: SessionId) -> Self {
        value.0
    }
}

impl From<u32> for SessionId {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

/// Stable per-server window identity (`@N`).
///
/// `WindowId` is the numeric identity rendered as `@N` by tmux-compatible
/// formats. Allocation lives in `rmux-core::session`; the type defined
/// here is the storable, transferable identity value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct WindowId(u32);

impl WindowId {
    /// Wraps a raw stable window identity.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw stable window identity.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for WindowId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "@{}", self.0)
    }
}

impl From<WindowId> for u32 {
    fn from(value: WindowId) -> Self {
        value.0
    }
}

impl From<u32> for WindowId {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

/// Stable per-server pane identity (`%N`).
///
/// `PaneId` is the numeric identity rendered as `%N` by tmux-compatible
/// formats. Allocation lives in `rmux-core::session::SessionStore`; the
/// type defined here is the storable, transferable identity value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct PaneId(u32);

impl PaneId {
    /// Wraps a raw stable pane identity.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw stable pane identity.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "%{}", self.0)
    }
}

impl From<PaneId> for u32 {
    fn from(value: PaneId) -> Self {
        value.0
    }
}

impl From<u32> for PaneId {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests;
