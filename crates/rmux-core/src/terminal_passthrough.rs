use std::sync::Arc;

use crate::input::InputEndType;

/// Maximum payload size retained for one terminal graphics passthrough event.
pub(crate) const MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

/// Opaque terminal command that must be forwarded to a capable outer terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPassthrough {
    kind: TerminalPassthroughKind,
    cursor_x: u32,
    cursor_y: u32,
    palette_index: Option<TerminalPaletteIndex>,
    clipboard_query: Option<TerminalClipboardQuery>,
    payload: Arc<[u8]>,
}

/// Supported terminal passthrough protocol families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalPassthroughKind {
    /// Opaque tmux DCS passthrough payload, already framed for the outer terminal.
    Raw,
    /// OSC 52 clipboard payload emitted by a pane program.
    Clipboard,
    /// OSC 4 palette query relayed to the attached outer terminal.
    PaletteQuery,
    /// Kitty terminal graphics protocol, encoded as an APC payload.
    KittyGraphics,
    /// SIXEL graphics protocol, encoded as a DCS payload.
    Sixel,
}

/// A terminal palette index accepted by OSC 4.
///
/// OSC 4 addresses the 256-entry terminal palette. Keeping the bound in a
/// type prevents arbitrary OSC bodies from being reflected through the outer
/// terminal query path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TerminalPaletteIndex(u8);

/// A bounded pane-originated OSC 52 clipboard query.
///
/// The selector is reduced to the first tmux-supported selector byte. Invalid
/// selector bytes are discarded instead of being reflected to a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalClipboardQuery {
    selection: Option<u8>,
    terminator: InputEndType,
}

impl TerminalClipboardQuery {
    const VALID_SELECTIONS: &'static [u8] = b"cpqs01234567";

    /// Creates a typed query from an OSC 52 selection field and terminator.
    #[must_use]
    pub fn new(selection: &str, terminator: InputEndType) -> Self {
        Self {
            selection: selection
                .bytes()
                .find(|byte| Self::VALID_SELECTIONS.contains(byte)),
            terminator,
        }
    }

    /// Returns the first valid tmux clipboard selector, if any.
    #[must_use]
    pub const fn selection(self) -> Option<u8> {
        self.selection
    }

    /// Returns the pane query's original OSC terminator.
    #[must_use]
    pub const fn terminator(self) -> InputEndType {
        self.terminator
    }
}

impl TerminalPaletteIndex {
    /// Parses one strict ASCII-decimal palette index in the inclusive 0..=255
    /// range.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        value.parse::<u8>().ok().map(Self)
    }

    /// Returns the numeric palette index.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<u8> for TerminalPaletteIndex {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

