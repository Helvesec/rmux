use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const INITIAL_DELAY: Duration = Duration::from_millis(100);
const MAX_DELAY: Duration = Duration::from_secs(10);
const RESET_AFTER: Duration = Duration::from_secs(5 * 60);
const GC_AFTER: Duration = Duration::from_secs(10 * 60);
const INITIAL_LOCK_DURATION: Duration = Duration::from_secs(60 * 60);
const MAX_LOCK_DURATION: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_BACKOFF_ENTRIES: usize = 4096;
const MAX_SHIFT: u32 = 7;
/// Authentication-failure budget per authentication key before each temporary
/// lock. The lock duration escalates across windows, so a leaked link cannot get
/// a fresh 50 guesses every hour for the whole share lifetime.
const LOCK_FAIL_CAP: u32 = 50;
/// High cumulative budget per authentication key. This keeps a 6-digit PIN from
/// becoming brute-forceable on long-lived shares while avoiding the previous
/// foot-gun where 50 bad guesses permanently denied a role.
const LIFETIME_FAIL_CAP: u32 = 500;
const MAX_LOCK_SHIFT: u32 = 8;

/// Outcome of reserving an attempt with [`AuthBackoff::begin_attempt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttemptDecision {
    /// Proceed after waiting this long; the caller owes exactly one
    /// [`AuthBackoff::settle`].
    Wait(Duration),
    /// The key exhausted its temporary-lock failure budget. Reject without
    /// reserving an attempt — the caller must fail closed and MUST NOT settle.
    Locked,
}

/// Cancellation-safe reservation returned by [`AuthBackoff::reserve_attempt`].
#[derive(Debug)]
pub(super) enum AttemptReservation<'a> {
    /// Proceed after waiting this long. Dropping the guard without settling
    /// releases the in-flight slot as a non-authentication outcome.
    Wait {
        delay: Duration,
        guard: AttemptGuard<'a>,
    },
    /// The key exhausted its temporary-lock failure budget. No attempt was
    /// reserved.
    Locked,
}

/// RAII guard for an in-flight authentication attempt.
#[derive(Debug)]
#[must_use = "dropping the guard settles the attempt as a non-authentication outcome"]
pub(super) struct AttemptGuard<'a> {
    backoff: &'a AuthBackoff,
    share_id: String,
    settled: bool,
}

impl AttemptGuard<'_> {
    pub(super) fn settle(mut self, outcome: AttemptOutcome) {
        self.backoff.settle(&self.share_id, outcome);
        self.settled = true;
    }
}

impl Drop for AttemptGuard<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.backoff.settle(&self.share_id, AttemptOutcome::Other);
        }
    }
}

/// How a connect attempt reserved by [`AuthBackoff::begin_attempt`] resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttemptOutcome {
    /// Authentication succeeded; the backoff for this key is cleared.
    Success,
    /// An authentication failure (bad token / bad PIN); escalates the backoff.
    AuthFailure,
    /// A non-authentication rejection (e.g. capacity reached, missing PIN); does
    /// not escalate, so a full or misconfigured share does not lock legitimate
    /// viewers out.
    Other,
}

/// Per-key authentication rate limiter.
///
/// The delay escalates with the number of confirmed failures **and** the number
/// of attempts currently in flight. Counting in-flight attempts is what stops a
/// burst of concurrent guesses from slipping through in a single window — a
/// past-failure-only gate would let every concurrent guess read a zero delay
/// before any of them recorded a failure.
#[derive(Debug, Default)]
pub(super) struct AuthBackoff {
    entries: Mutex<HashMap<String, BackoffEntry>>,
}

