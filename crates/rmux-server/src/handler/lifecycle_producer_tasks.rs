use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{watch, Notify};

use super::RequestHandler;

/// Coordinates delayed local tasks that may mutate state and publish lifecycle effects.
///
/// A task-wide registration starts pending. Closing a lane cancels pending work and waits for
/// any active mutation guard to finish its durable mutation and publication before cancellation.
#[derive(Debug)]
pub(in crate::handler) struct LifecycleProducerRegistry {
    state: Mutex<LifecycleProducerState>,
    normal_closing: watch::Sender<bool>,
    lifecycle_hook_closing: watch::Sender<bool>,
    idle: Notify,
}

#[derive(Debug)]
struct LifecycleProducerState {
    accepting_normal: bool,
    accepting_lifecycle_hook: bool,
    registrations_normal: usize,
    registrations_lifecycle_hook: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::handler) enum LifecycleProducerLane {
    Normal,
    LifecycleHook,
}

impl LifecycleProducerLane {
    pub(in crate::handler) fn current() -> Self {
        CURRENT_LIFECYCLE_PRODUCER
            .try_with(|registration| registration.lane)
            .unwrap_or_else(|_| {
                if crate::hook_runtime::lifecycle_hooks_disabled() {
                    Self::LifecycleHook
                } else {
                    Self::Normal
                }
            })
    }
}

#[derive(Debug)]
pub(in crate::handler) struct LifecycleProducerRegistration {
    registry: Arc<LifecycleProducerRegistry>,
    closing: watch::Receiver<bool>,
    lane: LifecycleProducerLane,
    execution: Arc<LifecycleProducerExecution>,
}

#[derive(Debug)]
struct LifecycleProducerExecution {
    mutation_depth: AtomicUsize,
    mutation_rejected: AtomicBool,
    pending: Notify,
}

#[derive(Debug)]
pub(in crate::handler) struct LifecycleMutationGuard {
    execution: Option<Arc<LifecycleProducerExecution>>,
}

#[derive(Clone)]
pub(in crate::handler) struct LifecycleProducerCancellation {
    closing: watch::Receiver<bool>,
    execution: Arc<LifecycleProducerExecution>,
}

tokio::task_local! {
    static CURRENT_LIFECYCLE_PRODUCER: LifecycleProducerRegistration;
}

impl LifecycleProducerRegistry {
    pub(in crate::handler) fn new() -> Self {
        let (normal_closing, _normal_receiver) = watch::channel(false);
        let (lifecycle_hook_closing, _lifecycle_hook_receiver) = watch::channel(false);
        Self {
            state: Mutex::new(LifecycleProducerState {
                accepting_normal: true,
                accepting_lifecycle_hook: true,
                registrations_normal: 0,
                registrations_lifecycle_hook: 0,
            }),
            normal_closing,
            lifecycle_hook_closing,
            idle: Notify::new(),
        }
    }

    pub(in crate::handler) fn try_register(
        self: &Arc<Self>,
    ) -> Option<LifecycleProducerRegistration> {
        self.try_register_in_lane_inner(LifecycleProducerLane::current())
    }

    pub(in crate::handler) fn try_register_in_lane(
        self: &Arc<Self>,
        lane: LifecycleProducerLane,
    ) -> Option<LifecycleProducerRegistration> {
        self.try_register_in_lane_inner(lane)
    }

    fn try_register_in_lane_inner(
        self: &Arc<Self>,
        lane: LifecycleProducerLane,
    ) -> Option<LifecycleProducerRegistration> {
        let mut state = self
            .state
            .lock()
            .expect("lifecycle producer registry must not be poisoned");
        match lane {
            LifecycleProducerLane::Normal if state.accepting_normal => {
                state.registrations_normal = state.registrations_normal.saturating_add(1);
            }
            LifecycleProducerLane::LifecycleHook if state.accepting_lifecycle_hook => {
                state.registrations_lifecycle_hook =
                    state.registrations_lifecycle_hook.saturating_add(1);
            }
            LifecycleProducerLane::Normal | LifecycleProducerLane::LifecycleHook => return None,
        }
        let closing = match lane {
            LifecycleProducerLane::Normal => self.normal_closing.subscribe(),
            LifecycleProducerLane::LifecycleHook => self.lifecycle_hook_closing.subscribe(),
        };
        Some(LifecycleProducerRegistration {
            registry: Arc::clone(self),
            closing,
            lane,
            execution: Arc::new(LifecycleProducerExecution {
                mutation_depth: AtomicUsize::new(0),
                mutation_rejected: AtomicBool::new(false),
                pending: Notify::new(),
            }),
        })
    }

