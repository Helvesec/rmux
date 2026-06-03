use std::time::{Duration, Instant};

const OPERATOR_RATE_LIMIT: u16 = 200;

pub(super) struct OperatorRateLimiter {
    remaining: u16,
    window_started: Instant,
}

impl OperatorRateLimiter {
    pub(super) fn new() -> Self {
        Self {
            remaining: OPERATOR_RATE_LIMIT,
            window_started: Instant::now(),
        }
    }

    pub(super) fn try_acquire(&mut self) -> bool {
        if self.window_started.elapsed() >= Duration::from_secs(1) {
            self.remaining = OPERATOR_RATE_LIMIT;
            self.window_started = Instant::now();
        }
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{OperatorRateLimiter, OPERATOR_RATE_LIMIT};
    use std::time::{Duration, Instant};

    impl OperatorRateLimiter {
        /// Test seam: start the window in the past so refill can be exercised
        /// without sleeping a real second.
        fn with_window_start(start: Instant) -> Self {
            Self {
                remaining: OPERATOR_RATE_LIMIT,
                window_started: start,
            }
        }
    }

    #[test]
    fn allows_exactly_the_budget_then_fails_closed() {
        let mut limiter = OperatorRateLimiter::new();
        for _ in 0..OPERATOR_RATE_LIMIT {
            assert!(limiter.try_acquire(), "budget frames must be admitted");
        }
        assert!(!limiter.try_acquire(), "frames past the budget are dropped");
        assert!(
            !limiter.try_acquire(),
            "and stay dropped within the same window"
        );
    }

    #[test]
    fn refills_after_the_window_elapses() {
        let mut limiter = OperatorRateLimiter::with_window_start(
            Instant::now()
                .checked_sub(Duration::from_secs(2))
                .expect("clock supports offset"),
        );
        assert!(limiter.try_acquire(), "window refills after >= 1s");
        for _ in 1..OPERATOR_RATE_LIMIT {
            assert!(limiter.try_acquire());
        }
        assert!(
            !limiter.try_acquire(),
            "the refilled budget is the same bound"
        );
    }
}