#[derive(Debug)]
struct BackoffEntry {
    /// Confirmed authentication failures (drives the escalating delay). Reset
    /// after `RESET_AFTER` idle and on success.
    fails: u32,
    /// Attempts started but not yet settled.
    in_flight: u32,
    /// Confirmed authentication failures in this lock window. Reset on success
    /// and after the temporary lock expires.
    lock_window_fails: u32,
    /// Confirmed authentication failures over the entry's life. Reset only on
    /// success; used for escalating locks and the high cumulative cap. The idle
    /// GC must not discard this counter, otherwise a long-lived leaked share can
    /// regain a fresh guess budget after every lock window.
    lifetime_fails: u32,
    locked_until: Option<Instant>,
    last_attempt_at: Instant,
}

impl BackoffEntry {
    fn unlock_if_expired(&mut self, now: Instant) {
        if self.locked_until.is_some_and(|deadline| now >= deadline) {
            self.fails = 0;
            self.in_flight = 0;
            self.lock_window_fails = 0;
            self.locked_until = None;
        }
    }

    fn locked(&self, now: Instant) -> bool {
        self.permanently_locked() || self.locked_until.is_some_and(|deadline| now < deadline)
    }

    fn permanently_locked(&self) -> bool {
        self.lifetime_fails >= LIFETIME_FAIL_CAP
    }
}

impl AuthBackoff {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Reserves an attempt for `share_id` and returns how long the caller must
    /// wait before proceeding. Every call MUST be balanced by exactly one
    /// [`Self::settle`].
    pub(super) fn begin_attempt(&self, share_id: &str) -> AttemptDecision {
        self.begin_attempt_at(share_id, Instant::now())
    }

    /// Reserves an attempt with a drop guard, making async cancellation safe.
    pub(super) fn reserve_attempt(&self, share_id: &str) -> AttemptReservation<'_> {
        match self.begin_attempt(share_id) {
            AttemptDecision::Wait(delay) => AttemptReservation::Wait {
                delay,
                guard: AttemptGuard {
                    backoff: self,
                    share_id: share_id.to_owned(),
                    settled: false,
                },
            },
            AttemptDecision::Locked => AttemptReservation::Locked,
        }
    }

    fn begin_attempt_at(&self, share_id: &str, now: Instant) -> AttemptDecision {
        let mut entries = self.entries.lock().expect("backoff mutex poisoned");
        retain_recent_entries(&mut entries, now);
        if !entries.contains_key(share_id) && entries.len() >= MAX_BACKOFF_ENTRIES {
            evict_oldest_entry(&mut entries);
        }
        let entry = entries.entry(share_id.to_owned()).or_insert(BackoffEntry {
            fails: 0,
            in_flight: 0,
            lock_window_fails: 0,
            lifetime_fails: 0,
            locked_until: None,
            last_attempt_at: now,
        });
        entry.unlock_if_expired(now);
        // A temporary lock takes priority over the idle reset below. Do not move
        // `locked_until`: repeated bad attempts must not extend a role lock.
        if entry.locked(now) {
            return AttemptDecision::Locked;
        }
        if now.duration_since(entry.last_attempt_at) >= RESET_AFTER {
            entry.fails = 0;
            entry.in_flight = 0;
        }
        entry.last_attempt_at = now;
        // This attempt sits behind every failed and every in-flight attempt.
        let position = entry.fails.saturating_add(entry.in_flight);
        entry.in_flight = entry.in_flight.saturating_add(1);
        AttemptDecision::Wait(delay_for_position(position))
    }

    /// Settles an attempt previously reserved with [`Self::begin_attempt`].
    pub(super) fn settle(&self, share_id: &str, outcome: AttemptOutcome) {
        self.settle_at(share_id, outcome, Instant::now());
    }

    fn settle_at(&self, share_id: &str, outcome: AttemptOutcome, now: Instant) {
        let mut entries = self.entries.lock().expect("backoff mutex poisoned");
        match outcome {
            AttemptOutcome::Success => {
                entries.remove(share_id);
            }
            AttemptOutcome::AuthFailure => {
                if let Some(entry) = entries.get_mut(share_id) {
                    entry.in_flight = entry.in_flight.saturating_sub(1);
                    entry.fails = entry.fails.saturating_add(1);
                    entry.lock_window_fails = entry.lock_window_fails.saturating_add(1);
                    entry.lifetime_fails = entry.lifetime_fails.saturating_add(1);
                    if entry.lifetime_fails >= LIFETIME_FAIL_CAP {
                        entry.locked_until = None;
                    } else if entry.lock_window_fails >= LOCK_FAIL_CAP {
                        entry.locked_until =
                            Some(now + lock_duration_for_failures(entry.lifetime_fails));
                    }
                    entry.last_attempt_at = now;
                }
            }
            AttemptOutcome::Other => {
                if let Some(entry) = entries.get_mut(share_id) {
                    entry.in_flight = entry.in_flight.saturating_sub(1);
                    if entry.fails == 0 && entry.in_flight == 0 && entry.lifetime_fails == 0 {
                        entries.remove(share_id);
                    }
                }
            }
        }
    }
}

