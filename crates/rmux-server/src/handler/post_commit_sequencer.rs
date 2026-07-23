use std::collections::BTreeSet;
use std::future::Future;
use std::sync::{Arc, Mutex};

use rmux_proto::RmuxError;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

use super::RequestHandler;

tokio::task_local! {
    static CURRENT_POST_COMMIT_RELEASE: Arc<PostCommitRelease>;
}

#[derive(Debug)]
struct SequencerState {
    accepting_normal: bool,
    accepting_lifecycle_hook: bool,
    pending_normal: usize,
    pending_lifecycle_hook: usize,
    next_reservation_sequence: u64,
    next_turn_sequence: u64,
    finished_out_of_order: BTreeSet<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostCommitLane {
    Normal,
    LifecycleHook,
}

#[derive(Debug)]
pub(in crate::handler) struct PostCommitSequencer {
    slots: Arc<Semaphore>,
    state: Mutex<SequencerState>,
    drained: Notify,
    turn_advanced: Notify,
}

impl PostCommitSequencer {
    pub(in crate::handler) fn new(capacity: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(capacity)),
            state: Mutex::new(SequencerState {
                accepting_normal: true,
                accepting_lifecycle_hook: true,
                pending_normal: 0,
                pending_lifecycle_hook: 0,
                next_reservation_sequence: 0,
                next_turn_sequence: 0,
                finished_out_of_order: BTreeSet::new(),
            }),
            drained: Notify::new(),
            turn_advanced: Notify::new(),
        }
    }

    pub(in crate::handler) async fn acquire_capacity(
        self: &Arc<Self>,
    ) -> Result<PostCommitCapacity, RmuxError> {
        let lane = if crate::hook_runtime::lifecycle_hooks_disabled() {
            PostCommitLane::LifecycleHook
        } else {
            PostCommitLane::Normal
        };
        let slot = self.slots.clone().acquire_owned().await.map_err(|_| {
            RmuxError::Server("server is shutting down; mutation was not started".to_owned())
        })?;
        self.register_capacity(slot, lane)
    }

    fn register_capacity(
        self: &Arc<Self>,
        slot: OwnedSemaphorePermit,
        lane: PostCommitLane,
    ) -> Result<PostCommitCapacity, RmuxError> {
        {
            let mut state = self
                .state
                .lock()
                .expect("post-commit sequencer state must not be poisoned");
            let accepting = match lane {
                PostCommitLane::Normal => state.accepting_normal,
                PostCommitLane::LifecycleHook => state.accepting_lifecycle_hook,
            };
            if !accepting {
                return Err(RmuxError::Server(
                    "server is shutting down; mutation was not started".to_owned(),
                ));
            }
            match lane {
                PostCommitLane::Normal => {
                    state.pending_normal = state.pending_normal.saturating_add(1);
                }
                PostCommitLane::LifecycleHook => {
                    state.pending_lifecycle_hook = state.pending_lifecycle_hook.saturating_add(1);
                }
            }
        }
        Ok(PostCommitCapacity {
            sequencer: Arc::clone(self),
            slot: Some(slot),
            pending: Some(PostCommitPending {
                sequencer: Arc::clone(self),
                lane,
            }),
        })
    }

    pub(crate) async fn close_normal_and_drain(&self) {
        loop {
            let drained = self.drained.notified();
            tokio::pin!(drained);
            drained.as_mut().enable();
            let is_drained = {
                let mut state = self
                    .state
                    .lock()
                    .expect("post-commit sequencer state must not be poisoned");
                state.accepting_normal = false;
                state.pending_normal == 0
            };
            if is_drained {
                return;
            }
            drained.await;
        }
    }

    pub(crate) async fn close_and_drain(&self) {
        loop {
            let drained = self.drained.notified();
            tokio::pin!(drained);
            drained.as_mut().enable();
            let is_drained = {
                let mut state = self
                    .state
                    .lock()
                    .expect("post-commit sequencer state must not be poisoned");
                state.accepting_normal = false;
                state.accepting_lifecycle_hook = false;
                self.slots.close();
                state.pending_normal == 0 && state.pending_lifecycle_hook == 0
            };
            if is_drained {
                return;
            }
            drained.await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn wait_until_idle(&self) {
        loop {
            let drained = self.drained.notified();
            tokio::pin!(drained);
            drained.as_mut().enable();
            let is_drained = {
                let state = self
                    .state
                    .lock()
                    .expect("post-commit sequencer state must not be poisoned");
                state.pending_normal == 0 && state.pending_lifecycle_hook == 0
            };
            if is_drained {
                return;
            }
            drained.await;
        }
    }

    fn finish_pending(&self, lane: PostCommitLane) {
        {
            let mut state = self
                .state
                .lock()
                .expect("post-commit sequencer state must not be poisoned");
            match lane {
                PostCommitLane::Normal => {
                    debug_assert!(
                        state.pending_normal > 0,
                        "normal post-commit pending count underflow"
                    );
                    state.pending_normal = state.pending_normal.saturating_sub(1);
                }
                PostCommitLane::LifecycleHook => {
                    debug_assert!(
                        state.pending_lifecycle_hook > 0,
                        "lifecycle-hook post-commit pending count underflow"
                    );
                    state.pending_lifecycle_hook = state.pending_lifecycle_hook.saturating_sub(1);
                }
            }
        }
        self.drained.notify_waiters();
    }

    async fn wait_for_turn(&self, sequence: u64) {
        loop {
            let advanced = self.turn_advanced.notified();
            tokio::pin!(advanced);
            advanced.as_mut().enable();
            if self
                .state
                .lock()
                .expect("post-commit sequencer state must not be poisoned")
                .next_turn_sequence
                == sequence
            {
                return;
            }
            advanced.await;
        }
    }

    fn finish_turn(&self, sequence: u64) {
        let advanced = {
            let mut state = self
                .state
                .lock()
                .expect("post-commit sequencer state must not be poisoned");
            if sequence < state.next_turn_sequence {
                false
            } else if sequence == state.next_turn_sequence {
                state.next_turn_sequence = state.next_turn_sequence.wrapping_add(1);
                loop {
                    let next_sequence = state.next_turn_sequence;
                    if !state.finished_out_of_order.remove(&next_sequence) {
                        break;
                    }
                    state.next_turn_sequence = state.next_turn_sequence.wrapping_add(1);
                }
                true
            } else {
                state.finished_out_of_order.insert(sequence);
                false
            }
        };
        if advanced {
            self.turn_advanced.notify_waiters();
        }
    }
}

pub(in crate::handler) struct PostCommitCapacity {
    sequencer: Arc<PostCommitSequencer>,
    slot: Option<OwnedSemaphorePermit>,
    pending: Option<PostCommitPending>,
}

impl PostCommitCapacity {
    pub(in crate::handler) fn sequence(mut self) -> PostCommitReservation {
        let sequence = {
            let mut state = self
                .sequencer
                .state
                .lock()
                .expect("post-commit sequencer state must not be poisoned");
            let sequence = state.next_reservation_sequence;
            state.next_reservation_sequence = state.next_reservation_sequence.wrapping_add(1);
            sequence
        };
        PostCommitReservation {
            position: Some(PostCommitPosition {
                sequencer: Arc::clone(&self.sequencer),
                sequence,
            }),
            slot: self.slot.take(),
            pending: self.pending.take(),
        }
    }
}

struct PostCommitPending {
    sequencer: Arc<PostCommitSequencer>,
    lane: PostCommitLane,
}

impl Drop for PostCommitPending {
    fn drop(&mut self) {
        self.sequencer.finish_pending(self.lane);
    }
}

struct PostCommitPosition {
    sequencer: Arc<PostCommitSequencer>,
    sequence: u64,
}

impl Drop for PostCommitPosition {
    fn drop(&mut self) {
        self.sequencer.finish_turn(self.sequence);
    }
}

pub(in crate::handler) struct PostCommitReservation {
    position: Option<PostCommitPosition>,
    slot: Option<OwnedSemaphorePermit>,
    pending: Option<PostCommitPending>,
}

impl PostCommitReservation {
    pub(in crate::handler) async fn wait_for_turn(mut self) -> PostCommitTurn {
        let position = self
            .position
            .as_ref()
            .expect("post-commit reservation must own its position");
        position.sequencer.wait_for_turn(position.sequence).await;
        PostCommitTurn {
            position: self.position.take(),
            slot: self.slot.take(),
            pending: self.pending.take(),
        }
    }

    pub(in crate::handler) async fn run<T>(self, action: impl Future<Output = T> + Send) -> T {
        self.wait_for_turn().await.run(action).await
    }

    pub(in crate::handler) fn run_durable<T, F>(
        self,
        action: F,
    ) -> impl Future<Output = Result<T, RmuxError>> + Send
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let hook_execution = crate::hook_runtime::current_hook_execution();
        let hook_formats = crate::hook_runtime::current_hook_formats();
        // Some pane-mode operations produce large futures. Put the action behind
        // one pointer before constructing the spawned wrapper so spawning does
        // not copy that state through an already deep command-dispatch stack.
        let action = Box::pin(action);
        async move {
            tokio::spawn(async move {
                crate::hook_runtime::with_optional_hook_execution(
                    hook_execution,
                    hook_formats,
                    self.run(action),
                )
                .await
            })
            .await
            .map_err(|error| RmuxError::Server(format!("durable post-commit task failed: {error}")))
        }
    }
}

