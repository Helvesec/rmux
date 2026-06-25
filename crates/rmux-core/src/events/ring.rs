use std::collections::VecDeque;
use std::sync::Arc;

use super::cursor::{OutputCursor, OutputCursorItem, OutputGap};
use crate::TerminalPassthrough;

/// Default retained pane-output events per pane.
pub const DEFAULT_OUTPUT_RING_CAPACITY: usize = 2048;
/// Default retained recent live output bytes per pane.
pub const DEFAULT_RECENT_LIVE_BUFFER_CAPACITY: usize = 256 * 1024;

/// A single pane-output event retained by an [`OutputRing`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputEvent {
    sequence: u64,
    bytes: Arc<[u8]>,
    passthroughs: Vec<TerminalPassthrough>,
}

impl OutputEvent {
    /// Creates an output event from already shared bytes.
    #[must_use]
    pub fn from_shared(
        sequence: u64,
        bytes: Arc<[u8]>,
        passthroughs: Vec<TerminalPassthrough>,
    ) -> Self {
        Self {
            sequence,
            bytes,
            passthroughs,
        }
    }

    /// Returns this event's monotonic per-ring sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the raw bytes carried by this output event.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the raw byte count without cloning the event payload.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    /// Returns whether the event carries no raw bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Returns terminal passthrough events produced by this output event.
    #[must_use]
    pub fn passthroughs(&self) -> &[TerminalPassthrough] {
        &self.passthroughs
    }

    /// Consumes this event and returns its raw bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes.to_vec()
    }

    /// Consumes this event and returns both raw bytes and terminal side effects.
    #[must_use]
    pub fn into_parts(self) -> (Vec<u8>, Vec<TerminalPassthrough>) {
        (self.bytes.to_vec(), self.passthroughs)
    }

    /// Consumes this event and returns only terminal side effects.
    ///
    /// Live render subscribers already derive visible state from the pane
    /// transcript, so cloning raw output bytes just to inspect passthroughs is
    /// unnecessary on that hot path.
    #[must_use]
    pub fn into_passthroughs(self) -> Vec<TerminalPassthrough> {
        self.passthroughs
    }

    /// Returns a copy of this event carrying terminal passthrough side effects.
    #[must_use]
    pub fn with_passthroughs(mut self, passthroughs: Vec<TerminalPassthrough>) -> Self {
        self.passthroughs = passthroughs;
        self
    }
}

/// Bounded recent live bytes retained alongside an output ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentOutputSnapshot {
    bytes: Vec<u8>,
    oldest_sequence: Option<u64>,
    newest_sequence: Option<u64>,
    chunks: Vec<RecentOutputSnapshotChunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecentOutputSnapshotChunk {
    sequence: u64,
    start: usize,
    starts_at_event_start: bool,
}

impl RecentOutputSnapshot {
    /// Returns the retained recent live bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns retained bytes whose contributing output event sequence is at
    /// least `min_sequence`.
    #[must_use]
    pub fn bytes_from_sequence(&self, min_sequence: u64) -> &[u8] {
        let start = self
            .chunks
            .iter()
            .find(|chunk| chunk.sequence >= min_sequence)
            .map_or(self.bytes.len(), |chunk| chunk.start);
        &self.bytes[start..]
    }

    /// Returns the oldest output sequence contributing retained bytes.
    #[must_use]
    pub const fn oldest_sequence(&self) -> Option<u64> {
        self.oldest_sequence
    }

    /// Returns the newest output sequence contributing retained bytes.
    #[must_use]
    pub const fn newest_sequence(&self) -> Option<u64> {
        self.newest_sequence
    }

    /// Returns the oldest retained contributing sequence at or after
    /// `min_sequence`.
    #[must_use]
    pub fn oldest_sequence_at_or_after(&self, min_sequence: u64) -> Option<u64> {
        self.chunks
            .iter()
            .find(|chunk| chunk.sequence >= min_sequence)
            .map(|chunk| chunk.sequence)
    }

    /// Returns whether the retained bytes for `sequence` begin at that output
    /// event's first byte.
    #[must_use]
    pub fn starts_at_event_start(&self, sequence: u64) -> bool {
        self.chunks
            .iter()
            .find(|chunk| chunk.sequence == sequence)
            .is_some_and(|chunk| chunk.starts_at_event_start)
    }

