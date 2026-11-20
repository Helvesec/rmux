use crate::terminal_passthrough::MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES;
use crate::{input::InputEndType, TerminalPaletteIndex, TerminalPassthrough};

use super::{Screen, MAX_TERMINAL_PASSTHROUGH_EVENTS};

impl Screen {
    /// Drains terminal passthrough events observed since the last drain.
    pub fn take_terminal_passthrough(&mut self) -> Vec<TerminalPassthrough> {
        std::mem::take(&mut self.terminal_passthrough)
    }

    /// Drains the count of terminal passthrough events dropped by safety limits.
    pub fn take_terminal_passthrough_dropped_count(&mut self) -> u64 {
        std::mem::take(&mut self.dropped_terminal_passthrough_count)
    }

    pub(super) fn push_terminal_passthrough(&mut self, passthrough: TerminalPassthrough) {
        if passthrough.payload().len() > MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES {
            self.dropped_terminal_passthrough_count =
                self.dropped_terminal_passthrough_count.saturating_add(1);
            return;
        }

        let overflow = self
            .terminal_passthrough
            .len()
            .saturating_add(1)
            .saturating_sub(MAX_TERMINAL_PASSTHROUGH_EVENTS);
        if overflow > 0 {
            self.terminal_passthrough.drain(..overflow);
            self.dropped_terminal_passthrough_count = self
                .dropped_terminal_passthrough_count
                .saturating_add(overflow as u64);
        }

        self.terminal_passthrough.push(passthrough);
    }

    pub(super) fn relay_palette_queries(&mut self, data: &str) {
        // OSC 4 is a list of index/value pairs. tmux 3.7b forwards each valid
        // query as its own canonical ST-terminated OSC while handling palette
        // sets internally. RMUX does not maintain an outer-terminal palette,
        // so keep set behaviour unchanged and relay only strict, bounded
        // queries. The dedicated event kind bypasses generic raw-passthrough
        // policy without reflecting arbitrary OSC payloads.
        let mut fields = data.split(';');
        while let (Some(index), Some(value)) = (fields.next(), fields.next()) {
            if value == "?" {
                if let Some(index) = TerminalPaletteIndex::parse(index) {
                    self.push_terminal_passthrough(TerminalPassthrough::palette_query(index));
                }
            }
        }
    }
    pub(super) fn relay_clipboard(&mut self, data: &str, end: InputEndType) {
        let mut sequence = Vec::with_capacity(data.len() + 7);
        sequence.extend_from_slice(b"\x1b]52;");
        sequence.extend_from_slice(data.as_bytes());
        match end {
            InputEndType::Bel => sequence.push(b'\x07'),
            InputEndType::St => sequence.extend_from_slice(b"\x1b\\"),
        }
        if let Some((selection, "?")) = data.split_once(';') {
            self.push_terminal_passthrough(TerminalPassthrough::clipboard_query(
                crate::TerminalClipboardQuery::new(selection, end),
                sequence,
            ));
            return;
        }
        self.push_terminal_passthrough(TerminalPassthrough::clipboard(sequence));
    }
}
