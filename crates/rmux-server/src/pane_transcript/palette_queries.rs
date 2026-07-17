use std::time::{Duration, Instant};

use rmux_core::{TerminalPaletteIndex, TerminalPassthrough};

// Registration happens when pane output is parsed, before a potentially busy
// attach renderer writes the query to the outer terminal. Keep enough budget
// for a loaded ConPTY/SSH path while retaining a short, explicit correlation
// window.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_RESPONSES_PER_INDEX: u8 = 8;

#[derive(Debug, Clone, Copy)]
struct PendingPaletteQuery {
    index: TerminalPaletteIndex,
    responses: u8,
    expires_at: Instant,
}

#[derive(Default)]
pub(super) struct PendingPaletteQueries {
    // Sparse in practice and inherently bounded to 256 distinct typed indices.
    // Avoid charging every pane for a 256-entry table before it asks a query.
    entries: Vec<PendingPaletteQuery>,
}

impl PendingPaletteQueries {
    pub(super) fn register(&mut self, passthroughs: &[TerminalPassthrough], now: Instant) {
        let mut indices = passthroughs
            .iter()
            .filter_map(TerminalPassthrough::palette_query_index)
            .peekable();
        if indices.peek().is_none() {
            return;
        }
        self.entries.retain(|entry| entry.expires_at >= now);
        for index in indices {
            if let Some(pending) = self.entries.iter_mut().find(|entry| entry.index == index) {
                pending.responses = if pending.expires_at >= now {
                    pending.responses.saturating_add(1)
                } else {
                    1
                }
                .min(MAX_RESPONSES_PER_INDEX);
                pending.expires_at = now + RESPONSE_TIMEOUT;
            } else {
                self.entries.push(PendingPaletteQuery {
                    index,
                    responses: 1,
                    expires_at: now + RESPONSE_TIMEOUT,
                });
            }
        }
    }

    pub(super) fn consume(&mut self, index: TerminalPaletteIndex, now: Instant) -> bool {
        let Some(position) = self.entries.iter().position(|entry| entry.index == index) else {
            return false;
        };
        if self.entries[position].expires_at < now {
            self.entries.swap_remove(position);
            return false;
        }
        if self.entries[position].responses == 1 {
            self.entries.swap_remove(position);
        } else {
            self.entries[position].responses -= 1;
        }
        true
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}
