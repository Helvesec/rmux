use std::io;
use std::sync::Mutex;

/// Serializes the short Win32 console operations which must never overlap an
/// attach input read with a temporary input-mode transition.
///
/// `ENABLE_VIRTUAL_TERMINAL_INPUT` changes how conhost exposes output DECSET
/// sequences. Keeping the mode change, output write, and exact restoration in
/// one critical section prevents the input thread from observing that temporary
/// mode. Raw-mode transitions use the same coordinator for the same reason.
#[derive(Debug)]
pub(super) struct ConsoleIoCoordinator {
    lock: Mutex<()>,
}

impl ConsoleIoCoordinator {
    pub(super) const fn new() -> Self {
        Self {
            lock: Mutex::new(()),
        }
    }

    /// Returns the callback's value (including an `Err`) inside `Ok`. An
    /// unwinding callback poisons the coordinator: waiting and later callers
    /// receive an I/O error without running their operations or clearing poison.
    /// Console mode may be uncertain after a panic, so this owner does not recover.
    pub(super) fn synchronized<T>(&self, operation: impl FnOnce() -> T) -> io::Result<T> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("Windows console coordinator poisoned"))?;
        Ok(operation())
    }
}

pub(super) static ATTACH_CONSOLE_IO: ConsoleIoCoordinator = ConsoleIoCoordinator::new();

#[cfg(test)]
mod tests {
    use std::io;
    use std::panic::{catch_unwind, panic_any};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Barrier, TryLockError};
    use std::thread;
    use std::time::Duration;

    use super::ConsoleIoCoordinator;

    #[test]
    fn coordinator_never_runs_console_operations_concurrently() {
        let coordinator = Arc::new(ConsoleIoCoordinator::new());
        let start = Arc::new(Barrier::new(3));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let workers = (0..2)
            .map(|_| {
                let coordinator = Arc::clone(&coordinator);
                let start = Arc::clone(&start);
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                thread::spawn(move || {
                    start.wait();
                    coordinator
                        .synchronized(|| {
                            assert!(matches!(
                                coordinator.lock.try_lock(),
                                Err(TryLockError::WouldBlock)
                            ));
                            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(now, Ordering::SeqCst);
                            thread::sleep(Duration::from_millis(20));
                            active.fetch_sub(1, Ordering::SeqCst);
                        })
                        .expect("coordinator remains usable");
                })
            })
            .collect::<Vec<_>>();

        start.wait();
        let results = workers
            .into_iter()
            .map(thread::JoinHandle::join)
            .collect::<Vec<_>>();
        for result in results {
            result.expect("worker completes");
        }

        assert_eq!(peak.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn callback_values_and_errors_do_not_poison_the_coordinator() {
        let coordinator = ConsoleIoCoordinator::new();
        let mut calls = 0;
        let value = String::from("console result");
        assert_eq!(
            coordinator
                .synchronized(|| {
                    calls += 1;
                    value
                })
                .expect("acquisition succeeds"),
            "console result"
        );
        let error = coordinator
            .synchronized(|| {
                calls += 1;
                Err::<(), _>(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "operation error",
                ))
            })
            .expect("callback error is not an acquisition error")
            .expect_err("callback error is preserved");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "operation error");
        assert!(!coordinator.lock.is_poisoned());
        assert_eq!(
            coordinator
                .synchronized(|| {
                    calls += 1;
                    42
                })
                .expect("usable after callback error"),
            42
        );
        assert_eq!(calls, 3);
    }

    fn assert_poison_error(error: io::Error) {
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(error.to_string(), "Windows console coordinator poisoned");
    }

    #[test]
    fn callback_unwind_poisons_every_later_acquisition_without_running_it() {
        let coordinator = ConsoleIoCoordinator::new();
        let panic = catch_unwind(|| {
            let _ = coordinator.synchronized(|| panic_any("console operation panic"));
        })
        .expect_err("callback panic must escape");
        assert_eq!(
            panic.downcast_ref::<&str>(),
            Some(&"console operation panic")
        );

        let mut calls = 0;
        for _ in 0..3 {
            assert!(coordinator.lock.is_poisoned());
            let result = coordinator.synchronized(|| calls += 1);
            assert_eq!(calls, 0, "poisoned acquisition must skip the callback");
            assert_poison_error(result.expect_err("poison remains an acquisition error"));
            assert!(
                coordinator.lock.is_poisoned(),
                "acquisition must not clear poison"
            );
        }
    }

    #[test]
    fn waiting_caller_observes_poison_after_owner_unwinds() {
        let coordinator = ConsoleIoCoordinator::new();
        let calls = AtomicUsize::new(0);
        let (held_tx, held_rx) = mpsc::channel();
        let (waiting_tx, waiting_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let timeout = Duration::from_secs(5);

        thread::scope(|scope| {
            let coordinator = &coordinator;
            let calls = &calls;
            let owner = scope.spawn(move || {
                coordinator.synchronized(|| {
                    held_tx.send(()).expect("announce held lock");
                    // Disconnection also releases the owner if the test fails.
                    let _ = release_rx.recv_timeout(timeout);
                    panic_any("owner operation panic");
                })
            });
            let waiter = scope.spawn(move || {
                held_rx
                    .recv_timeout(timeout)
                    .expect("owner entered callback");
                // Positive contention evidence; a timed absence of a result
                // would not prove that this thread had reached the lock.
                let contended =
                    matches!(coordinator.lock.try_lock(), Err(TryLockError::WouldBlock));
                waiting_tx.send(contended).expect("announce contention");
                coordinator.synchronized(|| calls.fetch_add(1, Ordering::SeqCst))
            });

            let contended = waiting_rx.recv_timeout(timeout);
            // Release before asserting, then join BOTH workers even on failure.
            // Every channel wait is bounded and no test thread retains a guard.
            drop(release_tx);
            let owner_result = owner.join();
            let waiter_result = waiter.join();

            assert!(contended.expect("waiter reported contention"));
            let panic = owner_result.expect_err("owner must unwind");
            assert_eq!(panic.downcast_ref::<&str>(), Some(&"owner operation panic"));
            assert_poison_error(
                waiter_result
                    .expect("waiter must not panic")
                    .expect_err("poisoned lock"),
            );
        });
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(coordinator.lock.is_poisoned());
    }
}
