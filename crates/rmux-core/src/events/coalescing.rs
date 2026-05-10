//! Per-pane snapshot-revision coalescing used to bound notification rate.
//!
//! Pane snapshot revisions are derived inside `rmux-server` from the same
//! `PaneSnapshotResponse.revision` value returned to the snapshot endpoint.
//! When the pane state changes faster than subscribers can usefully consume
//! it (bursty PTY output, rapid resizes, scrollback clears), the server has
//! to limit the rate at which it pushes revision notifications without
//! losing the freshest revision. The data structure in this module owns
//! that decision purely in terms of `(revision: u64, observed_at: Instant)`
//! observations; it has no dependency on tokio, on the attach refresh
//! scheduler, or on output-ring sequence numbers.
//!
//! ### Contract
//!
//! - The cap is a per-pane budget of at most
//!   [`DEFAULT_MAX_SNAPSHOT_NOTIFICATIONS_PER_SECOND`] emitted notifications
//!   per second. The first emission is always free; subsequent emissions
//!   must be at least [`SnapshotCoalescer::min_interval`] apart.
//! - Revisions equal to the most recently emitted revision are suppressed,
//!   so a stable pane never spams notifications with the same value.
//! - When several distinct revisions arrive inside one coalescing window,
//!   only the newest observed revision is retained as pending and is the
//!   one emitted when the window opens. Older pending revisions are
//!   discarded so subscribers never observe a revision that was already
//!   superseded at the time of emission.
//! - The `pending` slot tracks the most recently observed unemitted
//!   revision. Polling once the rate-limit window opens promotes that
//!   pending value to an emission; if a duplicate of the last emitted
//!   revision is observed while there is no pending value, the coalescer
//!   stays idle.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rmux_proto::PaneId;

/// v1 RFC default: at most 60 emitted snapshot notifications per second per pane.
pub const DEFAULT_MAX_SNAPSHOT_NOTIFICATIONS_PER_SECOND: u32 = 60;

/// Returns the minimum spacing between emissions for a given per-second rate.
///
/// `rate == 0` is treated as "never emit"; the returned interval is
/// [`Duration::MAX`]. For any positive rate the spacing is
/// `1_000_000_000 / rate` nanoseconds, floor-rounded so the cap is
/// strictly honored across any 1-second window.
#[must_use]
pub const fn min_interval_for_rate(rate: u32) -> Duration {
    if rate == 0 {
        Duration::MAX
    } else {
        let nanos_per_second: u64 = 1_000_000_000;
        Duration::from_nanos(nanos_per_second / rate as u64)
    }
}

#[derive(Debug, Clone, Copy)]
struct EmittedRevision {
    revision: u64,
    at: Instant,
}

#[derive(Debug, Clone, Copy)]
struct PendingRevision {
    revision: u64,
    observed_at: Instant,
}

/// Per-pane snapshot revision coalescer.
///
/// The coalescer is fed the actual `PaneSnapshotResponse.revision` value
/// for a given pane through [`SnapshotCoalescer::observe`]. It returns the
/// revision to deliver to subscribers when the rate-limit window allows
/// emission, otherwise it stores the freshest unemitted revision as
/// pending. Callers can drain pending values later through
/// [`SnapshotCoalescer::poll`]; the next allowed emission instant is
/// reported by [`SnapshotCoalescer::next_deadline`].
#[derive(Debug, Clone)]
pub struct SnapshotCoalescer {
    max_per_second: u32,
    min_interval: Duration,
    last_emitted: Option<EmittedRevision>,
    pending: Option<PendingRevision>,
}

impl SnapshotCoalescer {
    /// Builds a coalescer that allows at most `max_per_second` emitted
    /// notifications per second.
    #[must_use]
    pub fn new(max_per_second: u32) -> Self {
        Self {
            max_per_second,
            min_interval: min_interval_for_rate(max_per_second),
            last_emitted: None,
            pending: None,
        }
    }

