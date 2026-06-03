//! Canonical identity newtypes shared across the RMUX workspace.
//!
//! `rmux-proto` is the single public home for the identity vocabulary
//! (`SessionName`, `SessionId`, `WindowId`, `PaneId`). Other crates,
//! including `rmux-core`, `rmux-server`, and `rmux-sdk`, must re-export
//! these types rather than declaring their own. Allocation, lookup, and
//! resolution remain in `rmux-core::session`; the types defined here
//! describe identity values, not the policy that issues them.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

use crate::RmuxError;

/// A validated RMUX session name.
///
/// Empty strings are rejected. `:` and `.` characters are rewritten to `_`
/// to keep names safe for use inside exact target syntax (`session`,
/// `session:window`, `session:window.pane`). Non-printable characters are
/// rendered using tmux's `vis`-style escape sequences so display output is
/// always single-line and non-controlling.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SessionName(String);

impl SessionName {
    /// Validates and stores a session name using tmux-compatible rewriting.
    pub fn new(value: impl Into<String>) -> Result<Self, RmuxError> {
        let value = value.into();

        if value.is_empty() {
            return Err(RmuxError::EmptySessionName);
        }

        Ok(Self(sanitize_session_name(&value)))
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
    let mut sanitized = String::with_capacity(input.len());
    for character in input.chars() {
        let rewritten = match character {
            ':' | '.' => '_',
            other => other,
        };
        push_session_name_character(rewritten, &mut sanitized);
    }
    sanitized
}

fn push_session_name_character(character: char, output: &mut String) {
    match character {
        '\0' => output.push_str("\\000"),
        '\x07' => output.push_str("\\a"),
        '\x08' => output.push_str("\\b"),
        '\t' => output.push_str("\\t"),
        '\n' => output.push_str("\\n"),
        '\x0b' => output.push_str("\\v"),
        '\x0c' => output.push_str("\\f"),
        '\r' => output.push_str("\\r"),
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
        // `\NNN` form as the control arm and tmux's byte-oriented vis. The leading
        // backslash is never itself escaped, so the output re-sanitizes to itself
        // and stays idempotent through serde's re-sanitizing Deserialize.
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
    type Err = RmuxError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for SessionName {
    type Error = RmuxError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for SessionName {
    type Error = RmuxError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for SessionName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Stable per-server session identity (`$N`).
///
/// `SessionId` is the numeric identity rendered as `$N` by tmux-compatible
/// formats. Allocation lives in `rmux-core::session::SessionStore`; the
/// type defined here is the storable, transferable identity value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
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
mod tests {
    use super::{is_display_unsafe_format_char, PaneId, SessionId, SessionName, WindowId};
    use crate::RmuxError;

    #[test]
    fn session_name_rejects_empty_values() {
        assert_eq!(SessionName::new(""), Err(RmuxError::EmptySessionName));
    }

    #[test]
    fn session_name_rewrites_colon_and_dot() {
        assert_eq!(
            SessionName::new("alpha:beta.gamma")
                .expect("rewritten")
                .as_str(),
            "alpha_beta_gamma"
        );
    }

    #[test]
    fn session_name_round_trips_through_serde() {
        let payload = bincode::serialize("alpha.beta").expect("string encodes");
        assert_eq!(
            bincode::deserialize::<SessionName>(&payload).expect("rewritten on the wire"),
            SessionName::new("alpha_beta").expect("valid")
        );
    }

    #[test]
    fn session_name_serde_rejects_empty_payloads_truthfully() {
        let payload = bincode::serialize("").expect("empty string encodes");
        assert!(
            bincode::deserialize::<SessionName>(&payload).is_err(),
            "empty session names must fail deserialization rather than silently \
             producing an empty inner value"
        );
    }

    #[test]
    fn session_name_serialize_round_trips_after_rewriting() {
        let original = SessionName::new("alpha.beta").expect("rewrites dots");
        let bytes = bincode::serialize(&original).expect("session name encodes");
        let restored: SessionName =
            bincode::deserialize(&bytes).expect("session name decodes idempotently");
        assert_eq!(restored, original);
        assert_eq!(restored.as_str(), "alpha_beta");
    }

    #[test]
    fn session_name_from_str_and_try_from_match_constructor() {
        let from_str: SessionName = "alpha:beta".parse().expect("FromStr rewrites");
        let try_from_ref: SessionName =
            SessionName::try_from("alpha:beta").expect("TryFrom<&str> rewrites");
        let try_from_owned: SessionName =
            SessionName::try_from(String::from("alpha:beta")).expect("TryFrom<String> rewrites");
        assert_eq!(from_str, try_from_ref);
        assert_eq!(from_str, try_from_owned);
        assert_eq!(from_str.as_str(), "alpha_beta");
    }

    #[test]
    fn session_name_into_inner_returns_sanitized_string() {
        let owned = SessionName::new("alpha:beta")
            .expect("rewrites colons")
            .into_inner();
        assert_eq!(owned, "alpha_beta");
    }

    #[test]
    fn session_name_escapes_line_and_paragraph_separators() {
        // U+2028/U+2029 are not Cc controls but break the single-line invariant.
        assert_eq!(
            SessionName::new("a\u{2028}b\u{2029}c")
                .expect("escaped")
                .as_str(),
            "a\\342\\200\\250b\\342\\200\\251c"
        );
    }

    #[test]
    fn session_name_escapes_bidi_overrides_and_zero_width_marks() {
        // A right-to-left override could otherwise spoof the rendered name; a
        // zero-width space could hide content. Both must be escaped.
        let rendered = SessionName::new("a\u{202e}b\u{200b}c").expect("escaped");
        assert_eq!(rendered.as_str(), "a\\342\\200\\256b\\342\\200\\213c");
        assert!(!rendered.as_str().chars().any(is_display_unsafe_format_char));
    }

    #[test]
    fn session_name_sanitization_is_idempotent_through_serde() {
        // Deserialize re-runs sanitize_session_name, so an escaped name must
        // survive a second pass unchanged (no doubled backslashes).
        let original =
            SessionName::new("tab\there\u{2028}line\u{202e}rtl\x01ctl").expect("escaped");
        let bytes = bincode::serialize(&original).expect("encodes");
        let restored: SessionName = bincode::deserialize(&bytes).expect("decodes");
        assert_eq!(restored, original, "re-sanitizing must be a no-op");
        // And a direct second sanitize pass is also a no-op.
        assert_eq!(
            SessionName::new(original.as_str())
                .expect("re-sanitizes")
                .as_str(),
            original.as_str()
        );
    }

    #[test]
    fn session_id_displays_with_dollar_prefix() {
        assert_eq!(SessionId::new(7).to_string(), "$7");
        assert_eq!(SessionId::new(7).as_u32(), 7);
    }

    #[test]
    fn window_id_displays_with_at_prefix() {
        assert_eq!(WindowId::new(9).to_string(), "@9");
        assert_eq!(WindowId::new(9).as_u32(), 9);
    }

    #[test]
    fn window_id_zero_and_max_render_as_at_prefixed_decimal() {
        assert_eq!(WindowId::new(0).to_string(), "@0");
        assert_eq!(
            WindowId::new(u32::MAX).to_string(),
            format!("@{}", u32::MAX)
        );
    }

    #[test]
    fn pane_id_displays_with_percent_prefix() {
        assert_eq!(PaneId::new(3).to_string(), "%3");
        assert_eq!(PaneId::new(3).as_u32(), 3);
    }

    #[test]
    fn pane_id_zero_and_max_render_as_percent_prefixed_decimal() {
        assert_eq!(PaneId::new(0).to_string(), "%0");
        assert_eq!(PaneId::new(u32::MAX).to_string(), format!("%{}", u32::MAX));
    }

    #[test]
    fn session_id_zero_and_max_render_as_dollar_prefixed_decimal() {
        assert_eq!(SessionId::new(0).to_string(), "$0");
        assert_eq!(
            SessionId::new(u32::MAX).to_string(),
            format!("${}", u32::MAX)
        );
    }

    #[test]
    fn identity_newtypes_round_trip_through_u32_conversions() {
        for value in [0_u32, 1, 17, u32::MAX] {
            assert_eq!(u32::from(SessionId::from(value)), value);
            assert_eq!(u32::from(WindowId::from(value)), value);
            assert_eq!(u32::from(PaneId::from(value)), value);
            assert_eq!(SessionId::from(value).as_u32(), value);
            assert_eq!(WindowId::from(value).as_u32(), value);
            assert_eq!(PaneId::from(value).as_u32(), value);
        }
    }

    #[test]
    fn identity_newtypes_are_serde_transparent() {
        assert_eq!(
            bincode::serialize(&PaneId::new(11)).expect("encodes"),
            bincode::serialize(&11_u32).expect("encodes")
        );
        assert_eq!(
            bincode::serialize(&WindowId::new(11)).expect("encodes"),
            bincode::serialize(&11_u32).expect("encodes")
        );
        assert_eq!(
            bincode::serialize(&SessionId::new(11)).expect("encodes"),
            bincode::serialize(&11_u32).expect("encodes")
        );
    }

    #[test]
    fn identity_id_newtypes_decode_back_through_serde() {
        for value in [0_u32, 7, 257, u32::MAX] {
            let session_bytes =
                bincode::serialize(&SessionId::new(value)).expect("session id encodes");
            let window_bytes =
                bincode::serialize(&WindowId::new(value)).expect("window id encodes");
            let pane_bytes = bincode::serialize(&PaneId::new(value)).expect("pane id encodes");

            assert_eq!(
                bincode::deserialize::<SessionId>(&session_bytes).expect("session id decodes"),
                SessionId::new(value),
            );
            assert_eq!(
                bincode::deserialize::<WindowId>(&window_bytes).expect("window id decodes"),
                WindowId::new(value),
            );
            assert_eq!(
                bincode::deserialize::<PaneId>(&pane_bytes).expect("pane id decodes"),
                PaneId::new(value),
            );
        }
    }

    #[test]
    fn identity_id_newtypes_total_order_matches_inner_u32() {
        let mut ids = [PaneId::new(3), PaneId::new(0), PaneId::new(1)];
        ids.sort();
        assert_eq!(ids, [PaneId::new(0), PaneId::new(1), PaneId::new(3)]);
    }

    #[test]
    fn session_name_already_sanitized_round_trips_through_serde() {
        let original = SessionName::new("alpha-beta_gamma").expect("printable name");
        let bytes = bincode::serialize(&original).expect("session name encodes");
        let restored: SessionName =
            bincode::deserialize(&bytes).expect("session name decodes idempotently");
        assert_eq!(restored, original);
    }
}
