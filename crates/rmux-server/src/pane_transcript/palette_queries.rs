use std::time::{Duration, Instant};

use rmux_core::{TerminalPaletteIndex, TerminalPassthrough};

// Registration happens when pane output is parsed, before a potentially busy
// attach renderer writes the query to the outer terminal. Keep enough budget
// for a loaded ConPTY/SSH path while retaining a short, explicit correlation
// window.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(8);
/// tmux 3.7b `INPUT_REQUEST_TIMEOUT` (input.c:75): a forwarded query is
/// active, and slows the client's partial keys, only this long after it was
/// sent. The correlation window above stays deliberately longer.
const ACTIVE_QUERY_WINDOW: Duration = Duration::from_millis(500);
const MAX_RESPONSES_PER_INDEX: u8 = 8;

#[derive(Debug, Clone, Copy)]
struct PendingPaletteQuery {
    index: TerminalPaletteIndex,
    responses: u8,
    registered_at: Instant,
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
                pending.registered_at = now;
                pending.expires_at = now + RESPONSE_TIMEOUT;
            } else {
                self.entries.push(PendingPaletteQuery {
                    index,
                    responses: 1,
                    registered_at: now,
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

    /// Whether an unanswered query is still active in tmux's sense, so a
    /// response split across reads must not expire at the keyboard escape-time.
    pub(super) fn has_active_query(&self, now: Instant) -> bool {
        self.entries
            .iter()
            .any(|entry| now.saturating_duration_since(entry.registered_at) <= ACTIVE_QUERY_WINDOW)
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}
