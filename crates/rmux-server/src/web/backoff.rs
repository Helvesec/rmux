use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const INITIAL_DELAY: Duration = Duration::from_millis(100);
const MAX_DELAY: Duration = Duration::from_secs(10);
const RESET_AFTER: Duration = Duration::from_secs(5 * 60);
const GC_AFTER: Duration = Duration::from_secs(10 * 60);
const MAX_BACKOFF_ENTRIES: usize = 4096;

#[derive(Debug, Default)]
pub(super) struct AuthBackoff {
    entries: Mutex<HashMap<String, BackoffEntry>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AuthBackoffFailure {
    pub(super) fails: u32,
    pub(super) next_delay: Duration,
}

#[derive(Debug)]
struct BackoffEntry {
    fails: u32,
    last_attempt_at: Instant,
    blocked_until: Instant,
}

impl AuthBackoff {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn delay_before_next_attempt(&self, share_id: &str) -> Duration {
        let now = Instant::now();
        let mut entries = self.entries.lock().expect("backoff mutex poisoned");
        retain_recent_entries(&mut entries, now);

        let Some(entry) = entries.get(share_id) else {
            return Duration::ZERO;
        };
        if now.duration_since(entry.last_attempt_at) >= RESET_AFTER {
            entries.remove(share_id);
            return Duration::ZERO;
        }
        entry.blocked_until.saturating_duration_since(now)
    }

    pub(super) fn record_success(&self, share_id: &str) {
        self.entries
            .lock()
            .expect("backoff mutex poisoned")
            .remove(share_id);
    }

    pub(super) fn record_failure(&self, share_id: &str) -> AuthBackoffFailure {
        let now = Instant::now();
        let mut entries = self.entries.lock().expect("backoff mutex poisoned");
        retain_recent_entries(&mut entries, now);
        if !entries.contains_key(share_id) && entries.len() >= MAX_BACKOFF_ENTRIES {
            evict_oldest_entry(&mut entries);
        }
        let entry = entries
            .entry(share_id.to_owned())
            .or_insert_with(|| BackoffEntry {
                fails: 0,
                last_attempt_at: now,
                blocked_until: now,
            });
        entry.fails = entry.fails.saturating_add(1);
        entry.last_attempt_at = now;
        let shift = entry.fails.saturating_sub(1).min(7);
        let multiplier = 1_u32.checked_shl(shift).unwrap_or(1);
        let next_delay = INITIAL_DELAY.saturating_mul(multiplier).min(MAX_DELAY);
        entry.blocked_until = now + next_delay;
        AuthBackoffFailure {
            fails: entry.fails,
            next_delay,
        }
    }
}

fn retain_recent_entries(entries: &mut HashMap<String, BackoffEntry>, now: Instant) {
    entries.retain(|_, entry| now.duration_since(entry.last_attempt_at) <= GC_AFTER);
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
    use super::{AuthBackoff, MAX_BACKOFF_ENTRIES};
    use std::time::Duration;

    #[test]
    fn no_delay_on_first_attempt() {
        let backoff = AuthBackoff::new();
        assert_eq!(backoff.delay_before_next_attempt("share1"), Duration::ZERO);
    }

    #[test]
    fn delay_doubles_after_failures() {
        let backoff = AuthBackoff::new();
        backoff.record_failure("share1");
        assert!(backoff.delay_before_next_attempt("share1") <= Duration::from_millis(100));
        backoff.record_failure("share1");
        assert!(backoff.delay_before_next_attempt("share1") >= Duration::from_millis(150));
    }

    #[test]
    fn delay_caps_at_10_seconds() {
        let backoff = AuthBackoff::new();
        for _ in 0..20 {
            backoff.record_failure("share1");
        }
        let delay = backoff.delay_before_next_attempt("share1");
        assert!(delay <= Duration::from_secs(10));
        assert!(delay >= Duration::from_secs(9));
    }

    #[test]
    fn success_resets_backoff() {
        let backoff = AuthBackoff::new();
        backoff.record_failure("share1");
        backoff.record_failure("share1");
        backoff.record_success("share1");
        assert_eq!(backoff.delay_before_next_attempt("share1"), Duration::ZERO);
    }

    #[test]
    fn backoff_is_per_share_isolated() {
        let backoff = AuthBackoff::new();
        for _ in 0..5 {
            backoff.record_failure("victim");
        }
        assert!(backoff.delay_before_next_attempt("victim") > Duration::from_millis(500));
        assert_eq!(
            backoff.delay_before_next_attempt("innocent"),
            Duration::ZERO
        );
    }

    #[test]
    fn unknown_share_id_still_records_failure() {
        let backoff = AuthBackoff::new();
        backoff.record_failure("nonexistent");
        backoff.record_failure("nonexistent");
        assert!(backoff.delay_before_next_attempt("nonexistent") > Duration::from_millis(50));
    }

    #[test]
    fn failure_table_evicts_old_entries_at_capacity() {
        let backoff = AuthBackoff::new();
        for index in 0..(MAX_BACKOFF_ENTRIES + 128) {
            backoff.record_failure(&format!("share-{index}"));
        }

        let entries = backoff.entries.lock().expect("backoff mutex poisoned");
        assert!(entries.len() <= MAX_BACKOFF_ENTRIES);
        assert!(!entries.contains_key("share-0"));
        assert!(entries.contains_key(&format!("share-{}", MAX_BACKOFF_ENTRIES + 127)));
    }
}
