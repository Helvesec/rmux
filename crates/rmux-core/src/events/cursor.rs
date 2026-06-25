use super::ring::{OutputEvent, RecentOutputSnapshot};
use std::ops::Range;

/// Independent read position for one pane-output subscriber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputCursor {
    next_sequence: u64,
    missed_events: u64,
}

impl OutputCursor {
    /// Creates a cursor that will next read `next_sequence`.
    #[must_use]
    pub const fn new(next_sequence: u64) -> Self {
        Self {
            next_sequence,
            missed_events: 0,
        }
    }

    /// Returns the next sequence this cursor expects to read.
    #[must_use]
    pub const fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Returns the total number of events this cursor has explicitly missed.
    #[must_use]
    pub const fn missed_events(&self) -> u64 {
        self.missed_events
    }

    pub(super) fn advance_to(&mut self, next_sequence: u64) {
        self.next_sequence = next_sequence;
    }

    /// Advances past `sequence` only when it is exactly the next retained event.
    ///
    /// This lets live-output fast paths consume an already identified event
    /// without exposing arbitrary cursor rewinds to callers.
    pub fn advance_past_sequence(&mut self, sequence: u64) -> bool {
        if self.next_sequence != sequence {
            return false;
        }
        self.next_sequence = sequence.wrapping_add(1);
        true
    }

    pub(super) fn record_gap(&mut self, missed: u64, resume_sequence: u64) {
        self.missed_events = self.missed_events.saturating_add(missed);
        self.next_sequence = resume_sequence;
    }
}

/// One cursor poll result from an [`OutputRing`](super::ring::OutputRing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputCursorItem {
    /// A retained output event.
    Event(OutputEvent),
    /// The cursor fell behind the oldest retained event.
    Gap(Box<OutputGap>),
}

/// Explicit report for output events that no longer fit in the ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputGap {
    expected_sequence: u64,
    resume_sequence: u64,
    missed_events: u64,
    newest_sequence: u64,
    recent_snapshot: RecentOutputSnapshot,
}

impl OutputGap {
    pub(super) const fn new(
        expected_sequence: u64,
        resume_sequence: u64,
        missed_events: u64,
        newest_sequence: u64,
        recent_snapshot: RecentOutputSnapshot,
    ) -> Self {
        Self {
            expected_sequence,
            resume_sequence,
            missed_events,
            newest_sequence,
            recent_snapshot,
        }
    }

    /// Returns the sequence the cursor expected before lag was detected.
    #[must_use]
    pub const fn expected_sequence(&self) -> u64 {
        self.expected_sequence
    }

    /// Returns the oldest retained sequence the cursor can resume from.
    #[must_use]
    pub const fn resume_sequence(&self) -> u64 {
        self.resume_sequence
    }

    /// Returns the number of events skipped by this gap.
    #[must_use]
    pub const fn missed_events(&self) -> u64 {
        self.missed_events
    }

    /// Returns the half-open output sequence range skipped by this gap.
    #[must_use]
    pub fn missed_range(&self) -> Range<u64> {
        self.expected_sequence..self.resume_sequence
    }

    /// Returns the newest appended sequence when the gap was reported.
    #[must_use]
    pub const fn newest_sequence(&self) -> u64 {
        self.newest_sequence
    }

    /// Returns the bounded recent live bytes available at gap detection time.
    #[must_use]
    pub const fn recent_snapshot(&self) -> &RecentOutputSnapshot {
        &self.recent_snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputCursor, OutputCursorItem};
    use crate::events::OutputRing;

    #[test]
    fn output_cursor_item_size_stays_bounded() {
        assert!(
            std::mem::size_of::<OutputCursorItem>() <= 48,
            "OutputCursorItem should stay compact for batched cursor vectors, got {}",
            std::mem::size_of::<OutputCursorItem>()
        );
    }

    #[test]
    fn cursor_advances_independently_through_retained_events() {
        let mut ring = OutputRing::new(8, 64);
        ring.push(b"one".to_vec());
        ring.push(b"two".to_vec());
        let mut first = ring.cursor_from_oldest();
        let mut second = ring.cursor_from_oldest();

        assert_eq!(
            ring.poll_cursor(&mut first),
            Some(OutputCursorItem::Event(ring.retained_events()[0].clone()))
        );
        assert_eq!(first.next_sequence(), 1);
        assert_eq!(second.next_sequence(), 0);

        assert_eq!(
            ring.poll_cursor(&mut first),
            Some(OutputCursorItem::Event(ring.retained_events()[1].clone()))
        );
        assert_eq!(ring.poll_cursor(&mut first), None);
        assert_eq!(first.next_sequence(), ring.next_sequence());

        assert_eq!(
            ring.poll_cursor(&mut second),
            Some(OutputCursorItem::Event(ring.retained_events()[0].clone()))
        );
        assert_eq!(second.next_sequence(), 1);
    }

    #[test]
    fn lagged_cursor_reports_explicit_gap_and_resumes_at_oldest_event() {
        let mut ring = OutputRing::new(2, 64);
        let mut cursor = OutputCursor::new(0);
        for bytes in [b"zero".as_slice(), b"one".as_slice(), b"two".as_slice()] {
            ring.push(bytes.to_vec());
        }

        let Some(OutputCursorItem::Gap(gap)) = ring.poll_cursor(&mut cursor) else {
            panic!("cursor should report lag");
        };
        assert_eq!(gap.expected_sequence(), 0);
        assert_eq!(gap.resume_sequence(), 1);
        assert_eq!(gap.missed_events(), 1);
        assert_eq!(gap.missed_range(), 0..1);
        assert_eq!(gap.newest_sequence(), 2);
        assert_eq!(gap.recent_snapshot().oldest_sequence(), Some(0));
        assert_eq!(gap.recent_snapshot().newest_sequence(), Some(2));
        assert_eq!(cursor.missed_events(), 1);
        assert_eq!(cursor.next_sequence(), 1);

        let Some(OutputCursorItem::Event(event)) = ring.poll_cursor(&mut cursor) else {
            panic!("cursor should resume with oldest retained event");
        };
        assert_eq!(event.sequence(), 1);
        assert_eq!(event.bytes(), b"one");
    }
}