    pub(in crate::handler) async fn close_normal_and_wait(&self) {
        {
            let mut state = self
                .state
                .lock()
                .expect("lifecycle producer registry must not be poisoned");
            state.accepting_normal = false;
        }
        self.normal_closing.send_replace(true);
        self.wait_until_idle(false).await;
    }

    #[cfg(test)]
    async fn wait_until_normal_closing_for_test(&self) {
        let mut closing = self.normal_closing.subscribe();
        if *closing.borrow() {
            return;
        }
        while closing.changed().await.is_ok() {
            if *closing.borrow_and_update() {
                return;
            }
        }
    }

    pub(in crate::handler) async fn close_and_wait(&self) {
        {
            let mut state = self
                .state
                .lock()
                .expect("lifecycle producer registry must not be poisoned");
            state.accepting_normal = false;
            state.accepting_lifecycle_hook = false;
        }
        self.normal_closing.send_replace(true);
        self.lifecycle_hook_closing.send_replace(true);
        self.wait_until_idle(true).await;
    }

    async fn wait_until_idle(&self, include_lifecycle_hook: bool) {
        loop {
            let idle = self.idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            let is_idle = {
                let state = self
                    .state
                    .lock()
                    .expect("lifecycle producer registry must not be poisoned");
                state.registrations_normal == 0
                    && (!include_lifecycle_hook || state.registrations_lifecycle_hook == 0)
            };
            if is_idle {
                return;
            }
            idle.await;
        }
    }
}

impl LifecycleProducerRegistration {
    fn duplicate_for_scoped_execution(&self) -> Self {
        {
            let mut state = self
                .registry
                .state
                .lock()
                .expect("lifecycle producer registry must not be poisoned");
            match self.lane {
                LifecycleProducerLane::Normal => {
                    state.registrations_normal = state.registrations_normal.saturating_add(1);
                }
                LifecycleProducerLane::LifecycleHook => {
                    state.registrations_lifecycle_hook =
                        state.registrations_lifecycle_hook.saturating_add(1);
                }
            }
        }
        Self {
            registry: Arc::clone(&self.registry),
            closing: self.closing.clone(),
            lane: self.lane,
            execution: Arc::clone(&self.execution),
        }
    }

    fn can_continue(&self) -> bool {
        let state = self
            .registry
            .state
            .lock()
            .expect("lifecycle producer registry must not be poisoned");
        if self.execution.mutation_depth.load(Ordering::SeqCst) != 0 {
            return true;
        }
        match self.lane {
            LifecycleProducerLane::Normal => state.accepting_normal,
            LifecycleProducerLane::LifecycleHook => state.accepting_lifecycle_hook,
        }
    }

    #[cfg(windows)]
    fn lane_is_closing(&self) -> bool {
        *self.closing.borrow()
    }

    /// Starts a bounded local mutation scope, linearized against lane closure.
    pub(in crate::handler) fn try_begin_mutation(&self) -> Option<LifecycleMutationGuard> {
        let state = self
            .registry
            .state
            .lock()
            .expect("lifecycle producer registry must not be poisoned");
        let already_mutating = self.execution.mutation_depth.load(Ordering::SeqCst) != 0;
        let accepting = match self.lane {
            LifecycleProducerLane::Normal => state.accepting_normal,
            LifecycleProducerLane::LifecycleHook => state.accepting_lifecycle_hook,
        };
        if !already_mutating && !accepting {
            self.execution
                .mutation_rejected
                .store(true, Ordering::SeqCst);
            return None;
        }
        self.execution.mutation_depth.fetch_add(1, Ordering::SeqCst);
        Some(LifecycleMutationGuard {
            execution: Some(Arc::clone(&self.execution)),
        })
    }

