use std::{future::pending, time::Duration};

use tokio::time::Instant;

const ATTACH_REFRESH_COALESCE: Duration = Duration::from_millis(2);
const ATTACH_SUSTAINED_REFRESH_COALESCE: Duration = Duration::from_millis(100);
const ATTACH_SUSTAINED_OUTPUT_MAX_GAP: Duration = Duration::from_secs(3);
const ATTACH_SUSTAINED_OUTPUT_MIN_BATCHES: u8 = 3;
const ATTACH_SUSTAINED_OUTPUT_MIN_DURATION: Duration = Duration::from_millis(125);

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
    output_burst_started_at: Option<Instant>,
    last_output_at: Option<Instant>,
    output_burst_batches: u8,
}

#[derive(Debug, Clone)]
pub(super) struct AttachStatusRefreshScheduler {
    deadline: Option<Instant>,
}

impl Default for AttachRefreshScheduler {
    fn default() -> Self {
        Self {
            deadline: None,
            interval: ATTACH_REFRESH_COALESCE,
            output_burst_started_at: None,
            last_output_at: None,
            output_burst_batches: 0,
        }
    }
}

impl AttachRefreshScheduler {
    pub(super) fn schedule_now(&mut self) {
        let deadline = Instant::now() + self.interval;
        if self.deadline.is_none_or(|current| deadline < current) {
            self.deadline = Some(deadline);
        }
    }

    pub(super) fn schedule_immediate(&mut self) {
        self.deadline = Some(Instant::now());
    }

    pub(super) fn schedule_sustained(&mut self) {
        self.note_sustained_output();
        self.schedule(Instant::now(), ATTACH_SUSTAINED_REFRESH_COALESCE);
    }

    pub(super) fn note_output_batch(&mut self, batch_sustained: bool) -> bool {
        self.note_output_batch_at(Instant::now(), batch_sustained)
    }

