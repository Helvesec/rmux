use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

const TICKET_PENDING: u8 = 0;
const TICKET_CLAIMED: u8 = 1;
const TICKET_FINISHED: u8 = 2;

/// Orders lifecycle publication by the point at which an event was prepared
/// while the authoritative state lock was held.
#[derive(Debug)]
pub(crate) struct LifecycleCommitOrder {
    admission: Mutex<LifecycleCommitAdmission>,
    coordinator: Arc<LifecycleCommitCoordinator>,
    pending: LifecycleCommitPending,
}

#[derive(Debug)]
struct LifecycleCommitAdmission {
    accepting: bool,
    accepting_publications: bool,
    next_sequence: u64,
}

impl Default for LifecycleCommitOrder {
    fn default() -> Self {
        Self {
            admission: Mutex::new(LifecycleCommitAdmission {
                accepting: true,
                accepting_publications: true,
                next_sequence: 0,
            }),
            coordinator: Arc::default(),
            pending: LifecycleCommitPending::default(),
        }
    }
}

impl LifecycleCommitOrder {
    pub(crate) fn try_reserve(&self) -> Option<LifecycleCommitTicket> {
        let sequence = {
            let mut admission = self
                .admission
                .lock()
                .expect("lifecycle commit admission must not be poisoned");
            if !admission.accepting {
                return None;
            }
            let sequence = admission.next_sequence;
            admission.next_sequence = admission.next_sequence.wrapping_add(1);
            self.pending.start();
            sequence
        };
        Some(LifecycleCommitTicket {
            inner: Arc::new(LifecycleCommitTicketInner {
                sequence,
                coordinator: Arc::clone(&self.coordinator),
                state: AtomicU8::new(TICKET_PENDING),
                finished: Notify::new(),
                pending: self.pending.clone(),
                pending_accounted: AtomicU8::new(1),
            }),
        })
    }

    pub(crate) fn close(&self) -> LifecycleCommitPending {
        self.admission
            .lock()
            .expect("lifecycle commit admission must not be poisoned")
            .accepting = false;
        self.pending.clone()
    }

    /// Tracks a hookless publication that does not own an ordered hook ticket.
    ///
    /// Hook admission may already be closed during shutdown, but committed
    /// control effects still have to remain visible to the final drain barrier.
    pub(crate) fn track_unordered_publication(&self) -> Option<LifecyclePublicationGuard> {
        let admission = self
            .admission
            .lock()
            .expect("lifecycle commit admission must not be poisoned");
        if !admission.accepting_publications {
            return None;
        }
        self.pending.start();
        Some(LifecyclePublicationGuard {
            inner: Arc::new(LifecyclePublicationGuardInner {
                pending: self.pending.clone(),
            }),
        })
    }

    /// Seals the final publication boundary after all ordinary producers stop.
    pub(crate) fn seal_publications(&self) -> LifecycleCommitPending {
        let mut admission = self
            .admission
            .lock()
            .expect("lifecycle commit admission must not be poisoned");
        admission.accepting = false;
        admission.accepting_publications = false;
        self.pending.clone()
    }

    #[cfg(test)]
    pub(crate) fn pending(&self) -> LifecycleCommitPending {
        self.pending.clone()
    }

    #[cfg(test)]
    fn reserve(&self) -> LifecycleCommitTicket {
        self.try_reserve()
            .expect("test lifecycle commit admission remains open")
    }
}

/// Keeps one hookless lifecycle publication visible to shutdown barriers.
#[derive(Debug, Clone)]
pub(crate) struct LifecyclePublicationGuard {
    inner: Arc<LifecyclePublicationGuardInner>,
}

#[derive(Debug)]
struct LifecyclePublicationGuardInner {
    pending: LifecycleCommitPending,
}

impl Drop for LifecyclePublicationGuardInner {
    fn drop(&mut self) {
        self.pending.finish();
    }
}