    pub(in crate::handler) fn cancellation(&self) -> LifecycleProducerCancellation {
        LifecycleProducerCancellation {
            closing: self.closing.clone(),
            execution: Arc::clone(&self.execution),
        }
    }

    fn begin_cancellation_cleanup_mutation(&self) -> LifecycleMutationGuard {
        debug_assert_eq!(self.execution.mutation_depth.load(Ordering::SeqCst), 0);
        self.execution.mutation_depth.fetch_add(1, Ordering::SeqCst);
        LifecycleMutationGuard {
            execution: Some(Arc::clone(&self.execution)),
        }
    }
}

impl LifecycleProducerCancellation {
    pub(in crate::handler) fn is_mutating(&self) -> bool {
        self.execution.mutation_depth.load(Ordering::SeqCst) != 0
    }

    fn mutation_was_rejected(&self) -> bool {
        self.execution.mutation_rejected.load(Ordering::SeqCst)
    }

    pub(in crate::handler) async fn cancelled(&mut self) {
        while !*self.closing.borrow() {
            if self.closing.changed().await.is_err() {
                return;
            }
        }
    }

    pub(in crate::handler) async fn wait_until_pending(&self) {
        loop {
            let pending = self.execution.pending.notified();
            tokio::pin!(pending);
            pending.as_mut().enable();
            if !self.is_mutating() {
                return;
            }
            pending.await;
        }
    }
}

/// Waits until the parent has finished installing a pre-admitted continuation.
///
/// A caller may hold a mutation guard on the continuation's registration while it publishes
/// task-owned state (for example, inserting a timer handle). The spawned future must not be
/// polled while that guard is live: sharing the same execution would otherwise make an early
/// child mutation look nested and let it race ahead of the hand-off. Lane closure wins this
/// barrier and cancels the continuation once the parent mutation has drained.
async fn wait_until_handoff_complete(cancellation: &mut LifecycleProducerCancellation) -> bool {
    let pending_after_cancel = cancellation.clone();
    let pending_without_cancel = cancellation.clone();
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            pending_after_cancel.wait_until_pending().await;
            false
        }
        _ = pending_without_cancel.wait_until_pending() => true,
    }
}

impl Drop for LifecycleMutationGuard {
    fn drop(&mut self) {
        let Some(execution) = self.execution.as_ref() else {
            return;
        };
        let previous = execution.mutation_depth.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(previous > 0);
        if previous == 1 {
            execution.pending.notify_waiters();
        }
    }
}

pub(in crate::handler) async fn run_registered_lifecycle_producer<T, F>(
    registration: LifecycleProducerRegistration,
    future: F,
) -> Option<T>
where
    F: Future<Output = T>,
{
    let mut cancellation = registration.cancellation();
    if !wait_until_handoff_complete(&mut cancellation).await {
        return None;
    }
    let mut task = Box::pin(CURRENT_LIFECYCLE_PRODUCER.scope(registration, future));
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            if !cancellation.is_mutating() {
                return None;
            }
            tokio::select! {
                biased;
                _ = cancellation.wait_until_pending() => None,
                output = task.as_mut() => {
                    if cancellation.mutation_was_rejected() {
                        None
                    } else {
                        Some(output)
                    }
                },
            }
        }
        output = task.as_mut() => {
            if cancellation.mutation_was_rejected() {
                None
            } else {
                Some(output)
            }
        },
    }
}

/// Runs a producer and, if lane closure cancels it while Pending, performs one bounded local
/// cleanup before releasing the producer registration. The cleanup must not wait on external
/// work; it exists to roll back task-owned in-memory state that would otherwise be orphaned.
/// It runs outside the task-local producer scope, so it must not spawn follow-on producers or
/// derive lifecycle-lane authority of its own.
pub(in crate::handler) async fn run_registered_lifecycle_producer_with_cancellation_cleanup<
    T,
    F,
    C,