    fn schedule(&mut self, now: Instant, interval: Duration) {
        if self.deadline.is_none() {
            self.deadline = Some(now + interval);
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

    pub(super) fn note_sustained_output(&mut self) {
        self.interval = ATTACH_SUSTAINED_REFRESH_COALESCE;
    }

    pub(super) fn is_sustained(&self) -> bool {
        self.interval == ATTACH_SUSTAINED_REFRESH_COALESCE
    }

    pub(super) fn can_bypass_small_plain_output(&self) -> bool {
        self.can_bypass_small_plain_output_at(Instant::now())
    }

    fn can_bypass_small_plain_output_at(&self, now: Instant) -> bool {
        if self.is_sustained() {
            return false;
        }
        let continues_burst = self.last_output_at.is_some_and(|last| {
            now.saturating_duration_since(last) <= ATTACH_SUSTAINED_OUTPUT_MAX_GAP
        });
        !continues_burst || self.output_burst_batches < ATTACH_SUSTAINED_OUTPUT_MIN_BATCHES
    }

    pub(super) fn note_interactive_output(&mut self) {
        self.interval = ATTACH_REFRESH_COALESCE;
    }

    fn note_output_batch_at(&mut self, now: Instant, batch_sustained: bool) -> bool {
        if batch_sustained {
            self.last_output_at = Some(now);
            self.output_burst_batches = ATTACH_SUSTAINED_OUTPUT_MIN_BATCHES;
            self.note_sustained_output();
            return true;
        }

        let continues_burst = self.last_output_at.is_some_and(|last| {
            now.saturating_duration_since(last) <= ATTACH_SUSTAINED_OUTPUT_MAX_GAP
        });
        self.last_output_at = Some(now);
        if continues_burst {
            self.output_burst_batches = self
                .output_burst_batches
                .saturating_add(1)
                .min(ATTACH_SUSTAINED_OUTPUT_MIN_BATCHES);
        } else {
            self.output_burst_started_at = Some(now);
            self.output_burst_batches = 1;
        }

        if self.output_burst_batches >= ATTACH_SUSTAINED_OUTPUT_MIN_BATCHES
            && self.output_burst_started_at.is_some_and(|started_at| {
                now.saturating_duration_since(started_at) >= ATTACH_SUSTAINED_OUTPUT_MIN_DURATION
            })
        {
            self.note_sustained_output();
            true
        } else {
            self.note_interactive_output();
            false
        }
    }
}

impl AttachStatusRefreshScheduler {
    pub(super) fn new(interval: Option<Duration>) -> Self {
        let mut scheduler = Self { deadline: None };
        scheduler.reschedule(interval);
        scheduler
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(super) fn reschedule(&mut self, interval: Option<Duration>) {
        self.deadline = interval.map(|interval| Instant::now() + interval);
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
        let second = first + std::time::Duration::from_millis(1);

        scheduler.schedule(first, scheduler.interval);
        let first_deadline = scheduler.deadline().expect("scheduled deadline");
        scheduler.schedule(second, scheduler.interval);

        assert_eq!(scheduler.deadline(), Some(first_deadline));
        assert!(scheduler.is_pending());
        scheduler.clear();
        assert!(!scheduler.is_pending());
        scheduler.schedule(second, scheduler.interval);
        assert_ne!(scheduler.deadline(), Some(first_deadline));
    }

    #[test]
    fn sustained_schedule_uses_interactive_deadline_for_floods() {
        let mut scheduler = AttachRefreshScheduler::default();
        let before = Instant::now();

        scheduler.schedule_sustained();

        let deadline = scheduler.deadline().expect("scheduled deadline");
        assert!(deadline >= before + super::ATTACH_SUSTAINED_REFRESH_COALESCE);
        assert!(deadline <= before + super::ATTACH_SUSTAINED_REFRESH_COALESCE * 2);
    }

    #[test]
    fn immediate_schedule_uses_ready_deadline() {
        let mut scheduler = AttachRefreshScheduler::default();
        let before = Instant::now();

        scheduler.schedule_immediate();

        let deadline = scheduler.deadline().expect("scheduled deadline");
        assert!(deadline >= before);
        assert!(deadline <= Instant::now());
    }

    #[test]
    fn immediate_schedule_pulls_in_existing_sustained_deadline() {
        let mut scheduler = AttachRefreshScheduler::default();
        scheduler.note_sustained_output();
        scheduler.schedule_now();
        let sustained_deadline = scheduler.deadline().expect("sustained deadline");

        scheduler.schedule_immediate();

        let immediate_deadline = scheduler.deadline().expect("immediate deadline");
        assert!(immediate_deadline < sustained_deadline);
        assert!(immediate_deadline <= Instant::now());
    }

    #[test]
    fn interactive_schedule_pulls_in_existing_sustained_deadline() {
        let mut scheduler = AttachRefreshScheduler::default();
        scheduler.note_sustained_output();
        scheduler.schedule_now();
        let sustained_deadline = scheduler.deadline().expect("sustained deadline");

        scheduler.note_interactive_output();
        scheduler.schedule_now();

        let interactive_deadline = scheduler.deadline().expect("interactive deadline");
        assert!(interactive_deadline < sustained_deadline);
        assert!(interactive_deadline <= Instant::now() + super::ATTACH_REFRESH_COALESCE * 2);
    }

    #[test]
    fn sustained_output_promotes_the_next_refresh_window() {
        let mut scheduler = AttachRefreshScheduler::default();
        let before = Instant::now();

        scheduler.note_sustained_output();
        scheduler.schedule_now();

        let deadline = scheduler.deadline().expect("scheduled deadline");
        assert!(deadline >= before + super::ATTACH_SUSTAINED_REFRESH_COALESCE);
        assert!(deadline <= before + super::ATTACH_SUSTAINED_REFRESH_COALESCE * 2);

        scheduler.clear();
        scheduler.note_interactive_output();
        scheduler.schedule_now();

        let interactive_deadline = scheduler.deadline().expect("scheduled deadline");
        assert!(interactive_deadline < Instant::now() + super::ATTACH_SUSTAINED_REFRESH_COALESCE);
    }

    #[test]
    fn small_regular_output_batches_promote_to_sustained() {
        let mut scheduler = AttachRefreshScheduler::default();
        let start = Instant::now();

        for index in 0..super::ATTACH_SUSTAINED_OUTPUT_MIN_BATCHES {
            let promoted = scheduler.note_output_batch_at(
                start + super::ATTACH_SUSTAINED_OUTPUT_MAX_GAP * u32::from(index),
                false,
            );
            assert_eq!(
                promoted,
                index + 1 == super::ATTACH_SUSTAINED_OUTPUT_MIN_BATCHES
            );
        }

        scheduler.clear();
        let before = Instant::now();
        scheduler.schedule_now();
        let deadline = scheduler.deadline().expect("scheduled deadline");
        assert!(deadline >= before + super::ATTACH_SUSTAINED_REFRESH_COALESCE);
        assert!(scheduler.is_sustained());
    }

    #[test]
    fn sustained_then_small_batch_can_return_to_interactive_interval() {
        let mut scheduler = AttachRefreshScheduler::default();
        let t0 = Instant::now();

        assert!(scheduler.note_output_batch_at(t0, true));
        assert!(
            scheduler.is_sustained(),
            "after big batch, interval should be SUSTAINED"
        );

        let t1 = t0 + std::time::Duration::from_millis(10);
        let returned = scheduler.note_output_batch_at(t1, false);
        let still_sustained = scheduler.is_sustained();

        assert!(
            !still_sustained,
            "small follow-up output should use the interactive interval"
        );
        assert!(
            !returned,
            "small follow-up output is not itself a sustained batch"
        );
    }

    #[test]
    fn small_plain_bypass_stops_after_short_recent_burst() {
        let mut scheduler = AttachRefreshScheduler::default();
        let start = Instant::now();
        let step = super::ATTACH_SUSTAINED_OUTPUT_MIN_DURATION / 4;

        assert!(scheduler.can_bypass_small_plain_output_at(start));
        assert!(!scheduler.note_output_batch_at(start, false));

        let second = start + step;
        assert!(scheduler.can_bypass_small_plain_output_at(second));
        assert!(!scheduler.note_output_batch_at(second, false));

        let third = second + step;
        assert!(scheduler.can_bypass_small_plain_output_at(third));
        assert!(!scheduler.note_output_batch_at(third, false));

        assert!(!scheduler.can_bypass_small_plain_output_at(third + step));
    }

    #[test]
    fn isolated_output_after_gap_returns_to_interactive() {
        let mut scheduler = AttachRefreshScheduler::default();
        let start = Instant::now();
        for index in 0..super::ATTACH_SUSTAINED_OUTPUT_MIN_BATCHES {
            let _ = scheduler.note_output_batch_at(
                start + super::ATTACH_SUSTAINED_OUTPUT_MAX_GAP * u32::from(index),
                false,
            );
        }
        assert!(scheduler.note_output_batch_at(
            start
                + super::ATTACH_SUSTAINED_OUTPUT_MAX_GAP
                    * u32::from(super::ATTACH_SUSTAINED_OUTPUT_MIN_BATCHES),
            false,
        ));

        assert!(!scheduler.note_output_batch_at(
            start
                + super::ATTACH_SUSTAINED_OUTPUT_MAX_GAP
                    * u32::from(super::ATTACH_SUSTAINED_OUTPUT_MIN_BATCHES + 3),
            false,
        ));
        scheduler.clear();
        scheduler.schedule_now();
        let deadline = scheduler.deadline().expect("scheduled deadline");
        assert!(deadline < Instant::now() + super::ATTACH_SUSTAINED_REFRESH_COALESCE);
    }
}