impl PartialEq for LifecyclePublicationGuard {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for LifecyclePublicationGuard {}

#[derive(Debug, Default)]
struct LifecycleCommitCoordinator {
    state: Mutex<LifecycleCommitCoordinatorState>,
    advanced: Notify,
}

#[derive(Debug, Default)]
struct LifecycleCommitCoordinatorState {
    next_sequence: u64,
    finished_out_of_order: BTreeSet<u64>,
}

impl LifecycleCommitCoordinator {
    async fn wait_for_turn(&self, sequence: u64) {
        loop {
            let advanced = self.advanced.notified();
            tokio::pin!(advanced);
            advanced.as_mut().enable();
            if self
                .state
                .lock()
                .expect("lifecycle commit coordinator must not be poisoned")
                .next_sequence
                == sequence
            {
                return;
            }
            advanced.await;
        }
    }

    fn finish(&self, sequence: u64) {
        let advanced = {
            let mut state = self
                .state
                .lock()
                .expect("lifecycle commit coordinator must not be poisoned");
            if sequence < state.next_sequence {
                false
            } else if sequence == state.next_sequence {
                state.next_sequence = state.next_sequence.wrapping_add(1);
                while {
                    let next_sequence = state.next_sequence;
                    state.finished_out_of_order.remove(&next_sequence)
                } {
                    state.next_sequence = state.next_sequence.wrapping_add(1);
                }
                true
            } else {
                state.finished_out_of_order.insert(sequence);
                false
            }
        };
        if advanced {
            self.advanced.notify_waiters();
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LifecycleCommitPending {
    inner: Arc<LifecycleCommitPendingInner>,
}

#[derive(Debug, Default)]
struct LifecycleCommitPendingInner {
    count: Mutex<usize>,
    idle: Notify,
}

impl LifecycleCommitPending {
    fn start(&self) {
        let mut count = self
            .inner
            .count
            .lock()
            .expect("lifecycle pending count must not be poisoned");
        *count = count.saturating_add(1);
    }

    fn finish(&self) {
        let is_idle = {
            let mut count = self
                .inner
                .count
                .lock()
                .expect("lifecycle pending count must not be poisoned");
            debug_assert!(*count > 0, "lifecycle pending count underflow");
            *count = count.saturating_sub(1);
            *count == 0
        };
        if is_idle {
            self.inner.idle.notify_waiters();
        }
    }

    pub(crate) async fn wait_until_idle(&self) {
        loop {
            let idle = self.inner.idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            if *self
                .inner
                .count
                .lock()
                .expect("lifecycle pending count must not be poisoned")
                == 0
            {
                return;
            }
            idle.await;
        }
    }
}

/// A clonable reference to one committed lifecycle position.
///
/// Production clones represent the same committed event (for example an
/// alert plan cloned before dispatch), so exactly one clone claims the turn.
#[derive(Debug, Clone)]
pub(crate) struct LifecycleCommitTicket {
    inner: Arc<LifecycleCommitTicketInner>,
}

#[derive(Debug)]
struct LifecycleCommitTicketInner {
    sequence: u64,
    coordinator: Arc<LifecycleCommitCoordinator>,
    state: AtomicU8,
    finished: Notify,
    pending: LifecycleCommitPending,
    pending_accounted: AtomicU8,
}

impl LifecycleCommitTicketInner {
    fn finish_once(&self) {
        if self.state.swap(TICKET_FINISHED, Ordering::AcqRel) == TICKET_FINISHED {
            return;
        }
        self.coordinator.finish(self.sequence);
        self.finish_pending_once();
        self.finished.notify_waiters();
    }

    fn finish_pending_once(&self) {
        if self.pending_accounted.swap(0, Ordering::AcqRel) == 1 {
            self.pending.finish();
        }
    }
}

impl Drop for LifecycleCommitTicketInner {
    fn drop(&mut self) {
        self.finish_once();
    }
}

/// Owns a claimed position while its predecessor is still outstanding.
///
/// Dropping the future returned by `wait_for_turn` also drops this guard. That
/// marks the abandoned position finished, while the coordinator keeps later
/// positions behind every earlier outstanding commit.
#[derive(Debug)]
struct LifecycleCommitClaimGuard {
    ticket: Option<LifecycleCommitTicket>,
}

impl LifecycleCommitClaimGuard {
    fn into_turn(mut self) -> LifecycleCommitTurn {
        LifecycleCommitTurn {
            ticket: self.ticket.take(),
        }
    }
}

impl Drop for LifecycleCommitClaimGuard {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            ticket.inner.finish_once();
        }
    }
}

impl LifecycleCommitTicket {
    pub(crate) async fn wait_for_turn(&self) -> LifecycleCommitTurn {
        if self
            .inner
            .state
            .compare_exchange(
                TICKET_PENDING,
                TICKET_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            let claim = LifecycleCommitClaimGuard {
                ticket: Some(self.clone()),
            };
            self.inner
                .coordinator
                .wait_for_turn(self.inner.sequence)
                .await;
            return claim.into_turn();
        }

        loop {
            let finished = self.inner.finished.notified();
            tokio::pin!(finished);
            finished.as_mut().enable();
            if self.inner.state.load(Ordering::Acquire) == TICKET_FINISHED {
                return LifecycleCommitTurn { ticket: None };
            }
            finished.await;
        }
    }

