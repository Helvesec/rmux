use super::*;

impl OutputRing {
    /// Raw bytes a cursor poll may copy, without cloning output or a gap snapshot.
    #[must_use]
    pub fn next_cursor_byte_len(&self, cursor: &OutputCursor) -> Option<usize> {
        let next = cursor.next_sequence();
        let oldest = self.oldest_sequence();
        if next < oldest {
            return Some(self.recent.len);
        }
        if next >= self.next_sequence {
            return None;
        }
        let offset = usize::try_from(next.saturating_sub(oldest)).ok()?;
        self.events.get(offset).map(OutputEvent::byte_len)
    }
}

impl OutputRing {
    /// Polls a cursor without copying lag-recovery bytes the caller discards.
    pub fn poll_cursor_without_recovery(
        &self,
        cursor: &mut OutputCursor,
    ) -> Option<OutputCursorItem> {
        let expected = cursor.next_sequence();
        let oldest = self.oldest_sequence();
        if expected >= oldest {
            return self.poll_cursor(cursor);
        }
        let missed = oldest.saturating_sub(expected);
        cursor.record_gap(missed, oldest);
        Some(OutputCursorItem::Gap(Box::new(OutputGap::new(
            expected,
            oldest,
            missed,
            self.newest_sequence(),
            RecentOutputSnapshot {
                bytes: Vec::new(),
                oldest_sequence: None,
                newest_sequence: None,
                chunks: Vec::new(),
            },
        ))))
    }
}
