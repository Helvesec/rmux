use std::{future::pending, time::Duration};

use tokio::time::Instant;

const ATTACH_REFRESH_COALESCE: Duration = Duration::from_millis(16);

pub(super) async fn wait_for_refresh_deadline(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        pending::<()>().await;
    }
}

#[derive(Debug, Clone)]
pub(super) struct AttachRefreshScheduler {
    deadline: Option<Instant>,
    interval: Duration,
}

impl Default for AttachRefreshScheduler {
    fn default() -> Self {
        Self {
            deadline: None,
            interval: ATTACH_REFRESH_COALESCE,
        }
    }
}

impl AttachRefreshScheduler {
    pub(super) fn schedule_now(&mut self) {
        self.schedule(Instant::now());
    }

    fn schedule(&mut self, now: Instant) {
        if self.deadline.is_none() {
            self.deadline = Some(now + self.interval);
        }
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(super) fn is_pending(&self) -> bool {
        self.deadline.is_some()
    }

    pub(super) fn clear(&mut self) {
        self.deadline = None;
    }
}

#[cfg(test)]
mod tests {
    use tokio::time::Instant;

    use super::AttachRefreshScheduler;

    #[test]
    fn schedule_keeps_the_first_deadline_until_cleared() {
        let mut scheduler = AttachRefreshScheduler::default();
        let first = Instant::now();
        let second = first + scheduler.interval + scheduler.interval;

        scheduler.schedule(first);
        let first_deadline = scheduler.deadline().expect("scheduled deadline");
        scheduler.schedule(second);

        assert_eq!(scheduler.deadline(), Some(first_deadline));
        assert!(scheduler.is_pending());
        scheduler.clear();
        assert!(!scheduler.is_pending());
        scheduler.schedule(second);
        assert_ne!(scheduler.deadline(), Some(first_deadline));
    }
}
