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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
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
        for worker in workers {
            worker.join().expect("worker completes");
        }

        assert_eq!(peak.load(Ordering::SeqCst), 1);
    }
}
