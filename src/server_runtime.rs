//! Tokio runtime policy for long-lived RMUX daemon entrypoints.

use std::io;
use std::num::NonZeroUsize;
use std::time::Duration;

use tokio::runtime::{Builder, Runtime};

// Command-queue/source-file dispatch has a large debug future. A 2 MiB worker
// stack can abort the daemon before a valid set-option request reaches an
// await; use the same bounded stack budget as the release test workers.
const DAEMON_WORKER_THREAD_STACK_SIZE: usize = 8 * 1024 * 1024;
const DAEMON_MIN_WORKER_THREADS: usize = 1;
const DAEMON_MAX_WORKER_THREADS: usize = 1;
const DAEMON_MAX_BLOCKING_THREADS: usize = 128;
const DAEMON_BLOCKING_THREAD_KEEP_ALIVE: Duration = Duration::from_secs(2);

/// Builds the runtime used by daemon entrypoints.
///
/// Pane readers, IPC handlers, attach forwarding, and web-share tasks all run on
/// this runtime. Keep the scheduler small: pane readers are mostly readiness
/// driven, while render and status work is already coalesced. More workers
/// increase cross-thread wakeups and idle RSS on the common local daemon path.
pub(crate) fn build_daemon_runtime() -> io::Result<Runtime> {
    Builder::new_multi_thread()
        .worker_threads(daemon_worker_threads())
        .thread_stack_size(DAEMON_WORKER_THREAD_STACK_SIZE)
        .max_blocking_threads(DAEMON_MAX_BLOCKING_THREADS)
        .thread_keep_alive(DAEMON_BLOCKING_THREAD_KEEP_ALIVE)
        .enable_io()
        .enable_time()
        .build()
}

fn daemon_worker_threads() -> usize {
    std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(DAEMON_MAX_WORKER_THREADS)
        .clamp(DAEMON_MIN_WORKER_THREADS, DAEMON_MAX_WORKER_THREADS)
}

#[cfg(test)]
mod tests {
    use tokio::runtime::RuntimeFlavor;

    #[test]
    fn daemon_runtime_uses_multi_thread_scheduler() {
        let runtime = super::build_daemon_runtime().expect("daemon runtime should build");
        let flavor = runtime.block_on(async { tokio::runtime::Handle::current().runtime_flavor() });

        assert!(matches!(flavor, RuntimeFlavor::MultiThread));
    }

    #[test]
    fn daemon_runtime_worker_count_is_bounded() {
        let workers = super::daemon_worker_threads();

        assert!(
            (super::DAEMON_MIN_WORKER_THREADS..=super::DAEMON_MAX_WORKER_THREADS)
                .contains(&workers)
        );
    }

    #[test]
    fn daemon_runtime_resource_limits_are_intentional() {
        assert_eq!(super::DAEMON_WORKER_THREAD_STACK_SIZE, 8 * 1024 * 1024);
        assert_eq!(super::DAEMON_MIN_WORKER_THREADS, 1);
        assert_eq!(super::DAEMON_MAX_WORKER_THREADS, 1);
        assert_eq!(super::DAEMON_MAX_BLOCKING_THREADS, 128);
        assert_eq!(
            super::DAEMON_BLOCKING_THREAD_KEEP_ALIVE,
            std::time::Duration::from_secs(2)
        );
    }
}