    /// Builds a coalescer using the v1 RFC default rate.
    #[must_use]
    pub fn with_default_rate() -> Self {
        Self::new(DEFAULT_MAX_SNAPSHOT_NOTIFICATIONS_PER_SECOND)
    }

    /// Returns the configured emissions-per-second cap.
    #[must_use]
    pub const fn max_per_second(&self) -> u32 {
        self.max_per_second
    }

    /// Returns the minimum spacing the coalescer enforces between emissions.
    #[must_use]
    pub const fn min_interval(&self) -> Duration {
        self.min_interval
    }

    /// Records a newly observed snapshot revision.
    ///
    /// Returns `Some(revision)` if the cap allows emission immediately and
    /// the revision is not a duplicate of the last emitted value. Returns
    /// `None` if the revision was held back, suppressed as a duplicate, or
    /// stored as pending for a future poll.
    pub fn observe(&mut self, revision: u64, now: Instant) -> Option<u64> {
        if let Some(emitted) = self.last_emitted {
            if emitted.revision == revision {
                if let Some(pending) = self.pending {
                    if pending.revision == revision {
                        // The pending slot only carries the newest unemitted
                        // revision; if it equals the last emitted value
                        // there is nothing left to deliver.
                        self.pending = None;
                    }
                }
                return None;
            }
        }

        if self.may_emit_at(now) {
            self.commit_emission(revision, now);
            self.pending = None;
            return Some(revision);
        }

        // The freshest unemitted revision wins; older pending values are
        // discarded so subscribers never observe a revision that was
        // already superseded at the time of emission.
        self.pending = Some(PendingRevision {
            revision,
            observed_at: now,
        });
        None
    }

    /// Drains any pending revision once the rate-limit window allows.
    ///
    /// Returns `Some(revision)` exactly when the pending slot can now be
    /// promoted to an emission. Otherwise returns `None`.
    pub fn poll(&mut self, now: Instant) -> Option<u64> {
        let pending = self.pending?;
        if !self.may_emit_at(now) {
            return None;
        }
        if let Some(emitted) = self.last_emitted {
            if emitted.revision == pending.revision {
                self.pending = None;
                return None;
            }
        }
        self.commit_emission(pending.revision, now);
        self.pending = None;
        Some(pending.revision)
    }

    /// Returns the earliest `Instant` at which a pending value could be
    /// emitted, or `None` if there is nothing pending.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        let pending = self.pending?;
        match self.last_emitted {
            None => Some(pending.observed_at),
            Some(emitted) => {
                let earliest = emitted.at.checked_add(self.min_interval);
                match earliest {
                    Some(earliest) => Some(std::cmp::max(pending.observed_at, earliest)),
                    None => Some(pending.observed_at),
                }
            }
        }
    }

    /// Returns the most recently emitted revision, if any.
    #[must_use]
    pub fn last_emitted_revision(&self) -> Option<u64> {
        self.last_emitted.map(|emitted| emitted.revision)
    }

    /// Returns the pending unemitted revision, if any.
    #[must_use]
    pub fn pending_revision(&self) -> Option<u64> {
        self.pending.map(|pending| pending.revision)
    }

    /// Returns whether the coalescer would emit immediately at `now`.
    #[must_use]
    pub fn may_emit_at(&self, now: Instant) -> bool {
        match self.last_emitted {
            None => true,
            Some(emitted) => now.saturating_duration_since(emitted.at) >= self.min_interval,
        }
    }

    fn commit_emission(&mut self, revision: u64, at: Instant) {
        self.last_emitted = Some(EmittedRevision { revision, at });
    }
}

impl Default for SnapshotCoalescer {
    fn default() -> Self {
        Self::with_default_rate()
    }
}