    /// Returns the retained byte count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Returns whether the snapshot contains no retained bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// Per-pane bounded live output storage with independent cursor polling.
#[derive(Debug, Clone)]
pub struct OutputRing {
    event_capacity: usize,
    recent_byte_capacity: usize,
    retained_event_bytes: usize,
    next_sequence: u64,
    events: VecDeque<OutputEvent>,
    recent: RecentLiveBuffer,
}

impl OutputRing {
    /// Creates an empty output ring with explicit event and recent-byte limits.
    ///
    /// Both limits must be positive. A zero-sized ring would make every
    /// subscriber permanently lagged and is rejected at construction.
    #[must_use]
    pub fn new(event_capacity: usize, recent_byte_capacity: usize) -> Self {
        assert!(event_capacity > 0, "output ring capacity must be positive");
        assert!(
            recent_byte_capacity > 0,
            "recent live buffer capacity must be positive"
        );
        Self {
            event_capacity,
            recent_byte_capacity,
            retained_event_bytes: 0,
            next_sequence: 0,
            // Keep the retention budget logical, not preallocated. Idle panes
            // are common and should not pay for 1024 event slots before the
            // first byte is emitted.
            events: VecDeque::new(),
            recent: RecentLiveBuffer::new(recent_byte_capacity),
        }
    }

    /// Creates an empty output ring using the v1 defaults.
    #[must_use]
    pub fn with_default_capacities() -> Self {
        Self::new(
            DEFAULT_OUTPUT_RING_CAPACITY,
            DEFAULT_RECENT_LIVE_BUFFER_CAPACITY,
        )
    }

    /// Appends one output event, rotates the ring, and updates recent live bytes.
    pub fn push(&mut self, bytes: Vec<u8>) -> u64 {
        self.push_shared_with_recent_retention(bytes.into(), true)
    }

    /// Appends one output event, optionally updating the lag-recovery buffer.
    pub fn push_with_recent_retention(&mut self, bytes: Vec<u8>, retain_recent: bool) -> u64 {
        self.push_shared_with_recent_retention(bytes.into(), retain_recent)
    }

    /// Appends one shared output event without copying its byte payload.
    pub fn push_shared(&mut self, bytes: Arc<[u8]>) -> u64 {
        self.push_shared_with_recent_retention(bytes, true)
    }

    /// Appends one shared output event, optionally updating lag recovery bytes.
    pub fn push_shared_with_recent_retention(
        &mut self,
        bytes: Arc<[u8]>,
        retain_recent: bool,
    ) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("output ring sequence space exhausted");
        if retain_recent {
            self.recent.push(sequence, Arc::clone(&bytes));
        }
        self.retained_event_bytes = self.retained_event_bytes.saturating_add(bytes.len());
        self.events.push_back(OutputEvent {
            sequence,
            bytes,
            passthroughs: Vec::new(),
        });
        while self.events.len() > self.event_capacity {
            self.pop_oldest_event();
        }
        while self.retained_event_bytes > self.recent_byte_capacity && self.events.len() > 1 {
            self.pop_oldest_event();
        }
        sequence
    }

    /// Clears retained events and recent bytes without rewinding the sequence.
    pub fn clear_retained(&mut self) {
        self.events.clear();
        self.retained_event_bytes = 0;
        self.recent.clear();
    }

    /// Returns a cursor that starts with the oldest retained event.
    #[must_use]
    pub fn cursor_from_oldest(&self) -> OutputCursor {
        OutputCursor::new(self.oldest_sequence())
    }

    /// Returns a cursor that starts after the newest appended event.
    #[must_use]
    pub fn cursor_from_now(&self) -> OutputCursor {
        OutputCursor::new(self.next_sequence)
    }

    /// Returns a cursor that starts at an explicit output sequence.
    #[must_use]
    pub const fn cursor_from_sequence(sequence: u64) -> OutputCursor {
        OutputCursor::new(sequence)
    }