impl TerminalPassthrough {
    /// Creates an opaque passthrough event at a pane-local cursor position.
    #[must_use]
    pub fn raw(cursor_x: u32, cursor_y: u32, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            kind: TerminalPassthroughKind::Raw,
            cursor_x,
            cursor_y,
            palette_index: None,
            clipboard_query: None,
            payload: Arc::from(payload.into()),
        }
    }

    /// Creates an OSC 52 clipboard passthrough event.
    #[must_use]
    pub fn clipboard(payload: impl Into<Vec<u8>>) -> Self {
        Self {
            kind: TerminalPassthroughKind::Clipboard,
            cursor_x: 0,
            cursor_y: 0,
            palette_index: None,
            clipboard_query: None,
            payload: Arc::from(payload.into()),
        }
    }

    /// Creates a typed OSC 52 clipboard query event while retaining its
    /// original framed payload for inspection.
    #[must_use]
    pub fn clipboard_query(query: TerminalClipboardQuery, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            // Preserve the public protocol-family classification: adding a
            // new enum variant would break exhaustive downstream matches in
            // a patch release. The additive typed metadata distinguishes a
            // query from an OSC 52 write internally.
            kind: TerminalPassthroughKind::Clipboard,
            cursor_x: 0,
            cursor_y: 0,
            palette_index: None,
            clipboard_query: Some(query),
            payload: Arc::from(payload.into()),
        }
    }

    /// Creates a bounded OSC 4 query for one palette index.
    ///
    /// tmux 3.7b canonicalizes both BEL- and ST-terminated pane queries to an
    /// ST-terminated sequence before sending them to the outer terminal.
    #[must_use]
    pub fn palette_query(index: TerminalPaletteIndex) -> Self {
        let payload = format!("\x1b]4;{};?\x1b\\", index.get()).into_bytes();
        Self {
            kind: TerminalPassthroughKind::PaletteQuery,
            cursor_x: 0,
            cursor_y: 0,
            palette_index: Some(index),
            clipboard_query: None,
            payload: Arc::from(payload),
        }
    }

    /// Creates a Kitty graphics passthrough event at a pane-local cursor position.
    #[must_use]
    pub fn kitty_graphics(cursor_x: u32, cursor_y: u32, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            kind: TerminalPassthroughKind::KittyGraphics,
            cursor_x,
            cursor_y,
            palette_index: None,
            clipboard_query: None,
            payload: Arc::from(payload.into()),
        }
    }

    /// Creates a SIXEL passthrough event at a pane-local cursor position.
    #[must_use]
    pub fn sixel(cursor_x: u32, cursor_y: u32, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            kind: TerminalPassthroughKind::Sixel,
            cursor_x,
            cursor_y,
            palette_index: None,
            clipboard_query: None,
            payload: Arc::from(payload.into()),
        }
    }

    /// Returns the passthrough protocol family.
    #[must_use]
    pub const fn kind(&self) -> TerminalPassthroughKind {
        self.kind
    }

    /// Returns the pane-local cursor column captured when the sequence arrived.
    #[must_use]
    pub const fn cursor_x(&self) -> u32 {
        self.cursor_x
    }

    /// Returns the pane-local cursor row captured when the sequence arrived.
    #[must_use]
    pub const fn cursor_y(&self) -> u32 {
        self.cursor_y
    }

    /// Returns the opaque protocol payload without escape framing.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Returns the queried palette index for typed OSC 4 query events.
    #[must_use]
    pub const fn palette_query_index(&self) -> Option<TerminalPaletteIndex> {
        self.palette_index
    }

    /// Returns typed OSC 52 query metadata for clipboard-query events.
    #[must_use]
    pub const fn clipboard_query_metadata(&self) -> Option<TerminalClipboardQuery> {
        self.clipboard_query
    }

    /// Renders the passthrough as an outer-terminal escape sequence.
    #[must_use]
    pub fn render_sequence(&self) -> Vec<u8> {
        // Clipboard queries are server-correlated and must never fall
        // through the generic outer-terminal passthrough path.
        if self.clipboard_query.is_some() {
            return Vec::new();
        }
        match self.kind {
            TerminalPassthroughKind::Raw => self.payload.to_vec(),
            TerminalPassthroughKind::Clipboard => self.payload.to_vec(),
            TerminalPassthroughKind::PaletteQuery => self.payload.to_vec(),
            TerminalPassthroughKind::KittyGraphics => {
                let mut sequence = Vec::with_capacity(self.payload.len() + 4);
                sequence.extend_from_slice(b"\x1b_");
                sequence.extend_from_slice(&self.payload);
                sequence.extend_from_slice(b"\x1b\\");
                sequence
            }
            TerminalPassthroughKind::Sixel => {
                let mut sequence = Vec::with_capacity(self.payload.len() + 4);
                sequence.extend_from_slice(b"\x1bP");
                sequence.extend_from_slice(&self.payload);
                sequence.extend_from_slice(b"\x1b\\");
                sequence
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TerminalClipboardQuery, TerminalPaletteIndex, TerminalPassthrough};
    use crate::input::InputEndType;

    #[test]
    fn renders_kitty_apc_sequence() {
        let passthrough = TerminalPassthrough::kitty_graphics(0, 0, b"Gf=100;AAAA".to_vec());

        assert_eq!(passthrough.render_sequence(), b"\x1b_Gf=100;AAAA\x1b\\");
    }

    #[test]
    fn renders_raw_sequence_verbatim() {
        let passthrough = TerminalPassthrough::raw(0, 0, b"\x1b]52;c;QQ==\x1b\\".to_vec());

        assert_eq!(passthrough.render_sequence(), b"\x1b]52;c;QQ==\x1b\\");
    }

    #[test]
    fn renders_clipboard_sequence_verbatim() {
        let passthrough = TerminalPassthrough::clipboard(b"\x1b]52;c;QQ==\x07".to_vec());

        assert_eq!(passthrough.render_sequence(), b"\x1b]52;c;QQ==\x07");
    }

    #[test]
    fn clipboard_query_is_typed_bounded_and_never_generically_rendered() {
        let query = TerminalClipboardQuery::new("zzpc", InputEndType::St);
        assert_eq!(query.selection(), Some(b'p'));
        assert_eq!(query.terminator(), InputEndType::St);

        let passthrough =
            TerminalPassthrough::clipboard_query(query, b"\x1b]52;zzpc;?\x1b\\".to_vec());
        assert_eq!(
            passthrough.kind(),
            super::TerminalPassthroughKind::Clipboard
        );
        assert_eq!(passthrough.clipboard_query_metadata(), Some(query));
        assert_eq!(passthrough.payload(), b"\x1b]52;zzpc;?\x1b\\");
        assert!(passthrough.render_sequence().is_empty());

        let invalid = TerminalClipboardQuery::new("xyz", InputEndType::Bel);
        assert_eq!(invalid.selection(), None);
    }

    #[test]
    fn palette_query_is_bounded_typed_and_canonical() {
        assert_eq!(
            TerminalPaletteIndex::parse("0").map(TerminalPaletteIndex::get),
            Some(0)
        );
        assert_eq!(
            TerminalPaletteIndex::parse("255").map(TerminalPaletteIndex::get),
            Some(255)
        );
        assert_eq!(TerminalPaletteIndex::parse("256"), None);
        assert_eq!(TerminalPaletteIndex::parse("-1"), None);

        let query = TerminalPassthrough::palette_query(TerminalPaletteIndex::from(255));
        assert_eq!(query.render_sequence(), b"\x1b]4;255;?\x1b\\");
        assert_eq!(
            query.palette_query_index(),
            Some(TerminalPaletteIndex::from(255))
        );
    }

    #[test]
    fn renders_sixel_dcs_sequence() {
        let passthrough = TerminalPassthrough::sixel(0, 0, b"q#0!10~".to_vec());

        assert_eq!(passthrough.render_sequence(), b"\x1bPq#0!10~\x1b\\");
    }
}
