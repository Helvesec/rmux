//! A deadline-bound synchronous drain with operation-local cancellation.
//!
//! Windows named-pipe draining uses a synchronous `FlushFileBuffers` call whose
//! completion is controlled by the peer. The call cannot run on Tokio's blocking
//! pool: canceling its join handle does not cancel the call, and runtime teardown
//! waits for every blocking worker. Instead, each drain owns a dedicated OS
//! thread and a second dedicated deadline thread. Dropping the async waiter asks
//! that deadline thread to cancel the exact synchronous operation, while runtime
//! teardown is never coupled to either thread.

use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

const CANCEL_NOT_PENDING_RETRY: Duration = Duration::from_millis(1);

/// Result of asking a drain target to cancel its worker's synchronous I/O.
pub(crate) enum CancelAttempt {
    /// The pending operation was marked for cancellation.
    Requested,
    /// The worker has not entered the operation yet, or just completed it.
    NotPending,
}

/// One synchronous drain operation and its precise cancellation target.
pub(crate) trait DrainTarget: Send + Sync + 'static {
    /// Binds cancellation to the dedicated thread that is about to call `drain`.
    fn bind_to_current_thread(&self) -> io::Result<()>;

    /// Performs the peer-controlled synchronous drain.
    fn drain(&self) -> io::Result<()>;

    /// Cancels only the synchronous I/O issued by the bound worker thread.
    fn cancel(&self) -> io::Result<CancelAttempt>;

    /// Orders the mock worker and deadline without relying on thread scheduling.
    #[cfg(test)]
    fn before_deadline(&self) {}
}

/// Observable result of a drain whose waiter remains alive.
#[derive(Debug)]
pub(crate) enum BoundedDrainOutcome {
    /// The synchronous operation completed before its deadline.
    Completed(io::Result<()>),
    /// The deadline won and operation-local cancellation was requested.
    TimedOut,
}

/// Runs a synchronous drain outside Tokio under an operation-local deadline.
pub(crate) async fn drain_with_deadline<T>(
    target: Arc<T>,
    timeout: Duration,
) -> io::Result<BoundedDrainOutcome>
where
    T: DrainTarget,
{
    DrainWait::start(target, timeout)?.wait().await
}

struct DrainWait {
    shared: Arc<Shared>,
    outcome: oneshot::Receiver<BoundedDrainOutcome>,
    observed: bool,
}

impl DrainWait {
    fn start<T>(target: Arc<T>, timeout: Duration) -> io::Result<Self>
    where
        T: DrainTarget,
    {
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "transport drain deadline overflow",
            )
        })?;
        let (outcome_tx, outcome) = oneshot::channel();
        let shared = Arc::new(Shared::new(outcome_tx));

        let worker_shared = Arc::clone(&shared);
        let worker_target = Arc::clone(&target);
        let worker = std::thread::Builder::new()
            .name("rmux-pipe-drain".to_owned())
            .spawn(move || run_worker(worker_target, &worker_shared))?;
        drop(worker);

        let deadline_shared = Arc::clone(&shared);
        let deadline_target = Arc::clone(&target);
        let deadline_worker = match std::thread::Builder::new()
            .name("rmux-pipe-drain-cancel".to_owned())
            .spawn(move || run_deadline(deadline_target, &deadline_shared, deadline))
        {
            Ok(worker) => worker,
            Err(error) => {
                shared.request_abandon();
                shared.arm();
                return Err(error);
            }
        };
        drop(deadline_worker);

        // The drain worker cannot enter the synchronous call until both it and
        // its cancellation owner exist. This makes thread-spawn failure safe.
        shared.arm();
        Ok(Self {
            shared,
            outcome,
            observed: false,
        })
    }

    async fn wait(mut self) -> io::Result<BoundedDrainOutcome> {
        let outcome = (&mut self.outcome).await.map_err(|_| {
            io::Error::other("transport drain worker ended without reporting an outcome")
        })?;
        self.observed = true;
        Ok(outcome)
    }
}