    /// Polls one item for `cursor`, reporting gaps before retained events.
    pub fn poll_cursor(&self, cursor: &mut OutputCursor) -> Option<OutputCursorItem> {
        let next = cursor.next_sequence();
        let oldest = self.oldest_sequence();
        if next < oldest {
            let missed = oldest.saturating_sub(next);
            cursor.record_gap(missed, oldest);
            return Some(OutputCursorItem::Gap(Box::new(OutputGap::new(
                next,
                oldest,
                missed,
                self.newest_sequence(),
                self.recent_snapshot(),
            ))));
        }

        if next >= self.next_sequence {
            return None;
        }

        let offset = usize::try_from(next.saturating_sub(oldest)).ok()?;
        let event = self.events.get(offset).cloned()?;
        cursor.advance_to(next.wrapping_add(1));
        Some(OutputCursorItem::Event(event))
    }

    /// Polls up to `limit` items for `cursor` from one retained-ring snapshot.
    ///
    /// A lag gap is returned only as the first item. Once the cursor is inside
    /// the retained range, the same immutable ring view cannot produce a later
    /// gap in this batch; callers therefore never advance over an event and
    /// then replace it with a lag response from a concurrently rotated ring.
    pub fn poll_cursor_batch(
        &self,
        cursor: &mut OutputCursor,
        limit: usize,
    ) -> Vec<OutputCursorItem> {
        let mut items = Vec::new();
        for _ in 0..limit {
            let Some(item) = self.poll_cursor(cursor) else {
                break;
            };
            let is_gap = matches!(item, OutputCursorItem::Gap(_));
            items.push(item);
            if is_gap {
                break;
            }
        }
        items
    }

    /// Returns the oldest retained event sequence, or the next sequence if empty.
    #[must_use]
    pub fn oldest_sequence(&self) -> u64 {
        self.events
            .front()
            .map_or(self.next_sequence, OutputEvent::sequence)
    }

    /// Returns the next sequence that will be assigned.
    #[must_use]
    pub const fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Returns the newest appended sequence, or zero before the first append.
    #[must_use]
    pub fn newest_sequence(&self) -> u64 {
        self.next_sequence.saturating_sub(1)
    }

    /// Returns the configured event capacity.
    #[must_use]
    pub const fn event_capacity(&self) -> usize {
        self.event_capacity
    }

    /// Returns the configured recent live byte capacity.
    #[must_use]
    pub const fn recent_byte_capacity(&self) -> usize {
        self.recent_byte_capacity
    }

    /// Returns retained event count.
    #[must_use]
    pub fn retained_len(&self) -> usize {
        self.events.len()
    }

    /// Returns the total raw bytes retained by event payloads.
    #[must_use]
    pub const fn retained_event_bytes(&self) -> usize {
        self.retained_event_bytes
    }

    /// Returns the total bytes currently retained in recent live storage.
    #[must_use]
    pub fn recent_len(&self) -> usize {
        self.recent.len()
    }

    /// Returns a bounded recent live output snapshot.
    #[must_use]
    pub fn recent_snapshot(&self) -> RecentOutputSnapshot {
        self.recent.snapshot()
    }

    /// Returns retained events in sequence order.
    #[must_use]
    pub fn retained_events(&self) -> Vec<OutputEvent> {
        self.events.iter().cloned().collect()
    }

    fn pop_oldest_event(&mut self) {
        if let Some(event) = self.events.pop_front() {
            self.retained_event_bytes = self.retained_event_bytes.saturating_sub(event.byte_len());
        }
    }
}

impl Default for OutputRing {
    fn default() -> Self {
        Self::with_default_capacities()
    }
}

#[derive(Debug, Clone)]
struct RecentLiveBuffer {
    capacity: usize,
    len: usize,
    chunks: VecDeque<RecentLiveChunk>,
}

#[derive(Debug, Clone)]
struct RecentLiveChunk {
    sequence: u64,
    bytes: Arc<[u8]>,
    start: usize,
    end: usize,
    starts_at_event_start: bool,
}

