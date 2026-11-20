use super::*;

impl PaneOutputState {
    pub(super) fn new(event_capacity: usize, recent_byte_capacity: usize) -> Self {
        Self {
            ring: OutputRing::new(event_capacity, recent_byte_capacity),
            passthroughs: VecDeque::with_capacity(PANE_OUTPUT_PASSTHROUGH_CAPACITY),
            retained_passthrough_bytes: 0,
            invalidation_revision: 0,
            process_exit_revision: 0,
            process_exit_revision_at_invalidation: 0,
            last_invalidation_reason: PaneInvalidationReason::Initial,
        }
    }

    pub(super) fn boundary(&self, generation: u64) -> PaneBoundary {
        PaneBoundary {
            generation,
            next_output_sequence: self.next_sequence(),
            invalidation_revision: self.invalidation_revision,
            process_exit_revision: self.process_exit_revision,
        }
    }

    pub(super) fn invalidate(&mut self, reason: PaneInvalidationReason) {
        self.invalidation_revision = self.invalidation_revision.saturating_add(1);
        self.process_exit_revision_at_invalidation = self.process_exit_revision;
        self.last_invalidation_reason = reason;
    }

    pub(super) fn push(
        &mut self,
        bytes: Arc<[u8]>,
        passthroughs: Vec<TerminalPassthrough>,
        retain_recent: bool,
        retain_passthroughs: bool,
    ) -> u64 {
        if bytes.is_empty() {
            self.process_exit_revision = self.process_exit_revision.saturating_add(1);
        }
        let sequence = self
            .ring
            .push_shared_with_recent_retention(bytes, retain_recent);
        if retain_passthroughs && !passthroughs.is_empty() {
            let passthrough_bytes = passthrough_payload_bytes(&passthroughs);
            self.retained_passthrough_bytes = self
                .retained_passthrough_bytes
                .saturating_add(passthrough_bytes);
            self.passthroughs.push_back(PaneOutputPassthroughs {
                sequence,
                passthroughs,
            });
            while self.passthroughs.len() > PANE_OUTPUT_PASSTHROUGH_CAPACITY
                || self.retained_passthrough_bytes > PANE_OUTPUT_PASSTHROUGH_BYTE_CAPACITY
            {
                let Some(evicted) = self.passthroughs.pop_front() else {
                    break;
                };
                self.retained_passthrough_bytes = self
                    .retained_passthrough_bytes
                    .saturating_sub(passthrough_payload_bytes(&evicted.passthroughs));
            }
        }
        sequence
    }

    pub(super) fn cursor_from_now(&self) -> OutputCursor {
        self.ring.cursor_from_now()
    }

    pub(super) fn cursor_from_oldest(&self) -> OutputCursor {
        self.ring.cursor_from_oldest()
    }

    pub(super) fn next_sequence(&self) -> u64 {
        self.ring.next_sequence()
    }

    pub(super) fn clear_retained(&mut self) {
        self.ring.clear_retained();
        self.passthroughs.clear();
        self.retained_passthrough_bytes = 0;
    }

    pub(super) fn poll_cursor(
        &self,
        cursor: &mut OutputCursor,
        passthrough_floor_sequence: u64,
    ) -> Option<OutputCursorItem> {
        self.ring
            .poll_cursor(cursor)
            .map(|item| self.attach_passthroughs(item, passthrough_floor_sequence))
    }

    pub(super) fn poll_cursor_batch(
        &self,
        cursor: &mut OutputCursor,
        passthrough_floor_sequence: u64,
        limit: usize,
    ) -> Vec<OutputCursorItem> {
        self.ring
            .poll_cursor_batch(cursor, limit)
            .into_iter()
            .map(|item| self.attach_passthroughs(item, passthrough_floor_sequence))
            .collect()
    }

    pub(super) fn attach_passthroughs(
        &self,
        item: OutputCursorItem,
        passthrough_floor_sequence: u64,
    ) -> OutputCursorItem {
        let OutputCursorItem::Event(event) = item else {
            return item;
        };
        if event.sequence() < passthrough_floor_sequence {
            return OutputCursorItem::Event(event);
        }
        let passthroughs = self
            .passthroughs
            .iter()
            .find(|candidate| candidate.sequence == event.sequence())
            .map(|candidate| candidate.passthroughs.clone())
            .unwrap_or_default();
        OutputCursorItem::Event(event.with_passthroughs(passthroughs))
    }
}

fn passthrough_payload_bytes(passthroughs: &[TerminalPassthrough]) -> usize {
    passthroughs.iter().fold(0_usize, |total, passthrough| {
        total.saturating_add(passthrough.payload().len())
    })
}
