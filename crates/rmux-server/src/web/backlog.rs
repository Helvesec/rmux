use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BacklogLimits {
    per_sink: usize,
    global: usize,
}

impl BacklogLimits {
    pub(crate) const fn new(per_sink: usize, global: usize) -> Self {
        Self { per_sink, global }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BacklogPressure {
    AlreadyResyncing,
    Global,
    PerSink,
}

#[derive(Debug, Clone)]
pub(crate) struct BacklogBudget {
    global: Arc<AtomicUsize>,
    limits: BacklogLimits,
}

impl BacklogBudget {
    pub(crate) fn new(limits: BacklogLimits) -> Self {
        Self {
            global: Arc::new(AtomicUsize::new(0)),
            limits,
        }
    }

    pub(crate) fn new_sink(&self) -> SinkBacklog {
        SinkBacklog {
            queued: Arc::new(AtomicUsize::new(0)),
            resync_pending: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn queued_global(&self) -> usize {
        self.global.load(Ordering::Acquire)
    }

    pub(crate) fn try_reserve(
        &self,
        sink: &SinkBacklog,
        bytes: usize,
    ) -> Result<BacklogReservation, BacklogPressure> {
        if sink.resync_pending() {
            return Err(BacklogPressure::AlreadyResyncing);
        }
        if !reserve_counter(&sink.queued, bytes, self.limits.per_sink) {
            return Err(BacklogPressure::PerSink);
        }
        if !reserve_counter(&self.global, bytes, self.limits.global) {
            sink.queued.fetch_sub(bytes, Ordering::AcqRel);
            return Err(BacklogPressure::Global);
        }
        Ok(BacklogReservation {
            queued: Arc::clone(&sink.queued),
            global: Arc::clone(&self.global),
            bytes,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SinkBacklog {
    queued: Arc<AtomicUsize>,
    resync_pending: Arc<AtomicBool>,
}

impl SinkBacklog {
    pub(crate) fn begin_resync(&self) -> Option<ResyncAttempt> {
        self.resync_pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ResyncAttempt {
                pending: Arc::clone(&self.resync_pending),
                committed: false,
            })
    }

    pub(crate) fn complete_resync(&self) {
        self.resync_pending.store(false, Ordering::Release);
    }

    pub(crate) fn queued(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }

    pub(crate) fn resync_pending(&self) -> bool {
        self.resync_pending.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub(crate) struct ResyncAttempt {
    pending: Arc<AtomicBool>,
    committed: bool,
}

impl ResyncAttempt {
    pub(crate) fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for ResyncAttempt {
    fn drop(&mut self) {
        if !self.committed {
            self.pending.store(false, Ordering::Release);
        }
    }
}

#[derive(Debug)]
pub(crate) struct BacklogReservation {
    queued: Arc<AtomicUsize>,
    global: Arc<AtomicUsize>,
    bytes: usize,
}

impl BacklogReservation {
    pub(crate) const fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Drop for BacklogReservation {
    fn drop(&mut self) {
        self.queued.fetch_sub(self.bytes, Ordering::AcqRel);
        self.global.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

fn reserve_counter(counter: &AtomicUsize, amount: usize, limit: usize) -> bool {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let Some(next) = current.checked_add(amount) else {
            return false;
        };
        if next > limit {
            return false;
        }
        match counter.compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BacklogBudget, BacklogLimits, BacklogPressure};

    #[test]
    fn reservation_bounds_the_global_budget_per_enqueue() {
        let budget = BacklogBudget::new(BacklogLimits::new(10, 15));
        let first = budget.new_sink();
        let second = budget.new_sink();

        let first_reservation = budget
            .try_reserve(&first, 10)
            .expect("first sink fits both budgets");
        assert_eq!(first_reservation.bytes(), 10);

        let pressure = budget
            .try_reserve(&second, 10)
            .expect_err("second sink would exceed the global budget");
        assert_eq!(pressure, BacklogPressure::Global);
        assert_eq!(first.queued(), 10);
        assert_eq!(second.queued(), 0);
        assert_eq!(budget.queued_global(), 10);
    }

    #[test]
    fn reservation_drop_releases_both_counters() {
        let budget = BacklogBudget::new(BacklogLimits::new(10, 10));
        let sink = budget.new_sink();

        let reservation = budget
            .try_reserve(&sink, 7)
            .expect("reservation should fit");
        assert_eq!(sink.queued(), 7);
        assert_eq!(budget.queued_global(), 7);

        drop(reservation);
        assert_eq!(sink.queued(), 0);
        assert_eq!(budget.queued_global(), 0);
    }

    #[test]
    fn failed_resync_signal_does_not_leave_sink_stuck() {
        let budget = BacklogBudget::new(BacklogLimits::new(10, 10));
        let sink = budget.new_sink();

        drop(sink.begin_resync().expect("first resync should start"));
        assert!(!sink.resync_pending());

        sink.begin_resync()
            .expect("resync should be startable again")
            .commit();
        assert!(sink.resync_pending());
        assert_eq!(
            budget.try_reserve(&sink, 1).unwrap_err(),
            BacklogPressure::AlreadyResyncing
        );

        sink.complete_resync();
        assert!(budget.try_reserve(&sink, 1).is_ok());
    }
}
