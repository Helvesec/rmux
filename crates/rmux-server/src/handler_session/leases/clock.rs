use std::time::{Duration, Instant};

pub(super) const REAPER_INTERVAL: Duration = Duration::from_millis(100);
const REAPER_STALL_THRESHOLD: Duration =
    Duration::from_millis(rmux_proto::MIN_SESSION_LEASE_TTL_MILLIS);

#[derive(Debug, Clone, Copy)]
pub(super) struct ReaperWake {
    expected_at: Instant,
    observed_at: Instant,
    scheduler_stalled: bool,
}

impl ReaperWake {
    pub(super) const fn observed_at(self) -> Instant {
        self.observed_at
    }
}

#[derive(Debug)]
pub(super) struct ReaperSchedule {
    previous_wake: Instant,
}

impl ReaperSchedule {
    pub(super) const fn new(now: Instant) -> Self {
        Self { previous_wake: now }
    }

    pub(super) fn observe_wake(&mut self, now: Instant) -> ReaperWake {
        let elapsed = now.saturating_duration_since(self.previous_wake);
        let expected_at = self
            .previous_wake
            .checked_add(REAPER_INTERVAL)
            .unwrap_or(now);
        self.previous_wake = now;
        ReaperWake {
            expected_at,
            observed_at: now,
            scheduler_stalled: elapsed >= REAPER_STALL_THRESHOLD,
        }
    }
}

#[derive(Debug)]
pub(super) struct LeaseDeadline {
    renewed_at: Instant,
    expires_at: Instant,
}

impl LeaseDeadline {
    pub(super) fn from_now(ttl: Duration) -> Option<Self> {
        let now = Instant::now();
        Some(Self {
            renewed_at: now,
            expires_at: now.checked_add(ttl)?,
        })
    }

    pub(super) fn renew_from_now(&mut self, ttl: Duration) -> bool {
        let now = Instant::now();
        let Some(expires_at) = now.checked_add(ttl) else {
            return false;
        };
        self.renewed_at = now;
        self.expires_at = expires_at;
        true
    }

    pub(super) fn preserve_budget_across_reaper_pause(&mut self, wake: ReaperWake) {
        if !wake.scheduler_stalled
            || wake.observed_at <= wake.expected_at
            || self.renewed_at >= wake.expected_at
            || self.expires_at <= wake.expected_at
        {
            return;
        }
        let remaining = self.expires_at.duration_since(wake.expected_at);
        if let Some(deadline) = wake.observed_at.checked_add(remaining) {
            self.expires_at = deadline;
        }
    }

    pub(super) fn is_expired_at(&self, now: Instant) -> bool {
        self.expires_at <= now
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_pause_preserves_only_the_budget_remaining_at_the_expected_wake() {
        let start = Instant::now();
        let mut schedule = ReaperSchedule::new(start);
        let mut live = LeaseDeadline {
            renewed_at: start,
            expires_at: start + Duration::from_secs(5),
        };
        let mut already_due = LeaseDeadline {
            renewed_at: start,
            expires_at: start + REAPER_INTERVAL,
        };
        let resumed_at = start + Duration::from_secs(30);
        let wake = schedule.observe_wake(resumed_at);

        live.preserve_budget_across_reaper_pause(wake);
        already_due.preserve_budget_across_reaper_pause(wake);

        assert!(!live.is_expired_at(resumed_at));
        assert!(live.is_expired_at(resumed_at + Duration::from_millis(4_900)));
        assert!(already_due.is_expired_at(resumed_at));
    }

    #[test]
    fn elapsed_owner_deadline_still_expires_when_the_reaper_keeps_running() {
        let start = Instant::now();
        let deadline = LeaseDeadline {
            renewed_at: start,
            expires_at: start + Duration::from_millis(500),
        };

        assert!(deadline.is_expired_at(start + Duration::from_millis(500)));
    }

    #[test]
    fn repeated_timer_jitter_does_not_extend_the_lease_deadline() {
        let start = Instant::now();
        let mut schedule = ReaperSchedule::new(start);
        let mut deadline = LeaseDeadline {
            renewed_at: start,
            expires_at: start + Duration::from_millis(500),
        };
        let mut observed_at = start;

        for _ in 0..5 {
            observed_at += Duration::from_millis(110);
            deadline.preserve_budget_across_reaper_pause(schedule.observe_wake(observed_at));
        }

        assert!(deadline.is_expired_at(observed_at));
    }

    #[test]
    fn renewal_processed_after_resume_is_not_shifted_a_second_time() {
        let start = Instant::now();
        let mut schedule = ReaperSchedule::new(start);
        let resumed_at = start + Duration::from_secs(30);
        let wake = schedule.observe_wake(resumed_at);
        let mut renewed = LeaseDeadline {
            renewed_at: resumed_at,
            expires_at: resumed_at + Duration::from_secs(5),
        };

        renewed.preserve_budget_across_reaper_pause(wake);

        assert!(renewed.is_expired_at(resumed_at + Duration::from_secs(5)));
    }
}