fn delay_for_position(position: u32) -> Duration {
    if position == 0 {
        return Duration::ZERO;
    }
    let shift = position.saturating_sub(1).min(MAX_SHIFT);
    let multiplier = 1_u32.checked_shl(shift).unwrap_or(1);
    INITIAL_DELAY.saturating_mul(multiplier).min(MAX_DELAY)
}

fn lock_duration_for_failures(failures: u32) -> Duration {
    let completed_windows = failures.saturating_sub(1) / LOCK_FAIL_CAP;
    let shift = completed_windows.min(MAX_LOCK_SHIFT);
    let multiplier = 1_u32.checked_shl(shift).unwrap_or(u32::MAX);
    INITIAL_LOCK_DURATION
        .saturating_mul(multiplier)
        .min(MAX_LOCK_DURATION)
}

fn retain_recent_entries(entries: &mut HashMap<String, BackoffEntry>, now: Instant) {
    entries.retain(|_, entry| {
        entry.unlock_if_expired(now);
        entry.locked(now)
            || entry.in_flight > 0
            || entry.lifetime_fails > 0
            || now.duration_since(entry.last_attempt_at) <= GC_AFTER
    });
}

fn evict_oldest_entry(entries: &mut HashMap<String, BackoffEntry>) {
    if let Some(oldest_key) = entries
        .iter()
        .min_by_key(|(_, entry)| entry.last_attempt_at)
        .map(|(share_id, _)| share_id.clone())
    {
        entries.remove(&oldest_key);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        lock_duration_for_failures, AttemptDecision, AttemptOutcome, AttemptReservation,
        AuthBackoff, GC_AFTER, INITIAL_LOCK_DURATION, LIFETIME_FAIL_CAP, LOCK_FAIL_CAP,
        MAX_BACKOFF_ENTRIES, MAX_LOCK_DURATION,
    };
    use std::time::Duration;

    impl AuthBackoff {
        /// Test helper: reserve an attempt that must not be locked, returning the
        /// wait. Panics if the share is locked so lock regressions surface loudly.
        fn begin_wait(&self, share_id: &str) -> Duration {
            match self.begin_attempt(share_id) {
                AttemptDecision::Wait(delay) => delay,
                AttemptDecision::Locked => panic!("share unexpectedly locked"),
            }
        }
    }

    #[test]
    fn no_delay_on_first_attempt() {
        let backoff = AuthBackoff::new();
        assert_eq!(backoff.begin_wait("share1"), Duration::ZERO);
    }

    #[test]
    fn delay_escalates_after_failures() {
        let backoff = AuthBackoff::new();
        assert_eq!(backoff.begin_wait("share1"), Duration::ZERO);
        backoff.settle("share1", AttemptOutcome::AuthFailure);
        assert_eq!(backoff.begin_wait("share1"), Duration::from_millis(100));
        backoff.settle("share1", AttemptOutcome::AuthFailure);
        assert_eq!(backoff.begin_wait("share1"), Duration::from_millis(200));
        backoff.settle("share1", AttemptOutcome::AuthFailure);
    }

    #[test]
    fn concurrent_in_flight_guesses_are_throttled() {
        // Regression test for the round-batching weakness: without recording a
        // single failure, three *concurrent* (unsettled) attempts must see an
        // escalating wait, so a burst cannot bypass the limiter.
        let backoff = AuthBackoff::new();
        assert_eq!(backoff.begin_wait("share1"), Duration::ZERO);
        assert_eq!(backoff.begin_wait("share1"), Duration::from_millis(100));
        assert_eq!(backoff.begin_wait("share1"), Duration::from_millis(200));
    }

    #[test]
    fn dropped_reserved_attempt_releases_in_flight_without_escalating() {
        let backoff = AuthBackoff::new();
        let AttemptReservation::Wait { delay, guard } = backoff.reserve_attempt("share1") else {
            panic!("first attempt must reserve");
        };
        assert_eq!(delay, Duration::ZERO);

        drop(guard);

        let entries = backoff.entries.lock().expect("backoff mutex poisoned");
        assert!(
            !entries.contains_key("share1"),
            "dropping the guard must settle Other and remove an empty entry"
        );
        drop(entries);

        assert_eq!(backoff.begin_wait("share1"), Duration::ZERO);
        backoff.settle("share1", AttemptOutcome::Other);
    }

    #[test]
    fn non_auth_outcome_does_not_escalate() {
        // A full or misconfigured share (capacity / missing PIN) must not lock
        // out legitimate viewers.
        let backoff = AuthBackoff::new();
        assert_eq!(backoff.begin_wait("share1"), Duration::ZERO);
        backoff.settle("share1", AttemptOutcome::Other);
        assert_eq!(backoff.begin_wait("share1"), Duration::ZERO);
        backoff.settle("share1", AttemptOutcome::Other);
    }

    #[test]
    fn delay_caps_at_10_seconds() {
        let backoff = AuthBackoff::new();
        for _ in 0..20 {
            backoff.begin_wait("share1");
            backoff.settle("share1", AttemptOutcome::AuthFailure);
        }
        assert_eq!(backoff.begin_wait("share1"), Duration::from_secs(10));
    }

    #[test]
    fn success_resets_backoff() {
        let backoff = AuthBackoff::new();
        backoff.begin_wait("share1");
        backoff.settle("share1", AttemptOutcome::AuthFailure);
        backoff.begin_wait("share1");
        backoff.settle("share1", AttemptOutcome::Success);
        assert_eq!(backoff.begin_wait("share1"), Duration::ZERO);
    }

    #[test]
    fn backoff_is_per_share_isolated() {
        let backoff = AuthBackoff::new();
        for _ in 0..5 {
            backoff.begin_wait("victim");
            backoff.settle("victim", AttemptOutcome::AuthFailure);
        }
        assert!(backoff.begin_wait("victim") > Duration::from_millis(500));
        assert_eq!(backoff.begin_wait("innocent"), Duration::ZERO);
    }

    #[test]
    fn unknown_share_id_still_escalates() {
        let backoff = AuthBackoff::new();
        backoff.begin_wait("nonexistent");
        backoff.settle("nonexistent", AttemptOutcome::AuthFailure);
        backoff.begin_wait("nonexistent");
        backoff.settle("nonexistent", AttemptOutcome::AuthFailure);
        assert!(backoff.begin_wait("nonexistent") > Duration::from_millis(50));
    }

    #[test]
    fn failure_table_evicts_old_entries_at_capacity() {
        let backoff = AuthBackoff::new();
        for index in 0..(MAX_BACKOFF_ENTRIES + 128) {
            let key = format!("share-{index}");
            backoff.begin_wait(&key);
            backoff.settle(&key, AttemptOutcome::AuthFailure);
        }

        let entries = backoff.entries.lock().expect("backoff mutex poisoned");
        assert!(entries.len() <= MAX_BACKOFF_ENTRIES);
        assert!(!entries.contains_key("share-0"));
        assert!(entries.contains_key(&format!("share-{}", MAX_BACKOFF_ENTRIES + 127)));
    }

    fn fail_n(backoff: &AuthBackoff, share_id: &str, n: u32) {
        let now = std::time::Instant::now();
        fail_n_at(backoff, share_id, n, now);
    }

    fn fail_n_at(backoff: &AuthBackoff, share_id: &str, n: u32, now: std::time::Instant) {
        for _ in 0..n {
            // Drive failures directly so an escalating delay never blocks the test;
            // begin_wait would still return instantly here but settle is what counts.
            backoff.begin_attempt_at(share_id, now);
            backoff.settle_at(share_id, AttemptOutcome::AuthFailure, now);
        }
    }

    #[test]
    fn failures_eventually_lock_the_share_temporarily() {
        let backoff = AuthBackoff::new();
        fail_n(&backoff, "share1", LOCK_FAIL_CAP);
        assert_eq!(
            backoff.begin_attempt("share1"),
            AttemptDecision::Locked,
            "the share must fail closed once the lock budget is spent"
        );
    }

    #[test]
    fn temporary_lock_expires_without_success() {
        let backoff = AuthBackoff::new();
        let now = std::time::Instant::now();
        fail_n_at(&backoff, "share1", LOCK_FAIL_CAP, now);
        let lock_duration = lock_duration_for_failures(LOCK_FAIL_CAP);
        let before_unlock = now + lock_duration - Duration::from_millis(1);
        assert_eq!(
            backoff.begin_attempt_at("share1", before_unlock),
            AttemptDecision::Locked
        );
        let after_unlock = now + lock_duration + Duration::from_millis(1);
        assert_eq!(
            backoff.begin_attempt_at("share1", after_unlock),
            AttemptDecision::Wait(Duration::ZERO)
        );
    }

    #[test]
    fn locked_attempts_do_not_extend_the_temporary_lock() {
        let backoff = AuthBackoff::new();
        let now = std::time::Instant::now();
        fail_n_at(&backoff, "share1", LOCK_FAIL_CAP, now);
        let lock_duration = lock_duration_for_failures(LOCK_FAIL_CAP);
        assert_eq!(
            backoff.begin_attempt_at("share1", now + Duration::from_secs(1)),
            AttemptDecision::Locked
        );
        assert_eq!(
            backoff.begin_attempt_at("share1", now + lock_duration + Duration::from_millis(1)),
            AttemptDecision::Wait(Duration::ZERO)
        );
    }

    #[test]
    fn repeated_lock_windows_escalate_duration() {
        let backoff = AuthBackoff::new();
        let now = std::time::Instant::now();
        fail_n_at(&backoff, "share1", LOCK_FAIL_CAP, now);
        let first_lock = lock_duration_for_failures(LOCK_FAIL_CAP);
        assert_eq!(first_lock, INITIAL_LOCK_DURATION);

        let second_window = now + first_lock + Duration::from_millis(1);
        fail_n_at(&backoff, "share1", LOCK_FAIL_CAP, second_window);
        let second_lock = lock_duration_for_failures(LOCK_FAIL_CAP * 2);
        assert_eq!(second_lock, INITIAL_LOCK_DURATION * 2);

        assert_eq!(
            backoff.begin_attempt_at(
                "share1",
                second_window + second_lock - Duration::from_millis(1)
            ),
            AttemptDecision::Locked
        );
        assert_eq!(
            backoff.begin_attempt_at(
                "share1",
                second_window + second_lock + Duration::from_millis(1)
            ),
            AttemptDecision::Wait(Duration::ZERO)
        );
    }

    #[test]
    fn cumulative_failures_eventually_fail_closed() {
        let backoff = AuthBackoff::new();
        let mut now = std::time::Instant::now();
        let windows = LIFETIME_FAIL_CAP / LOCK_FAIL_CAP;

        for window in 0..windows {
            fail_n_at(&backoff, "share1", LOCK_FAIL_CAP, now);
            if window + 1 < windows {
                let failures = (window + 1) * LOCK_FAIL_CAP;
                now += lock_duration_for_failures(failures) + Duration::from_millis(1);
            }
        }

        assert_eq!(
            backoff.begin_attempt_at("share1", now + MAX_LOCK_DURATION * 2),
            AttemptDecision::Locked,
            "high cumulative failure budget must still fail closed"
        );
    }

    #[test]
    fn success_before_lock_clears_lock_window_failures() {
        let backoff = AuthBackoff::new();
        fail_n(&backoff, "share1", LOCK_FAIL_CAP - 1);
        backoff.begin_wait("share1");
        backoff.settle("share1", AttemptOutcome::Success);
        // Success removes the entry, so the failure counter starts fresh and the
        // share is not one failure away from another lock window.
        assert_eq!(backoff.begin_wait("share1"), Duration::ZERO);
        fail_n(&backoff, "share1", LOCK_FAIL_CAP - 1);
        assert!(matches!(
            backoff.begin_attempt("share1"),
            AttemptDecision::Wait(_)
        ));
    }

    #[test]
    fn locked_attempt_reserves_no_in_flight() {
        let backoff = AuthBackoff::new();
        fail_n(&backoff, "share1", LOCK_FAIL_CAP);
        let before = backoff
            .entries
            .lock()
            .expect("backoff mutex poisoned")
            .get("share1")
            .map(|entry| entry.in_flight)
            .expect("entry exists");
        assert_eq!(backoff.begin_attempt("share1"), AttemptDecision::Locked);
        let after = backoff
            .entries
            .lock()
            .expect("backoff mutex poisoned")
            .get("share1")
            .map(|entry| entry.in_flight)
            .expect("entry exists");
        assert_eq!(
            before, after,
            "a locked attempt must not reserve an in-flight slot"
        );
    }

    #[test]
    fn idle_gc_preserves_cumulative_failures_after_lock_expiry() {
        let backoff = AuthBackoff::new();
        let now = std::time::Instant::now();
        fail_n_at(&backoff, "share1", LOCK_FAIL_CAP, now);
        let after_first_lock =
            now + lock_duration_for_failures(LOCK_FAIL_CAP) + GC_AFTER + Duration::from_secs(1);

        // Drive GC through unrelated activity. The expired lock should be
        // cleared, but the cumulative failure budget must survive.
        assert_eq!(
            backoff.begin_attempt_at("other", after_first_lock),
            AttemptDecision::Wait(Duration::ZERO)
        );
        backoff.settle_at("other", AttemptOutcome::Other, after_first_lock);
        assert_eq!(
            backoff.begin_attempt_at("other2", after_first_lock + Duration::from_millis(1)),
            AttemptDecision::Wait(Duration::ZERO)
        );
        backoff.settle_at(
            "other2",
            AttemptOutcome::Other,
            after_first_lock + Duration::from_millis(1),
        );

        let lifetime_fails = backoff
            .entries
            .lock()
            .expect("backoff mutex poisoned")
            .get("share1")
            .map(|entry| entry.lifetime_fails)
            .expect("share backoff entry must survive idle GC");
        assert_eq!(lifetime_fails, LOCK_FAIL_CAP);

        fail_n_at(&backoff, "share1", LOCK_FAIL_CAP, after_first_lock);
        let second_lock = lock_duration_for_failures(LOCK_FAIL_CAP * 2);
        assert_eq!(second_lock, INITIAL_LOCK_DURATION * 2);
        assert_eq!(
            backoff.begin_attempt_at(
                "share1",
                after_first_lock + second_lock - Duration::from_millis(1)
            ),
            AttemptDecision::Locked
        );
    }
}