pub(in crate::handler) struct PostCommitTurn {
    position: Option<PostCommitPosition>,
    slot: Option<OwnedSemaphorePermit>,
    pending: Option<PostCommitPending>,
}

impl Drop for PostCommitTurn {
    fn drop(&mut self) {
        drop(self.position.take());
        drop(self.slot.take());
        drop(self.pending.take());
    }
}

struct PostCommitRelease {
    turn: Mutex<Option<PostCommitTurn>>,
}

impl PostCommitRelease {
    fn new(turn: PostCommitTurn) -> Self {
        Self {
            turn: Mutex::new(Some(turn)),
        }
    }

    fn release(&self) {
        drop(
            self.turn
                .lock()
                .expect("post-commit release state must not be poisoned")
                .take(),
        );
    }
}

/// Lifecycle publication may backpressure behind a hook that needs the next
/// post-commit turn. At that point the mutation is already committed and its
/// lifecycle ticket owns observable ordering, so keeping the post-commit turn
/// would create a lock cycle.
pub(in crate::handler) fn release_current_post_commit_turn() {
    let _ = CURRENT_POST_COMMIT_RELEASE.try_with(|release| release.release());
}

impl PostCommitTurn {
    pub(in crate::handler) async fn run<T>(self, action: impl Future<Output = T> + Send) -> T {
        let release = Arc::new(PostCommitRelease::new(self));
        let output = CURRENT_POST_COMMIT_RELEASE
            .scope(Arc::clone(&release), action)
            .await;
        release.release();
        output
    }
}

