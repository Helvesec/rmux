use std::time::{Duration, Instant};

/// Shared startup deadline for connect-or-start critical sections.
///
/// `None` and `Duration::MAX` intentionally mean no deadline. Very large
/// finite values that overflow `Instant` are treated the same way rather than
/// panicking inside the startup path.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StartupDeadline {
    started_at: Instant,
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    requested: Option<Duration>,
    expires_at: Option<Instant>,
}

impl StartupDeadline {
    pub(crate) fn from_timeout(timeout: Option<Duration>) -> Self {
        Self::from_timeout_at(timeout, Instant::now())
    }

    pub(crate) fn from_timeout_at(timeout: Option<Duration>, started_at: Instant) -> Self {
        let requested = match timeout {
            Some(Duration::MAX) | None => None,
            Some(timeout) => Some(timeout),
        };
        let expires_at = requested.and_then(|timeout| started_at.checked_add(timeout));
        Self {
            started_at,
            requested,
            expires_at,
        }
    }

    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    pub(crate) fn requested_timeout(self) -> Option<Duration> {
        self.requested
    }

    pub(crate) fn elapsed(self) -> Duration {
        self.elapsed_at(Instant::now())
    }

    pub(crate) fn elapsed_at(self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started_at)
    }

    pub(crate) fn is_elapsed(self) -> bool {
        self.is_elapsed_at(Instant::now())
    }

    pub(crate) fn is_elapsed_at(self, now: Instant) -> bool {
        self.expires_at.is_some_and(|expires_at| now >= expires_at)
    }

    pub(crate) fn sleep_for(self, poll_interval: Duration) -> Duration {
        self.sleep_for_at(Instant::now(), poll_interval)
    }

    pub(crate) fn sleep_for_at(self, now: Instant, poll_interval: Duration) -> Duration {
        self.expires_at
            .map(|expires_at| poll_interval.min(expires_at.saturating_duration_since(now)))
            .unwrap_or(poll_interval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_max_is_unbounded() {
        let now = Instant::now();
        let deadline = StartupDeadline::from_timeout_at(Some(Duration::MAX), now);
        assert_eq!(deadline.requested_timeout(), None);
        assert!(!deadline.is_elapsed_at(now + Duration::from_secs(1)));
        assert_eq!(
            deadline.sleep_for_at(now + Duration::from_secs(1), Duration::from_millis(5)),
            Duration::from_millis(5)
        );
    }

    #[test]
    fn finite_deadline_reports_elapsed_and_caps_sleep() {
        let now = Instant::now();
        let deadline = StartupDeadline::from_timeout_at(Some(Duration::from_millis(10)), now);

        assert_eq!(
            deadline.requested_timeout(),
            Some(Duration::from_millis(10))
        );
        assert!(!deadline.is_elapsed_at(now + Duration::from_millis(9)));
        assert!(deadline.is_elapsed_at(now + Duration::from_millis(10)));
        assert_eq!(
            deadline.sleep_for_at(now + Duration::from_millis(8), Duration::from_millis(5)),
            Duration::from_millis(2)
        );
        assert_eq!(
            deadline.elapsed_at(now + Duration::from_millis(12)),
            Duration::from_millis(12)
        );
    }
}