/// Per-pane registry of [`SnapshotCoalescer`] instances.
///
/// The registry lazily allocates a coalescer the first time a pane id is
/// observed. All stored coalescers share the same per-second cap that the
/// registry was constructed with so cap accounting stays uniform.
#[derive(Debug, Clone)]
pub struct PaneSnapshotCoalescerRegistry {
    coalescers: HashMap<PaneId, SnapshotCoalescer>,
    max_per_second: u32,
}

impl PaneSnapshotCoalescerRegistry {
    /// Builds a registry that constructs new coalescers using `max_per_second`.
    #[must_use]
    pub fn new(max_per_second: u32) -> Self {
        Self {
            coalescers: HashMap::new(),
            max_per_second,
        }
    }

    /// Builds a registry using the v1 RFC default rate.
    #[must_use]
    pub fn with_default_rate() -> Self {
        Self::new(DEFAULT_MAX_SNAPSHOT_NOTIFICATIONS_PER_SECOND)
    }

    /// Returns the configured per-second cap.
    #[must_use]
    pub const fn max_per_second(&self) -> u32 {
        self.max_per_second
    }

    /// Records a snapshot revision for the given pane and returns whatever
    /// the coalescer would emit immediately.
    pub fn observe(&mut self, pane_id: PaneId, revision: u64, now: Instant) -> Option<u64> {
        self.entry(pane_id).observe(revision, now)
    }

    /// Drains any pending revision for the given pane that is now eligible
    /// to be emitted.
    pub fn poll(&mut self, pane_id: PaneId, now: Instant) -> Option<u64> {
        self.coalescers.get_mut(&pane_id)?.poll(now)
    }

    /// Returns the next emission deadline across all panes, if any.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.coalescers
            .values()
            .filter_map(SnapshotCoalescer::next_deadline)
            .min()
    }

    /// Returns the last emitted revision recorded for a pane, if any.
    #[must_use]
    pub fn last_emitted_revision(&self, pane_id: PaneId) -> Option<u64> {
        self.coalescers
            .get(&pane_id)
            .and_then(SnapshotCoalescer::last_emitted_revision)
    }

    /// Returns the pending unemitted revision recorded for a pane, if any.
    #[must_use]
    pub fn pending_revision(&self, pane_id: PaneId) -> Option<u64> {
        self.coalescers
            .get(&pane_id)
            .and_then(SnapshotCoalescer::pending_revision)
    }

    /// Removes the coalescer state attached to a pane and returns it.
    pub fn forget(&mut self, pane_id: PaneId) -> Option<SnapshotCoalescer> {
        self.coalescers.remove(&pane_id)
    }

    /// Returns the number of panes currently tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.coalescers.len()
    }

    /// Returns whether the registry currently tracks any pane.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.coalescers.is_empty()
    }

    fn entry(&mut self, pane_id: PaneId) -> &mut SnapshotCoalescer {
        let max_per_second = self.max_per_second;
        self.coalescers
            .entry(pane_id)
            .or_insert_with(|| SnapshotCoalescer::new(max_per_second))
    }
}