impl RequestHandler {
    #[cfg(test)]
    pub(crate) async fn wait_for_post_commit_operations(&self) {
        self.pane_mode_post_commit.wait_until_idle().await;
    }

    pub(crate) async fn close_and_drain_post_commit_operations(&self) {
        self.pane_mode_post_commit.close_and_drain().await;
    }

    pub(crate) async fn close_normal_and_drain_post_commit_operations(&self) {
        self.pane_mode_post_commit.close_normal_and_drain().await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{release_current_post_commit_turn, PostCommitSequencer};

    #[tokio::test]
    async fn reservations_execute_in_reservation_order() {
        let sequencer = Arc::new(PostCommitSequencer::new(2));
        let first = sequencer
            .acquire_capacity()
            .await
            .expect("first reservation")
            .sequence();
        let second = sequencer
            .acquire_capacity()
            .await
            .expect("second reservation")
            .sequence();
        let order = Arc::new(Mutex::new(Vec::new()));

        let second_order = Arc::clone(&order);
        tokio::spawn(async move {
            second
                .run(async move {
                    second_order.lock().expect("order lock").push(2);
                })
                .await;
        });
        let first_order = Arc::clone(&order);
        tokio::spawn(async move {
            first
                .run(async move {
                    first_order.lock().expect("order lock").push(1);
                })
                .await;
        });

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if order.lock().expect("order lock").len() == 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("sequenced actions complete");
        assert_eq!(*order.lock().expect("order lock"), vec![1, 2]);
    }

    #[tokio::test]
    async fn dropped_reservation_does_not_stall_successor() {
        let sequencer = Arc::new(PostCommitSequencer::new(2));
        let first = sequencer
            .acquire_capacity()
            .await
            .expect("first reservation")
            .sequence();
        let second = sequencer
            .acquire_capacity()
            .await
            .expect("second reservation")
            .sequence();
        drop(first);

        let completed = Arc::new(tokio::sync::Notify::new());
        let task_completed = Arc::clone(&completed);
        tokio::spawn(async move {
            second
                .run(async move {
                    task_completed.notify_one();
                })
                .await;
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), completed.notified())
            .await
            .expect("successor runs after predecessor cancellation");
    }

    #[tokio::test]
    async fn cancelling_waiter_preserves_order_before_releasing_successor() {
        let sequencer = Arc::new(PostCommitSequencer::new(3));
        let predecessor = sequencer
            .acquire_capacity()
            .await
            .expect("predecessor reservation")
            .sequence();
        let cancelled = sequencer
            .acquire_capacity()
            .await
            .expect("cancelled reservation")
            .sequence();
        let successor = sequencer
            .acquire_capacity()
            .await
            .expect("successor reservation")
            .sequence();
        let predecessor_turn = predecessor.wait_for_turn().await;

        let cancelled_started = Arc::new(tokio::sync::Notify::new());
        let cancelled_task_started = Arc::clone(&cancelled_started);
        let cancelled_task = tokio::spawn(async move {
            cancelled_task_started.notify_one();
            let _turn = cancelled.wait_for_turn().await;
        });
        cancelled_started.notified().await;
        assert!(!cancelled_task.is_finished());

        let successor_started = Arc::new(tokio::sync::Notify::new());
        let successor_task_started = Arc::clone(&successor_started);
        let mut successor_task = tokio::spawn(async move {
            successor_task_started.notify_one();
            let _turn = successor.wait_for_turn().await;
        });
        successor_started.notified().await;
        assert!(!successor_task.is_finished());

        cancelled_task.abort();
        assert!(cancelled_task
            .await
            .expect_err("cancelled waiter must not complete")
            .is_cancelled());
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
        tokio::time::timeout(Duration::from_secs(1), sequencer.wait_until_idle())
            .await
            .expect("all cancelled and completed positions drain");
    }

    #[tokio::test]
    async fn cancelling_an_active_turn_releases_its_successor() {
        let sequencer = Arc::new(PostCommitSequencer::new(2));
        let first = sequencer
            .acquire_capacity()
            .await
            .expect("first reservation")
            .sequence();
        let second = sequencer
            .acquire_capacity()
            .await
            .expect("second reservation")
            .sequence();
        let first_turn = first.wait_for_turn().await;
        let action_started = Arc::new(tokio::sync::Notify::new());
        let task_action_started = Arc::clone(&action_started);
        let active_task = tokio::spawn(first_turn.run(async move {
            task_action_started.notify_one();
            std::future::pending::<()>().await;
        }));
        action_started.notified().await;

        let mut successor_task = tokio::spawn(second.wait_for_turn());
        tokio::task::yield_now().await;
        assert!(!successor_task.is_finished());

        active_task.abort();
        assert!(active_task
            .await
            .expect_err("active turn must be cancelled")
            .is_cancelled());
        tokio::time::timeout(Duration::from_secs(1), &mut successor_task)
            .await
            .expect("active turn cancellation releases successor")
            .expect("successor waiter joins");
        tokio::time::timeout(Duration::from_secs(1), sequencer.wait_until_idle())
            .await
            .expect("cancelled active turn drains");
    }

    #[tokio::test]
    async fn committed_action_can_release_its_turn_before_waiting_on_external_backpressure() {
        let sequencer = Arc::new(PostCommitSequencer::new(2));
        let first = sequencer
            .acquire_capacity()
            .await
            .expect("first reservation")
            .sequence();
        let second = sequencer
            .acquire_capacity()
            .await
            .expect("second reservation")
            .sequence();
        let (committed_tx, committed_rx) = tokio::sync::oneshot::channel();
        let (backpressure_tx, backpressure_rx) = tokio::sync::oneshot::channel();
        let mut first_task = tokio::spawn(first.run(async move {
            release_current_post_commit_turn();
            let _ = committed_tx.send(());
            let _ = backpressure_rx.await;
        }));
        committed_rx
            .await
            .expect("first action reaches its durable commit boundary");

        let second_task = tokio::spawn(second.run(async {}));
        tokio::time::timeout(Duration::from_secs(1), second_task)
            .await
            .expect("successor does not deadlock behind committed backpressure")
            .expect("successor task joins");
        assert!(
            !first_task.is_finished(),
            "first action remains independently backpressured"
        );

        let _ = backpressure_tx.send(());
        tokio::time::timeout(Duration::from_secs(1), &mut first_task)
            .await
            .expect("first action completes after backpressure clears")
            .expect("first task joins");
        tokio::time::timeout(Duration::from_secs(1), sequencer.wait_until_idle())
            .await
            .expect("released actions drain all capacity accounting");
    }

    #[tokio::test]
    async fn shutdown_waits_for_an_accepted_action_and_rejects_new_work() {
        let sequencer = Arc::new(PostCommitSequencer::new(2));
        let reservation = sequencer
            .acquire_capacity()
            .await
            .expect("accepted reservation")
            .sequence();
        let release = Arc::new(tokio::sync::Notify::new());
        let action_release = Arc::clone(&release);
        tokio::spawn(async move {
            reservation
                .run(async move {
                    action_release.notified().await;
                })
                .await;
        });

        let draining = {
            let sequencer = Arc::clone(&sequencer);
            tokio::spawn(async move { sequencer.close_and_drain().await })
        };
        tokio::task::yield_now().await;
        assert!(sequencer.acquire_capacity().await.is_err());
        assert!(!draining.is_finished(), "accepted action is still pending");

        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), draining)
            .await
            .expect("drain completes")
            .expect("drain task joins");
    }

    #[tokio::test]
    async fn normal_shutdown_lane_drains_accepted_work_but_keeps_lifecycle_hook_lane_open() {
        let sequencer = Arc::new(PostCommitSequencer::new(2));
        let normal = sequencer
            .acquire_capacity()
            .await
            .expect("normal reservation accepted before shutdown");

        let normal_drain = {
            let sequencer = Arc::clone(&sequencer);
            tokio::spawn(async move { sequencer.close_normal_and_drain().await })
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if !sequencer
                    .state
                    .lock()
                    .expect("post-commit state")
                    .accepting_normal
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("normal lane closes");
        assert!(sequencer.acquire_capacity().await.is_err());
        assert!(
            !normal_drain.is_finished(),
            "normal drain waits for work accepted before closure"
        );

        let hook_capacity = crate::hook_runtime::with_hook_execution(
            crate::hook_runtime::HookExecutionContext::lifecycle(
                rmux_proto::HookName::SessionCreated,
            ),
            Vec::new(),
            sequencer.acquire_capacity(),
        )
        .await
        .expect("lifecycle hook lane remains available while hooks drain");
        drop(hook_capacity);
        drop(normal);
        tokio::time::timeout(std::time::Duration::from_secs(1), normal_drain)
            .await
            .expect("normal lane drains")
            .expect("normal drain task joins");

        sequencer.close_and_drain().await;
        let hook_after_close = crate::hook_runtime::with_hook_execution(
            crate::hook_runtime::HookExecutionContext::lifecycle(
                rmux_proto::HookName::SessionCreated,
            ),
            Vec::new(),
            sequencer.acquire_capacity(),
        )
        .await;
        assert!(hook_after_close.is_err());
    }

    #[tokio::test]
    async fn cancelling_run_durable_caller_does_not_cancel_accepted_action() {
        let sequencer = Arc::new(PostCommitSequencer::new(1));
        let reservation = sequencer
            .acquire_capacity()
            .await
            .expect("accepted reservation")
            .sequence();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let completed = Arc::new(AtomicBool::new(false));
        let action_started = Arc::clone(&started);
        let action_release = Arc::clone(&release);
        let action_completed = Arc::clone(&completed);
        let caller = tokio::spawn(async move {
            reservation
                .run_durable(async move {
                    action_started.notify_one();
                    action_release.notified().await;
                    action_completed.store(true, Ordering::SeqCst);
                })
                .await
        });
        started.notified().await;
        caller.abort();
        let _ = caller.await;

        let mut idle = tokio::spawn({
            let sequencer = Arc::clone(&sequencer);
            async move { sequencer.wait_until_idle().await }
        });
        assert!(!idle.is_finished(), "accepted action must remain pending");
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut idle)
            .await
            .expect("durable action drains after caller cancellation")
            .expect("idle waiter joins");
        assert!(completed.load(Ordering::SeqCst));
    }
}