impl RecentLiveBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            len: 0,
            chunks: VecDeque::new(),
        }
    }

    fn push(&mut self, sequence: u64, bytes: Arc<[u8]>) {
        if bytes.is_empty() {
            return;
        }
        if bytes.len() >= self.capacity {
            let starts_at_event_start = bytes.len() == self.capacity;
            let bytes = if starts_at_event_start {
                bytes
            } else {
                Arc::from(&bytes[bytes.len() - self.capacity..])
            };
            self.chunks.clear();
            self.chunks.push_back(RecentLiveChunk {
                sequence,
                start: 0,
                end: bytes.len(),
                bytes,
                starts_at_event_start,
            });
            self.len = self.capacity;
            return;
        }
        let byte_len = bytes.len();
        self.chunks.push_back(RecentLiveChunk {
            sequence,
            bytes,
            start: 0,
            end: byte_len,
            starts_at_event_start: true,
        });
        self.len = self.len.saturating_add(byte_len);
        self.trim_front();
    }

    fn clear(&mut self) {
        self.chunks.clear();
        self.len = 0;
    }

    fn trim_front(&mut self) {
        while self.len > self.capacity {
            let overflow = self.len - self.capacity;
            let Some(front) = self.chunks.front_mut() else {
                self.len = 0;
                return;
            };
            let front_len = front.end.saturating_sub(front.start);
            if front_len <= overflow {
                self.len -= front_len;
                let _ = self.chunks.pop_front();
            } else {
                let start = front.start + overflow;
                front.bytes = Arc::from(&front.bytes[start..front.end]);
                front.start = 0;
                front.end = front.bytes.len();
                front.starts_at_event_start = false;
                self.len -= overflow;
            }
        }
    }

    const fn len(&self) -> usize {
        self.len
    }

    fn oldest_sequence(&self) -> Option<u64> {
        self.chunks.front().map(|chunk| chunk.sequence)
    }

    fn newest_sequence(&self) -> Option<u64> {
        self.chunks.back().map(|chunk| chunk.sequence)
    }

    fn snapshot(&self) -> RecentOutputSnapshot {
        let mut bytes = Vec::with_capacity(self.len);
        let mut snapshot_chunks = Vec::with_capacity(self.chunks.len());
        for chunk in &self.chunks {
            let start = bytes.len();
            bytes.extend_from_slice(&chunk.bytes[chunk.start..chunk.end]);
            snapshot_chunks.push(RecentOutputSnapshotChunk {
                sequence: chunk.sequence,
                start,
                starts_at_event_start: chunk.starts_at_event_start,
            });
        }
        RecentOutputSnapshot {
            bytes,
            oldest_sequence: self.oldest_sequence(),
            newest_sequence: self.newest_sequence(),
            chunks: snapshot_chunks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputRing, DEFAULT_OUTPUT_RING_CAPACITY, DEFAULT_RECENT_LIVE_BUFFER_CAPACITY};
    use crate::events::{OutputCursor, OutputCursorItem};

    #[test]
    fn default_capacities_match_recorded_budget() {
        let ring = OutputRing::default();
        assert_eq!(ring.event_capacity(), DEFAULT_OUTPUT_RING_CAPACITY);
        assert_eq!(
            ring.recent_byte_capacity(),
            DEFAULT_RECENT_LIVE_BUFFER_CAPACITY
        );
        assert_eq!(DEFAULT_OUTPUT_RING_CAPACITY, 2_048);
        assert_eq!(DEFAULT_RECENT_LIVE_BUFFER_CAPACITY, 262_144);
    }

    #[test]
    fn default_ring_preserves_immediate_sdk_burst_budget() {
        let mut ring = OutputRing::default();
        let mut cursor = ring.cursor_from_now();

        for index in 0..1_200 {
            ring.push(format!("line-{index:04}\n").into_bytes());
        }

        let Some(OutputCursorItem::Event(first)) = ring.poll_cursor(&mut cursor) else {
            panic!("default output ring must retain the head of a fresh SDK burst");
        };
        assert_eq!(first.sequence(), 0);
        assert_eq!(first.bytes(), b"line-0000\n");
    }

    #[test]
    fn ring_rotation_keeps_only_most_recent_events() {
        let mut ring = OutputRing::new(2, 64);
        ring.push(b"zero".to_vec());
        ring.push(b"one".to_vec());
        ring.push(b"two".to_vec());

        let sequences = ring
            .retained_events()
            .iter()
            .map(|event| event.sequence())
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![1, 2]);
        assert_eq!(ring.oldest_sequence(), 1);
        assert_eq!(ring.next_sequence(), 3);
    }

    #[test]
    fn ring_rotation_obeys_recent_byte_budget_for_retained_events() {
        let mut ring = OutputRing::new(8, 6);
        ring.push(b"aaaa".to_vec());
        ring.push(b"bbbb".to_vec());
        ring.push(b"cc".to_vec());

        let sequences = ring
            .retained_events()
            .iter()
            .map(|event| event.sequence())
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![1, 2]);
        assert_eq!(ring.retained_event_bytes(), 6);
        assert_eq!(ring.oldest_sequence(), 1);
    }

    #[test]
    fn ring_keeps_newest_event_when_single_event_exceeds_byte_budget() {
        let mut ring = OutputRing::new(8, 4);
        ring.push(b"abcdef".to_vec());

        assert_eq!(ring.retained_events().len(), 1);
        assert_eq!(ring.retained_event_bytes(), 6);
        assert_eq!(ring.oldest_sequence(), 0);
    }

    #[test]
    fn recent_live_buffer_obeys_byte_bound() {
        let mut ring = OutputRing::new(8, 5);
        ring.push(b"abc".to_vec());
        ring.push(b"defg".to_vec());
        ring.push(b"hi".to_vec());

        let snapshot = ring.recent_snapshot();
        assert_eq!(snapshot.bytes(), b"efghi");
        assert_eq!(snapshot.len(), 5);
        assert_eq!(snapshot.oldest_sequence(), Some(1));
        assert_eq!(snapshot.newest_sequence(), Some(2));
        assert_eq!(ring.recent_len(), 5);
    }

    #[test]
    fn recent_live_buffer_releases_trimmed_prefix_capacity() {
        let mut ring = OutputRing::new(8, 4);
        ring.push(b"abcd".to_vec());
        ring.push(b"ef".to_vec());

        assert_eq!(ring.recent_snapshot().bytes(), b"cdef");
        assert_eq!(ring.recent_len(), 4);
        let retained_capacity = ring
            .recent
            .chunks
            .iter()
            .map(|chunk| chunk.bytes.len())
            .sum::<usize>();
        assert!(
            retained_capacity <= ring.recent_byte_capacity(),
            "recent buffer retained capacity {retained_capacity} exceeds configured bound {}",
            ring.recent_byte_capacity()
        );
    }

    #[test]
    fn recent_live_buffer_trims_oversized_single_event_to_bound() {
        let mut ring = OutputRing::new(8, 4);
        ring.push(b"012345".to_vec());

        assert_eq!(ring.recent_snapshot().bytes(), b"2345");
        assert_eq!(ring.recent_snapshot().oldest_sequence(), Some(0));
        assert_eq!(ring.recent_snapshot().newest_sequence(), Some(0));
        assert_eq!(ring.recent_len(), 4);
        assert_eq!(ring.retained_events()[0].bytes(), b"012345");
    }

    #[test]
    fn recent_snapshot_filters_bytes_by_contributing_sequence() {
        let mut ring = OutputRing::new(8, 64);
        ring.push(b"stale".to_vec());
        ring.push(b"future".to_vec());
        ring.push(b"tail".to_vec());

        let snapshot = ring.recent_snapshot();

        assert_eq!(snapshot.bytes_from_sequence(0), b"stalefuturetail");
        assert_eq!(snapshot.bytes_from_sequence(1), b"futuretail");
        assert_eq!(snapshot.bytes_from_sequence(2), b"tail");
        assert_eq!(snapshot.bytes_from_sequence(3), b"");
        assert_eq!(snapshot.oldest_sequence_at_or_after(1), Some(1));
        assert_eq!(snapshot.oldest_sequence_at_or_after(3), None);
        assert!(snapshot.starts_at_event_start(1));
    }

    #[test]
    fn recent_snapshot_records_when_retained_event_prefix_was_trimmed() {
        let mut ring = OutputRing::new(8, 4);
        ring.push(b"012345".to_vec());

        let snapshot = ring.recent_snapshot();

        assert_eq!(snapshot.bytes_from_sequence(0), b"2345");
        assert_eq!(snapshot.oldest_sequence_at_or_after(0), Some(0));
        assert!(!snapshot.starts_at_event_start(0));
    }

    #[test]
    fn cursor_lag_across_full_rotation_reports_all_missed_events() {
        let mut ring = OutputRing::new(3, 16);
        let mut cursor = OutputCursor::new(0);
        for index in 0..6 {
            ring.push(format!("{index}").into_bytes());
        }

        let Some(OutputCursorItem::Gap(gap)) = ring.poll_cursor(&mut cursor) else {
            panic!("cursor should lag after ring rotation");
        };
        assert_eq!(gap.expected_sequence(), 0);
        assert_eq!(gap.resume_sequence(), 3);
        assert_eq!(gap.missed_events(), 3);
        assert_eq!(gap.missed_range(), 0..3);
        assert_eq!(gap.recent_snapshot().bytes(), b"012345");
        assert_eq!(gap.recent_snapshot().oldest_sequence(), Some(0));
        assert_eq!(gap.recent_snapshot().newest_sequence(), Some(5));
        assert_eq!(cursor.missed_events(), 3);
    }

    #[test]
    fn cursor_polls_rotated_ring_by_sequence_offset() {
        let mut ring = OutputRing::new(3, 16);
        for index in 0..6 {
            ring.push(format!("{index}").into_bytes());
        }
        let mut cursor = OutputCursor::new(4);

        let Some(OutputCursorItem::Event(event)) = ring.poll_cursor(&mut cursor) else {
            panic!("cursor should read retained event from rotated ring");
        };
        assert_eq!(event.sequence(), 4);
        assert_eq!(event.bytes(), b"4");
        assert_eq!(cursor.next_sequence(), 5);
    }

    #[test]
    fn batch_poll_reports_gap_only_for_lagged_cursor() {
        let mut ring = OutputRing::new(2, 16);
        let mut stale = OutputCursor::new(0);
        let mut aligned = OutputCursor::new(2);
        for index in 0..4 {
            ring.push(format!("{index}").into_bytes());
        }

        let stale_batch = ring.poll_cursor_batch(&mut stale, 8);
        assert_eq!(stale_batch.len(), 1);
        let OutputCursorItem::Gap(gap) = &stale_batch[0] else {
            panic!("stale cursor should report its own output gap");
        };
        assert_eq!(gap.expected_sequence(), 0);
        assert_eq!(gap.resume_sequence(), 2);
        assert_eq!(gap.missed_events(), 2);
        assert_eq!(stale.next_sequence(), 2);

        let aligned_batch = ring.poll_cursor_batch(&mut aligned, 8);
        let sequences = aligned_batch
            .iter()
            .map(|item| match item {
                OutputCursorItem::Event(event) => event.sequence(),
                OutputCursorItem::Gap(gap) => {
                    panic!("aligned cursor must not inherit stale cursor lag: {gap:?}")
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![2, 3]);
        assert_eq!(aligned.missed_events(), 0);
        assert_eq!(aligned.next_sequence(), ring.next_sequence());
    }

    #[test]
    fn clear_retained_drops_recent_snapshot_range_without_rewinding_sequence() {
        let mut ring = OutputRing::new(3, 16);
        let mut cursor = OutputCursor::new(0);
        ring.push(b"one".to_vec());
        ring.push(b"two".to_vec());

        ring.clear_retained();

        assert_eq!(ring.next_sequence(), 2);
        let snapshot = ring.recent_snapshot();
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.oldest_sequence(), None);
        assert_eq!(snapshot.newest_sequence(), None);

        let Some(OutputCursorItem::Gap(gap)) = ring.poll_cursor(&mut cursor) else {
            panic!("cursor should observe cleared retained output as a gap");
        };
        assert_eq!(gap.expected_sequence(), 0);
        assert_eq!(gap.resume_sequence(), 2);
        assert_eq!(gap.missed_events(), 2);
        assert_eq!(gap.missed_range(), 0..2);
        assert_eq!(gap.newest_sequence(), 1);
        assert!(gap.recent_snapshot().is_empty());
    }
}