impl Default for PaneSnapshotCoalescerRegistry {
    fn default() -> Self {
        Self::with_default_rate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    #[test]
    fn min_interval_for_rate_handles_zero_and_default_rate() {
        assert_eq!(min_interval_for_rate(0), Duration::MAX);
        // 60 emissions per second yields a floor-rounded 16.666... ms interval.
        let interval = min_interval_for_rate(60);
        assert_eq!(interval, Duration::from_nanos(1_000_000_000 / 60));
        assert!(interval < Duration::from_millis(17));
        assert!(interval > Duration::from_millis(16));
    }

    #[test]
    fn first_observation_emits_immediately_and_records_revision() {
        let mut coalescer = SnapshotCoalescer::with_default_rate();
        let now = Instant::now();
        assert_eq!(coalescer.observe(7, now), Some(7));
        assert_eq!(coalescer.last_emitted_revision(), Some(7));
        assert_eq!(coalescer.pending_revision(), None);
        assert_eq!(coalescer.next_deadline(), None);
    }

    #[test]
    fn duplicate_revision_is_suppressed_even_after_window_opens() {
        let mut coalescer = SnapshotCoalescer::with_default_rate();
        let base = Instant::now();
        assert_eq!(coalescer.observe(11, base), Some(11));
        // Same revision inside the window: held back as pending? No — duplicates
        // are suppressed regardless of timing.
        assert_eq!(coalescer.observe(11, at(base, 1)), None);
        assert_eq!(coalescer.pending_revision(), None);
        // After the window opens, the duplicate must still not produce a
        // notification because its revision matches the last emitted value.
        let after = base + coalescer.min_interval();
        assert_eq!(coalescer.observe(11, after), None);
        assert_eq!(coalescer.last_emitted_revision(), Some(11));
    }

    #[test]
    fn newest_revision_overwrites_older_pending_inside_window() {
        let mut coalescer = SnapshotCoalescer::new(60);
        let base = Instant::now();
        assert_eq!(coalescer.observe(1, base), Some(1));
        // Three rapid observations inside the same coalescing window.
        assert_eq!(coalescer.observe(2, at(base, 1)), None);
        assert_eq!(coalescer.observe(3, at(base, 2)), None);
        assert_eq!(coalescer.observe(4, at(base, 3)), None);
        assert_eq!(coalescer.pending_revision(), Some(4));
        // When the window opens, only revision 4 is delivered: 2 and 3 are
        // suppressed because they were superseded before the cap allowed an
        // emission.
        let after = base + coalescer.min_interval();
        assert_eq!(coalescer.poll(after), Some(4));
        assert_eq!(coalescer.pending_revision(), None);
        assert_eq!(coalescer.last_emitted_revision(), Some(4));
    }

    #[test]
    fn poll_returns_none_until_min_interval_has_elapsed() {
        let mut coalescer = SnapshotCoalescer::new(60);
        let base = Instant::now();
        assert_eq!(coalescer.observe(10, base), Some(10));
        assert_eq!(coalescer.observe(11, at(base, 1)), None);
        // Just before the window opens: still no emission.
        let just_before = base + coalescer.min_interval() - Duration::from_nanos(1);
        assert_eq!(coalescer.poll(just_before), None);
        // At the boundary: emission allowed.
        let at_boundary = base + coalescer.min_interval();
        assert_eq!(coalescer.poll(at_boundary), Some(11));
    }

    #[test]
    fn cap_holds_at_most_max_per_second_per_pane_under_dense_observations() {
        // Drive the coalescer faster than 1 kHz over a full second and
        // confirm that emissions never exceed the configured cap. This is
        // the cap check requested by the chain coverage contract; it does not
        // touch any tokio scheduler or attach refresh scheduler.
        let mut coalescer = SnapshotCoalescer::new(60);
        let base = Instant::now();
        let mut emitted: Vec<u64> = Vec::new();
        let mut revision: u64 = 0;
        let mut cursor: u64 = 0;
        // Observe at 1 kHz for 1,000 ms (1,000 distinct revisions). Then
        // poll the trailing pending value at the end of the window.
        while cursor <= 1_000 {
            revision = revision.wrapping_add(1);
            let now = at(base, cursor);
            if let Some(value) = coalescer.observe(revision, now) {
                emitted.push(value);
            }
            cursor += 1;
        }
        // Drain whatever is pending at the end of the second.
        if let Some(value) = coalescer.poll(at(base, cursor)) {
            emitted.push(value);
        }
        assert!(
            emitted.len() <= 60,
            "coalescer emitted {} notifications (cap is 60/s)",
            emitted.len(),
        );
        // The cap should also be saturated — feeding 1 kHz of distinct
        // revisions for one second should produce close to the cap.
        assert!(
            emitted.len() >= 55,
            "expected near-saturation under 1 kHz observations, got {}",
            emitted.len(),
        );
    }

    #[test]
    fn delivered_revisions_track_the_observation_order_after_resize_or_clear() {
        // A resize, scrollback clear, lag, or bursty PTY output simply
        // produces a fresh revision value. The coalescer must not reorder
        // these or replay an older revision after a newer one has been
        // emitted, even when the new revision happens to numerically sort
        // below the previous one (revisions are hash-derived).
        let mut coalescer = SnapshotCoalescer::new(120);
        let base = Instant::now();
        // Emit one revision, then a numerically-smaller fresh revision
        // outside the window: the second emission must still happen and
        // become the new last_emitted_revision.
        assert_eq!(
            coalescer.observe(0xFFFF_FFFF_FFFF_FFFF, base),
            Some(0xFFFF_FFFF_FFFF_FFFF)
        );
        let after_first = base + coalescer.min_interval();
        assert_eq!(
            coalescer.observe(0x0000_0000_0000_0001, after_first),
            Some(1)
        );
        assert_eq!(coalescer.last_emitted_revision(), Some(1));
        // Now bursty output during one window: only the freshest stays.
        assert_eq!(
            coalescer.observe(2, after_first + Duration::from_nanos(1)),
            None
        );
        assert_eq!(
            coalescer.observe(3, after_first + Duration::from_nanos(2)),
            None
        );
        assert_eq!(coalescer.pending_revision(), Some(3));
        // After the window opens, only 3 is delivered. 2 is dropped.
        let after_second = after_first + coalescer.min_interval();
        assert_eq!(coalescer.poll(after_second), Some(3));
        assert_eq!(coalescer.last_emitted_revision(), Some(3));
    }

    #[test]
    fn next_deadline_reports_the_pending_release_time() {
        let mut coalescer = SnapshotCoalescer::new(60);
        let base = Instant::now();
        assert_eq!(coalescer.observe(1, base), Some(1));
        assert_eq!(coalescer.next_deadline(), None);
        assert_eq!(coalescer.observe(2, at(base, 1)), None);
        let deadline = coalescer
            .next_deadline()
            .expect("pending implies a deadline");
        assert_eq!(deadline, base + coalescer.min_interval());
    }

    #[test]
    fn registry_serves_per_pane_state_and_forgets_on_request() {
        let mut registry = PaneSnapshotCoalescerRegistry::new(60);
        let base = Instant::now();
        let pane_a = PaneId::new(1);
        let pane_b = PaneId::new(2);
        assert_eq!(registry.observe(pane_a, 100, base), Some(100));
        assert_eq!(registry.observe(pane_b, 200, base), Some(200));
        // Per-pane cap accounting is independent: pane_a has used its
        // first emission, pane_b has used its own. Both are now in the
        // post-emission cooldown.
        assert_eq!(registry.observe(pane_a, 101, at(base, 1)), None);
        assert_eq!(registry.observe(pane_b, 201, at(base, 1)), None);
        assert_eq!(registry.last_emitted_revision(pane_a), Some(100));
        assert_eq!(registry.last_emitted_revision(pane_b), Some(200));
        assert_eq!(registry.pending_revision(pane_a), Some(101));
        assert_eq!(registry.pending_revision(pane_b), Some(201));
        let after = base + min_interval_for_rate(registry.max_per_second());
        assert_eq!(registry.poll(pane_a, after), Some(101));
        assert_eq!(registry.poll(pane_b, after), Some(201));
        assert_eq!(registry.len(), 2);
        assert!(registry.forget(pane_a).is_some());
        assert!(registry.forget(pane_a).is_none());
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn registry_default_uses_rfc_recorded_rate() {
        let registry = PaneSnapshotCoalescerRegistry::default();
        assert_eq!(
            registry.max_per_second(),
            DEFAULT_MAX_SNAPSHOT_NOTIFICATIONS_PER_SECOND,
        );
        assert_eq!(DEFAULT_MAX_SNAPSHOT_NOTIFICATIONS_PER_SECOND, 60);
    }
}