impl Drop for DrainWait {
    fn drop(&mut self) {
        if !self.observed {
            // Connection-task cancellation reaches this guard. The drain is on
            // an OS thread, so this request neither blocks the dropping task nor
            // leaves a Tokio blocking worker for Runtime::drop to join.
            self.shared.request_abandon();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Waiting,
    Running,
    Complete,
}

#[derive(Clone, Copy)]
enum Cancellation {
    Deadline,
    Abandoned,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

struct State {
    armed: bool,
    phase: Phase,
    cancellation: Option<Cancellation>,
    outcome: Option<oneshot::Sender<BoundedDrainOutcome>>,
}

impl Shared {
    fn new(outcome: oneshot::Sender<BoundedDrainOutcome>) -> Self {
        Self {
            state: Mutex::new(State {
                armed: false,
                phase: Phase::Waiting,
                cancellation: None,
                outcome: Some(outcome),
            }),
            wake: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn arm(&self) {
        self.lock().armed = true;
        self.wake.notify_all();
    }

    fn begin_worker(&self) -> bool {
        let mut state = self.lock();
        while !state.armed && state.cancellation.is_none() {
            state = self
                .wake
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if state.cancellation.is_some() {
            state.phase = Phase::Complete;
            drop(state);
            self.wake.notify_all();
            return false;
        }
        state.phase = Phase::Running;
        drop(state);
        self.wake.notify_all();
        true
    }

    fn complete(&self, result: io::Result<()>) {
        let sender = {
            let mut state = self.lock();
            state.phase = Phase::Complete;
            if state.cancellation.is_none() {
                state.outcome.take()
            } else {
                None
            }
        };
        self.wake.notify_all();
        if let Some(sender) = sender {
            let _ = sender.send(BoundedDrainOutcome::Completed(result));
        }
    }

    fn request_abandon(&self) {
        let mut state = self.lock();
        if state.phase != Phase::Complete && state.cancellation.is_none() {
            state.cancellation = Some(Cancellation::Abandoned);
        }
        drop(state);
        self.wake.notify_all();
    }
}

fn run_worker<T>(target: Arc<T>, shared: &Shared)
where
    T: DrainTarget,
{
    if let Err(error) = target.bind_to_current_thread() {
        shared.complete(Err(error));
        return;
    }
    if !shared.begin_worker() {
        return;
    }
    shared.complete(target.drain());
}

fn run_deadline<T>(target: Arc<T>, shared: &Shared, deadline: Instant)
where
    T: DrainTarget,
{
    #[cfg(test)]
    target.before_deadline();

    let sender = {
        let mut state = shared.lock();
        loop {
            if state.phase == Phase::Complete {
                return;
            }
            if state.cancellation.is_some() {
                break None;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.cancellation = Some(Cancellation::Deadline);
                break state.outcome.take();
            }
            state = shared
                .wake
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    };

    // Publish the strict async bound before making the cancellation call. Even
    // a broken platform primitive can therefore strand only these dedicated OS
    // threads, never the connection task or Tokio runtime teardown.
    if let Some(sender) = sender {
        let _ = sender.send(BoundedDrainOutcome::TimedOut);
    }
    shared.wake.notify_all();

    loop {
        let phase = shared.lock().phase;
        match phase {
            Phase::Waiting | Phase::Complete => return,
            Phase::Running => {}
        }

        match target.cancel() {
            Ok(CancelAttempt::Requested) => return,
            Ok(CancelAttempt::NotPending) => {
                // The worker marks itself Running immediately before entering
                // the syscall. Bridge that unavoidable instruction-level race
                // without broadening cancellation to the pipe's overlapped I/O.
                let state = shared.lock();
                if state.phase == Phase::Complete {
                    return;
                }
                drop(
                    shared
                        .wake
                        .wait_timeout(state, CANCEL_NOT_PENDING_RETRY)
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "failed to cancel a dedicated control transport drain"
                );
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread::ThreadId;

    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    #[derive(Clone, Copy)]
    enum CancellationOrder {
        Unconstrained,
        BeforeEntry,
        AfterEntry,
    }

    struct PeerControlledDrain {
        state: Mutex<PeerState>,
        wake: Condvar,
        cancel_calls: AtomicUsize,
        entry_delay: Duration,
        cancellation_releases: bool,
        cancellation_order: CancellationOrder,
    }

    #[derive(Default)]
    struct PeerState {
        worker: Option<ThreadId>,
        starting: bool,
        draining: bool,
        saw_not_pending: bool,
        consumed: bool,
        cancelled: bool,
        complete: bool,
    }

    impl PeerControlledDrain {
        fn new(entry_delay: Duration, cancellation_releases: bool) -> Arc<Self> {
            Self::with_cancellation_order(
                entry_delay,
                cancellation_releases,
                CancellationOrder::Unconstrained,
            )
        }

        fn with_cancellation_order(
            entry_delay: Duration,
            cancellation_releases: bool,
            cancellation_order: CancellationOrder,
        ) -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(PeerState::default()),
                wake: Condvar::new(),
                cancel_calls: AtomicUsize::new(0),
                entry_delay,
                cancellation_releases,
                cancellation_order,
            })
        }

        fn lock(&self) -> MutexGuard<'_, PeerState> {
            self.state.lock().expect("peer-controlled drain state")
        }

        fn consume(&self) {
            self.lock().consumed = true;
            self.wake.notify_all();
        }

        fn wait_for(&self, predicate: impl Fn(&PeerState) -> bool) {
            let deadline = Instant::now() + TEST_TIMEOUT;
            let mut state = self.lock();
            while !predicate(&state) {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "drain state did not advance in time");
                state = self
                    .wake
                    .wait_timeout(state, remaining)
                    .expect("peer-controlled drain wait")
                    .0;
            }
        }
    }

    impl DrainTarget for PeerControlledDrain {
        fn before_deadline(&self) {
            match self.cancellation_order {
                CancellationOrder::Unconstrained => {}
                CancellationOrder::BeforeEntry => self.wait_for(|state| state.starting),
                CancellationOrder::AfterEntry => self.wait_for(|state| state.draining),
            }
        }

        fn bind_to_current_thread(&self) -> io::Result<()> {
            self.lock().worker = Some(std::thread::current().id());
            self.wake.notify_all();
            Ok(())
        }

        fn drain(&self) -> io::Result<()> {
            // run_worker has already committed Phase::Running at this point.
            self.lock().starting = true;
            self.wake.notify_all();
            std::thread::sleep(self.entry_delay);
            if matches!(self.cancellation_order, CancellationOrder::BeforeEntry) {
                self.wait_for(|state| state.saw_not_pending);
            }
            let mut state = self.lock();
            state.draining = true;
            self.wake.notify_all();
            while !(state.consumed || state.cancelled && self.cancellation_releases) {
                state = self.wake.wait(state).expect("peer-controlled drain wait");
            }
            let cancelled = state.cancelled && !state.consumed;
            state.complete = true;
            drop(state);
            self.wake.notify_all();
            if cancelled {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                Ok(())
            }
        }

        fn cancel(&self) -> io::Result<CancelAttempt> {
            self.cancel_calls.fetch_add(1, Ordering::SeqCst);
            let mut state = self.lock();
            if !state.draining {
                state.saw_not_pending = true;
                self.wake.notify_all();
                return Ok(CancelAttempt::NotPending);
            }
            state.cancelled = true;
            drop(state);
            self.wake.notify_all();
            Ok(CancelAttempt::Requested)
        }
    }

    #[tokio::test]
    async fn a_consumed_drain_completes_without_cancellation() {
        let target = PeerControlledDrain::new(Duration::ZERO, true);
        let peer = Arc::clone(&target);
        let consumer = std::thread::spawn(move || {
            peer.wait_for(|state| state.draining);
            peer.consume();
        });

        let outcome = tokio::time::timeout(
            TEST_TIMEOUT,
            drain_with_deadline(Arc::clone(&target), TEST_TIMEOUT),
        )
        .await
        .expect("the consumed drain must finish")
        .expect("the drain threads must start");
        assert!(matches!(outcome, BoundedDrainOutcome::Completed(Ok(()))));
        assert_eq!(target.cancel_calls.load(Ordering::SeqCst), 0);
        consumer.join().expect("consumer thread");
    }

    #[tokio::test]
    async fn deadline_cancellation_bridges_the_worker_start_race() {
        // Running must precede the deadline, and the first cancellation must
        // observe NotPending. The 10/40 ms delays alone impose neither order.
        let target = PeerControlledDrain::with_cancellation_order(
            Duration::from_millis(40),
            true,
            CancellationOrder::BeforeEntry,
        );
        let outcome = tokio::time::timeout(
            TEST_TIMEOUT,
            drain_with_deadline(Arc::clone(&target), Duration::from_millis(10)),
        )
        .await
        .expect("the deadline must be observable")
        .expect("the drain threads must start");
        assert!(matches!(outcome, BoundedDrainOutcome::TimedOut));

        target.wait_for(|state| state.complete);
        assert!(target.lock().cancelled);
        assert!(target.lock().saw_not_pending);
        assert!(
            target.cancel_calls.load(Ordering::SeqCst) > 1,
            "ERROR_NOT_FOUND-shaped start races must be retried"
        );
    }

    #[tokio::test]
    async fn deadline_cancellation_after_worker_entry_needs_only_one_request() {
        let target = PeerControlledDrain::with_cancellation_order(
            Duration::from_millis(40),
            true,
            CancellationOrder::AfterEntry,
        );
        let outcome = tokio::time::timeout(
            TEST_TIMEOUT,
            drain_with_deadline(Arc::clone(&target), Duration::from_millis(10)),
        )
        .await
        .expect("the deadline must be observable")
        .expect("the drain threads must start");
        assert!(matches!(outcome, BoundedDrainOutcome::TimedOut));

        target.wait_for(|state| state.complete);
        assert!(target.lock().cancelled);
        assert!(!target.lock().saw_not_pending);
        assert_eq!(target.cancel_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_the_waiter_cancels_only_its_dedicated_worker() {
        let target = PeerControlledDrain::new(Duration::ZERO, true);
        let task = tokio::spawn(drain_with_deadline(
            Arc::clone(&target),
            Duration::from_secs(3_600),
        ));
        tokio::task::block_in_place(|| target.wait_for(|state| state.draining));

        task.abort();
        assert!(task
            .await
            .expect_err("the waiter must be cancelled")
            .is_cancelled());
        tokio::task::block_in_place(|| target.wait_for(|state| state.complete));
        let state = target.lock();
        assert!(state.cancelled);
        assert_ne!(state.worker, Some(std::thread::current().id()));
    }

    #[test]
    fn runtime_drop_never_waits_for_the_dedicated_drain_thread() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("test runtime");
        let target = PeerControlledDrain::new(Duration::ZERO, false);
        runtime.block_on(async {
            let task = tokio::spawn(drain_with_deadline(
                Arc::clone(&target),
                Duration::from_secs(3_600),
            ));
            tokio::task::block_in_place(|| target.wait_for(|state| state.draining));
            task.abort();
            assert!(task
                .await
                .expect_err("the waiter must be cancelled")
                .is_cancelled());
        });

        let started = Instant::now();
        drop(runtime);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "Tokio must not own or join the synchronous drain worker"
        );

        // This mock deliberately ignores cancellation until its peer releases
        // it. Clean it up after proving runtime independence.
        target.consume();
        target.wait_for(|state| state.complete);
    }
}