>(
    registration: LifecycleProducerRegistration,
    future: F,
    cleanup: C,
) -> Option<T>
where
    F: Future<Output = T>,
    C: Future<Output = ()>,
{
    let mut cancellation = registration.cancellation();
    if !wait_until_handoff_complete(&mut cancellation).await {
        let cleanup_mutation = registration.begin_cancellation_cleanup_mutation();
        cleanup.await;
        drop(cleanup_mutation);
        return None;
    }
    let scoped_registration = registration.duplicate_for_scoped_execution();
    let mut task = Box::pin(CURRENT_LIFECYCLE_PRODUCER.scope(scoped_registration, future));
    let output = tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            if cancellation.is_mutating() {
                tokio::select! {
                    biased;
                    _ = cancellation.wait_until_pending() => None,
                    output = task.as_mut() => Some(output),
                }
            } else {
                None
            }
        }
        output = task.as_mut() => Some(output),
    };
    if let Some(output) = output {
        if !cancellation.mutation_was_rejected() {
            return Some(output);
        }
        drop(output);
    }
    drop(task);
    let cleanup_mutation = registration.begin_cancellation_cleanup_mutation();
    cleanup.await;
    drop(cleanup_mutation);
    None
}

/// Starts a mutation guard for the current delayed producer. Ordinary request paths that are not
/// running inside a registered producer are already protected by normal-request admission.
pub(in crate::handler) fn begin_current_lifecycle_mutation() -> Option<LifecycleMutationGuard> {
    CURRENT_LIFECYCLE_PRODUCER
        .try_with(LifecycleProducerRegistration::try_begin_mutation)
        .unwrap_or_else(|_| Some(LifecycleMutationGuard { execution: None }))
}

pub(in crate::handler) fn current_lifecycle_producer_can_continue() -> bool {
    CURRENT_LIFECYCLE_PRODUCER
        .try_with(LifecycleProducerRegistration::can_continue)
        .unwrap_or(true)
}

#[cfg(windows)]
pub(in crate::handler) fn current_lifecycle_producer_is_closing() -> bool {
    CURRENT_LIFECYCLE_PRODUCER
        .try_with(LifecycleProducerRegistration::lane_is_closing)
        .unwrap_or(false)
}

#[cfg(any(windows, debug_assertions))]
pub(in crate::handler) fn current_lifecycle_producer_is_mutating() -> bool {
    CURRENT_LIFECYCLE_PRODUCER
        .try_with(|registration| registration.execution.mutation_depth.load(Ordering::SeqCst) != 0)
        .unwrap_or(false)
}

impl Drop for LifecycleProducerRegistration {
    fn drop(&mut self) {
        debug_assert_eq!(
            self.execution.mutation_depth.load(Ordering::SeqCst),
            0,
            "lifecycle producer registration dropped while mutating"
        );
        let lane_became_idle = {
            let mut state = self
                .registry
                .state
                .lock()
                .expect("lifecycle producer registry must not be poisoned");
            match self.lane {
                LifecycleProducerLane::Normal => {
                    debug_assert!(state.registrations_normal > 0);
                    state.registrations_normal = state.registrations_normal.saturating_sub(1);
                    state.registrations_normal == 0
                }
                LifecycleProducerLane::LifecycleHook => {
                    debug_assert!(state.registrations_lifecycle_hook > 0);
                    state.registrations_lifecycle_hook =
                        state.registrations_lifecycle_hook.saturating_sub(1);
                    state.registrations_lifecycle_hook == 0
                }
            }
        };
        if lane_became_idle {
            self.registry.idle.notify_waiters();
        }
    }
}

impl Default for LifecycleProducerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestHandler {
    #[cfg(test)]
    pub(in crate::handler) fn try_begin_lifecycle_hook_producer(
        &self,
    ) -> Option<LifecycleProducerRegistration> {
        self.lifecycle_producers
            .try_register_in_lane(LifecycleProducerLane::LifecycleHook)
    }

    pub(crate) async fn close_normal_and_drain_lifecycle_producers(&self) {
        self.lifecycle_producers.close_normal_and_wait().await;
    }

    #[cfg(test)]
    pub(crate) async fn wait_until_normal_lifecycle_producers_closing_for_test(&self) {
        self.lifecycle_producers
            .wait_until_normal_closing_for_test()
            .await;
    }

    pub(crate) async fn close_and_drain_lifecycle_producers(&self) {
        self.lifecycle_producers.close_and_wait().await;
    }
}

#[cfg(test)]
#[path = "lifecycle_producer_tasks/tests.rs"]
mod tests;
