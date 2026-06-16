use std::sync::Arc;

/// Maximum payload size retained for one terminal graphics passthrough event.
pub(crate) const MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

/// Opaque terminal command that must be forwarded to a capable outer terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPassthrough {
    kind: TerminalPassthroughKind,
    cursor_x: u32,
    cursor_y: u32,
    payload: Arc<[u8]>,
}

/// Supported terminal passthrough protocol families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalPassthroughKind {
    /// Opaque tmux DCS passthrough payload, already framed for the outer terminal.
    Raw,
    /// OSC 52 clipboard payload emitted by a pane program.
    Clipboard,
    /// Kitty terminal graphics protocol, encoded as an APC payload.
    KittyGraphics,
    /// SIXEL graphics protocol, encoded as a DCS payload.
    Sixel,
}

impl TerminalPassthrough {
    /// Creates an opaque passthrough event at a pane-local cursor position.
    #[must_use]
    pub fn raw(cursor_x: u32, cursor_y: u32, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            kind: TerminalPassthroughKind::Raw,
            cursor_x,
            cursor_y,
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
            payload: Arc::from(payload.into()),
        }
    }

    /// Creates a Kitty graphics passthrough event at a pane-local cursor position.
    #[must_use]
    pub fn kitty_graphics(cursor_x: u32, cursor_y: u32, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            kind: TerminalPassthroughKind::KittyGraphics,
            cursor_x,
            cursor_y,
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

    /// Renders the passthrough as an outer-terminal escape sequence.
    #[must_use]
    pub fn render_sequence(&self) -> Vec<u8> {
        match self.kind {
            TerminalPassthroughKind::Raw => self.payload.to_vec(),
            TerminalPassthroughKind::Clipboard => self.payload.to_vec(),
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
    use super::TerminalPassthrough;

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
    fn renders_sixel_dcs_sequence() {
        let passthrough = TerminalPassthrough::sixel(0, 0, b"q#0!10~".to_vec());

        assert_eq!(passthrough.render_sequence(), b"\x1bPq#0!10~\x1b\\");
    }
}