    #[cfg(test)]
    pub(crate) fn sequence(&self) -> u64 {
        self.inner.sequence
    }
}

impl PartialEq for LifecycleCommitTicket {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for LifecycleCommitTicket {}

/// Releases the next committed lifecycle event when publication admission is
/// complete. Hook execution itself deliberately happens after this turn.
#[derive(Debug)]
pub(crate) struct LifecycleCommitTurn {
    ticket: Option<LifecycleCommitTicket>,
}

impl Drop for LifecycleCommitTurn {
    fn drop(&mut self) {
        let Some(ticket) = self.ticket.take() else {
            return;
        };
        ticket.inner.finish_once();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{LifecycleCommitOrder, TICKET_CLAIMED, TICKET_FINISHED};

    #[tokio::test]
    async fn turns_follow_reservation_order_even_when_waiters_arrive_reversed() {
        let order = LifecycleCommitOrder::default();
        let first = order.reserve();
        let second = order.reserve();
        let observed = Arc::new(Mutex::new(Vec::new()));

        let second_observed = Arc::clone(&observed);
        let second_task = tokio::spawn(async move {
            let _turn = second.wait_for_turn().await;
            second_observed.lock().expect("observed order").push(2);
        });
        tokio::task::yield_now().await;
        assert!(observed.lock().expect("observed order").is_empty());

        let first_observed = Arc::clone(&observed);
        let first_task = tokio::spawn(async move {
            let _turn = first.wait_for_turn().await;
            first_observed.lock().expect("observed order").push(1);
        });

        first_task.await.expect("first waiter");
        second_task.await.expect("second waiter");
        assert_eq!(*observed.lock().expect("observed order"), vec![1, 2]);
    }

    #[tokio::test]
    async fn dropping_an_unpublished_ticket_releases_its_successor() {
        let order = LifecycleCommitOrder::default();
        let first = order.reserve();
        let second = order.reserve();
        drop(first);

        tokio::time::timeout(std::time::Duration::from_secs(1), second.wait_for_turn())
            .await
            .expect("cancelled predecessor releases successor");
    }

    #[tokio::test]
    async fn cancelling_claim_while_waiting_preserves_order_and_releases_successor() {
        let order = LifecycleCommitOrder::default();
        let predecessor = order.reserve();
        let cancelled = order.reserve();
        let successor = order.reserve();
        let predecessor_turn = predecessor.wait_for_turn().await;

        let cancelled_probe = cancelled.clone();
        let cancelled_task = tokio::spawn(async move {
            let _turn = cancelled.wait_for_turn().await;
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while cancelled_probe.inner.state.load(Ordering::Acquire) != TICKET_CLAIMED {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled waiter claims its position");

        let successor_probe = successor.clone();
        let mut successor_task = tokio::spawn(async move {
            let _turn = successor.wait_for_turn().await;
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while successor_probe.inner.state.load(Ordering::Acquire) != TICKET_CLAIMED {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("successor claims its position");

        cancelled_task.abort();
        assert!(cancelled_task
            .await
            .expect_err("cancelled waiter must not complete")
            .is_cancelled());
        assert_eq!(
            cancelled_probe.inner.state.load(Ordering::Acquire),
            TICKET_FINISHED
        );
        tokio::task::yield_now().await;
        assert!(
            !successor_task.is_finished(),
            "cancellation must not let the successor overtake its predecessor"
        );

        drop(predecessor_turn);
        tokio::time::timeout(Duration::from_secs(1), &mut successor_task)
            .await
            .expect("successor advances after the predecessor finishes")
            .expect("successor waiter joins");
    }

    #[tokio::test]
    async fn pending_wait_includes_unpublished_tickets_and_finishes_on_cancellation() {
        let order = LifecycleCommitOrder::default();
        let pending = order.pending();
        let ticket = order.reserve();
        let mut idle = tokio::spawn(async move { pending.wait_until_idle().await });
        tokio::task::yield_now().await;
        assert!(!idle.is_finished());

        drop(ticket);
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut idle)
            .await
            .expect("cancelling the unpublished ticket reaches idle")
            .expect("idle waiter joins");
    }

    #[tokio::test]
    async fn closing_admission_rejects_later_tickets_and_waits_for_earlier_ones() {
        let order = LifecycleCommitOrder::default();
        let ticket = order.reserve();
        let pending = order.close();
        assert!(order.try_reserve().is_none());

        let mut drained = tokio::spawn(async move { pending.wait_until_idle().await });
        tokio::task::yield_now().await;
        assert!(!drained.is_finished());
        drop(ticket);
        tokio::time::timeout(Duration::from_secs(1), &mut drained)
            .await
            .expect("pre-close ticket drains")
            .expect("drain waiter joins");
    }

    #[tokio::test]
    async fn hookless_publication_after_admission_close_is_still_counted() {
        let order = LifecycleCommitOrder::default();
        let pending = order.close();
        let publication = order
            .track_unordered_publication()
            .expect("hook closure still accepts hookless control effects");
        let mut drained = tokio::spawn(async move { pending.wait_until_idle().await });

        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut drained)
                .await
                .is_err(),
            "the final barrier must include hookless committed effects"
        );

        drop(publication);
        tokio::time::timeout(Duration::from_secs(1), drained)
            .await
            .expect("hookless publication releases final barrier")
            .expect("drain waiter joins");
    }

    #[tokio::test]
    async fn sealing_publications_linearizes_the_final_idle_barrier() {
        let order = LifecycleCommitOrder::default();
        let publication = order
            .track_unordered_publication()
            .expect("publication admission starts open");
        let pending = order.seal_publications();
        assert!(order.track_unordered_publication().is_none());
        assert!(order.try_reserve().is_none());

        let mut drained = tokio::spawn(async move { pending.wait_until_idle().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut drained)
                .await
                .is_err(),
            "the pre-seal publication remains part of the final barrier"
        );
        drop(publication);
        tokio::time::timeout(Duration::from_secs(1), drained)
            .await
            .expect("pre-seal publication drains")
            .expect("drain waiter joins");
    }

    #[test]
    fn cloned_ticket_keeps_one_commit_identity() {
        let order = LifecycleCommitOrder::default();
        let ticket = order.reserve();
        let clone = ticket.clone();
        assert_eq!(ticket, clone);
        assert_eq!(ticket.sequence(), clone.sequence());

        let unrelated = LifecycleCommitOrder::default().reserve();
        assert_ne!(ticket, unrelated);
    }
}
