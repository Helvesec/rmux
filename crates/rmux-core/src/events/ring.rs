use std::collections::VecDeque;

use super::cursor::{OutputCursor, OutputCursorItem, OutputGap};

/// Default retained pane-output events per pane.
pub const DEFAULT_OUTPUT_RING_CAPACITY: usize = 1024;
/// Default retained recent live output bytes per pane.
pub const DEFAULT_RECENT_LIVE_BUFFER_CAPACITY: usize = 1024 * 1024;

/// A single pane-output event retained by an [`OutputRing`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputEvent {
    sequence: u64,
    bytes: Vec<u8>,
}

impl OutputEvent {
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

    /// Consumes this event and returns its raw bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Bounded recent live bytes retained alongside an output ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentOutputSnapshot {
    bytes: Vec<u8>,
    oldest_sequence: Option<u64>,
    newest_sequence: Option<u64>,
}

impl RecentOutputSnapshot {
    /// Returns the retained recent live bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
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
            next_sequence: 0,
            events: VecDeque::with_capacity(event_capacity),
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
    pub fn push(&mut self, bytes: Vec<u8>) -> OutputEvent {
        let event = OutputEvent {
            sequence: self.next_sequence,
            bytes,
        };
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("output ring sequence space exhausted");
        self.recent.push(event.sequence, &event.bytes);
        self.events.push_back(event.clone());
        while self.events.len() > self.event_capacity {
            let _ = self.events.pop_front();
        }
        event
    }

    /// Clears retained events and recent bytes without rewinding the sequence.
    pub fn clear_retained(&mut self) {
        self.events.clear();
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

    /// Polls one item for `cursor`, reporting gaps before retained events.
    pub fn poll_cursor(&self, cursor: &mut OutputCursor) -> Option<OutputCursorItem> {
        let next = cursor.next_sequence();
        let oldest = self.oldest_sequence();
        if next < oldest {
            let missed = oldest.saturating_sub(next);
            cursor.record_gap(missed, oldest);
            return Some(OutputCursorItem::Gap(OutputGap::new(
                next,
                oldest,
                missed,
                self.newest_sequence(),
                self.recent_snapshot(),
            )));
        }

        if next >= self.next_sequence {
            return None;
        }

        let offset = usize::try_from(next.saturating_sub(oldest)).ok()?;
        let event = self.events.get(offset).cloned()?;
        cursor.advance_to(next.wrapping_add(1));
        Some(OutputCursorItem::Event(event))
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

    /// Returns the total bytes currently retained in recent live storage.
    #[must_use]
    pub fn recent_len(&self) -> usize {
        self.recent.len()
    }

    /// Returns a bounded recent live output snapshot.
    #[must_use]
    pub fn recent_snapshot(&self) -> RecentOutputSnapshot {
        RecentOutputSnapshot {
            bytes: self.recent.bytes(),
            oldest_sequence: self.recent.oldest_sequence(),
            newest_sequence: self.recent.newest_sequence(),
        }
    }

    /// Returns retained events in sequence order.
    #[must_use]
    pub fn retained_events(&self) -> Vec<OutputEvent> {
        self.events.iter().cloned().collect()
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
    bytes: Vec<u8>,
}

impl RecentLiveBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            len: 0,
            chunks: VecDeque::new(),
        }
    }

    fn push(&mut self, sequence: u64, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if bytes.len() >= self.capacity {
            self.chunks.clear();
            self.chunks.push_back(RecentLiveChunk {
                sequence,
                bytes: bytes[bytes.len() - self.capacity..].to_vec(),
            });
            self.len = self.capacity;
            return;
        }
        self.chunks.push_back(RecentLiveChunk {
            sequence,
            bytes: bytes.to_vec(),
        });
        self.len = self.len.saturating_add(bytes.len());
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
            if front.bytes.len() <= overflow {
                self.len -= front.bytes.len();
                let _ = self.chunks.pop_front();
            } else {
                front.bytes = front.bytes.split_off(overflow);
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

    fn bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.len);
        for chunk in &self.chunks {
            bytes.extend_from_slice(&chunk.bytes);
        }
        bytes
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
        assert_eq!(DEFAULT_OUTPUT_RING_CAPACITY, 1_024);
        assert_eq!(DEFAULT_RECENT_LIVE_BUFFER_CAPACITY, 1_048_576);
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
            .map(|chunk| chunk.bytes.capacity())
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
