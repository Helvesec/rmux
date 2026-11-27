use std::io;
use std::path::Path;
#[cfg(unix)]
use std::time::SystemTime;
use std::time::{Duration, Instant};

use crate::connection::{connect_or_absent_with_timeout, ConnectResult};
use crate::ClientError;

// Shutdown may spend five seconds bounding a foreground shell, five seconds
// waiting for the next accepted lifecycle hook, and two seconds joining
// background tasks. Leave additional margin for endpoint cleanup and scheduler
// contention. A stopping server that still owns a socket file touches its
// mtime each time a lifecycle hook finishes, and that restarts this bound: a
// long drain that advances is waited for, a stuck one is still reported.
const SERVER_ENDPOINT_CLEANUP_TIMEOUT: Duration = Duration::from_secs(15);
// One poll accounts for at most this much of the socket-file wait: a stretch
// in which this client could not run (stopped, starved) is not time the server
// spent without progress, as the server counts its own hook grace.
#[cfg(unix)]
const SERVER_ENDPOINT_CLEANUP_MAX_STEP: Duration = Duration::from_millis(100);
const SERVER_ENDPOINT_PROBE_TIMEOUT: Duration = Duration::from_millis(50);
const SERVER_ENDPOINT_CLEANUP_MIN_POLL: Duration = Duration::from_millis(1);
const SERVER_ENDPOINT_CLEANUP_MAX_POLL: Duration = Duration::from_millis(10);

/// Waits until a stopped RMUX server has released its local endpoint.
///
/// Filesystem Unix sockets are considered released only after their pathname
/// is removed. Linux abstract sockets and Windows named pipes are probed with
/// bounded connections. A deadline is reported as an error rather than being
/// mistaken for a successful `kill-server`.
pub fn wait_for_server_endpoint_cleanup(endpoint: &Path) -> Result<(), ClientError> {
    wait_for_server_endpoint_cleanup_with(
        endpoint,
        SERVER_ENDPOINT_CLEANUP_TIMEOUT,
        SERVER_ENDPOINT_PROBE_TIMEOUT,
    )
}

fn wait_for_server_endpoint_cleanup_with(
    endpoint: &Path,
    cleanup_timeout: Duration,
    probe_timeout: Duration,
) -> Result<(), ClientError> {
    #[cfg(unix)]
    if !endpoint.as_os_str().is_empty() {
        return wait_for_filesystem_endpoint_cleanup(endpoint, cleanup_timeout);
    }

    let deadline = Instant::now() + cleanup_timeout;
    let mut next_poll = SERVER_ENDPOINT_CLEANUP_MIN_POLL;
    loop {
        match connect_or_absent_with_timeout(endpoint, probe_timeout) {
            Ok(ConnectResult::Absent) => return Ok(()),
            Ok(ConnectResult::Connected(connection)) => drop(connection),
            Err(error) if shutdown_probe_error_is_transient(&error) => {}
            Err(error) => return Err(error),
        }

        let now = Instant::now();
        if now >= deadline {
            return Err(cleanup_timeout_error(endpoint));
        }
        std::thread::sleep(next_poll.min(deadline.saturating_duration_since(now)));
        next_poll = (next_poll + next_poll).min(SERVER_ENDPOINT_CLEANUP_MAX_POLL);
    }
}

#[cfg(unix)]
fn wait_for_filesystem_endpoint_cleanup(
    endpoint: &Path,
    cleanup_timeout: Duration,
) -> Result<(), ClientError> {
    let mut wait = FilesystemCleanupWait::new(cleanup_timeout, Instant::now());
    loop {
        let observation = observe_filesystem_endpoint(endpoint).map_err(ClientError::Io)?;
        match wait.step(Instant::now(), observation) {
            CleanupStep::Released => return Ok(()),
            CleanupStep::TimedOut => return Err(cleanup_timeout_error(endpoint)),
            CleanupStep::PollAfter(delay) => std::thread::sleep(delay),
        }
    }
}

/// What one poll saw at a filesystem endpoint path.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointObservation {
    /// The pathname is gone: the server released its endpoint.
    Released,
    /// The pathname still exists; `progress` is its mtime when the platform reports one.
    Present { progress: Option<SystemTime> },
}

#[cfg(unix)]
fn observe_filesystem_endpoint(endpoint: &Path) -> io::Result<EndpointObservation> {
    match std::fs::metadata(endpoint) {
        Ok(metadata) => Ok(EndpointObservation::Present {
            progress: metadata.modified().ok(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(EndpointObservation::Released),
        Err(error) => Err(error),
    }
}

/// The next action of a socket-file wait after one observation.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupStep {
    Released,
    TimedOut,
    PollAfter(Duration),
}

/// The socket-file wait as a pure state machine over observations and the instants they were
/// taken at, so its bound is decided by what the client saw, not by the host's clock or disk.
#[cfg(unix)]
struct FilesystemCleanupWait {
    timeout: Duration,
    wait: ObservedWait,
    last_progress: Option<SystemTime>,
    next_poll: Duration,
}

#[cfg(unix)]
impl FilesystemCleanupWait {
    fn new(timeout: Duration, now: Instant) -> Self {
        Self {
            timeout,
            wait: ObservedWait::new(now),
            last_progress: None,
            next_poll: SERVER_ENDPOINT_CLEANUP_MIN_POLL,
        }
    }

    fn step(&mut self, now: Instant, observation: EndpointObservation) -> CleanupStep {
        let progress = match observation {
            EndpointObservation::Released => return CleanupStep::Released,
            EndpointObservation::Present { progress } => progress,
        };
        self.wait.observe(now);
        if progress.is_some() && self.last_progress.is_some() && progress != self.last_progress {
            self.wait.restart();
        }
        self.last_progress = progress;
        let remaining = self.timeout.saturating_sub(self.wait.waited);
        if remaining.is_zero() {
            return CleanupStep::TimedOut;
        }
        let delay = self.next_poll.min(remaining);
        self.next_poll = (self.next_poll + self.next_poll).min(SERVER_ENDPOINT_CLEANUP_MAX_POLL);
        CleanupStep::PollAfter(delay)
    }
}

/// The time this client observed waiting since the server last showed progress.
#[cfg(unix)]
struct ObservedWait {
    waited: Duration,
    last_check: Instant,
}

#[cfg(unix)]
impl ObservedWait {
    fn new(now: Instant) -> Self {
        Self {
            waited: Duration::ZERO,
            last_check: now,
        }
    }

    fn observe(&mut self, now: Instant) {
        self.waited += now
            .saturating_duration_since(self.last_check)
            .min(SERVER_ENDPOINT_CLEANUP_MAX_STEP);
        self.last_check = now;
    }

    fn restart(&mut self) {
        self.waited = Duration::ZERO;
    }
}

fn cleanup_timeout_error(endpoint: &Path) -> ClientError {
    ClientError::Io(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "kill-server timed out waiting for the daemon endpoint '{}' to close",
            endpoint.display()
        ),
    ))
}

fn shutdown_probe_error_is_transient(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionReset
            )
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::{
        observe_filesystem_endpoint, wait_for_server_endpoint_cleanup,
        wait_for_server_endpoint_cleanup_with, CleanupStep, EndpointObservation,
        FilesystemCleanupWait, ObservedWait, SERVER_ENDPOINT_CLEANUP_MAX_POLL,
        SERVER_ENDPOINT_CLEANUP_MAX_STEP, SERVER_ENDPOINT_CLEANUP_TIMEOUT,
        SERVER_ENDPOINT_PROBE_TIMEOUT,
    };
    use crate::ClientError;
    use std::path::PathBuf;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    #[test]
    fn filesystem_endpoint_cleanup_waits_for_path_removal() {
        let endpoint = unique_endpoint("wait");
        std::fs::write(&endpoint, b"occupied").expect("create endpoint marker");
        let removed_endpoint = endpoint.clone();
        let remover = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            std::fs::remove_file(removed_endpoint).expect("remove endpoint marker");
        });

        // Bounded by the published deadline, not a test-only one: a disk slow to unlink is
        // waited for as `kill-server` waits for it.
        let started = Instant::now();
        wait_for_server_endpoint_cleanup(&endpoint).expect("endpoint cleanup");
        remover.join().expect("endpoint remover");

        assert!(
            started.elapsed() >= Duration::from_millis(30),
            "cleanup returned before the endpoint path was removed"
        );
    }

    #[test]
    fn filesystem_endpoint_cleanup_waits_while_the_server_shows_progress() {
        // The drain touches its socket file every 40 ms and outlasts the bound threefold. Time is
        // virtual: a disk that delays the touches cannot turn this drain into a stuck one.
        let bound = Duration::from_millis(500);
        let drain = Duration::from_millis(1_500);
        let (step, elapsed) = drive_wait(bound, |elapsed| {
            if elapsed >= drain {
                EndpointObservation::Released
            } else {
                EndpointObservation::Present {
                    progress: touched_every(TOUCH_INTERVAL, elapsed),
                }
            }
        });

        assert_eq!(
            step,
            CleanupStep::Released,
            "a drain that keeps progressing is waited for"
        );
        assert!(
            elapsed >= drain,
            "cleanup returned before the endpoint path was removed"
        );
    }

    #[test]
    fn filesystem_endpoint_cleanup_reports_a_drain_that_stops_progressing() {
        // The server shows progress for 300 ms, then never touches nor removes its socket file.
        let bound = Duration::from_millis(500);
        let stuck_after = Duration::from_millis(300);
        let (step, elapsed) = drive_wait(bound, |elapsed| EndpointObservation::Present {
            progress: touched_every(TOUCH_INTERVAL, elapsed.min(stuck_after)),
        });

        assert_eq!(step, CleanupStep::TimedOut, "a stuck drain is reported");
        assert!(
            elapsed > bound && elapsed <= stuck_after + bound,
            "a stuck drain is reported one bound after its last progress, not at {elapsed:?}"
        );
    }

    #[test]
    fn filesystem_endpoint_observation_reads_progress_and_removal_from_the_path() {
        let endpoint = unique_endpoint("observe");
        std::fs::write(&endpoint, b"occupied").expect("create endpoint marker");
        let marker = std::fs::File::options()
            .write(true)
            .open(&endpoint)
            .expect("open endpoint marker");

        for second in [1_001_u64, 1_002] {
            let touched = UNIX_EPOCH + Duration::from_secs(second);
            marker.set_modified(touched).expect("show drain progress");
            assert_eq!(
                observe_filesystem_endpoint(&endpoint).expect("observe present endpoint"),
                EndpointObservation::Present {
                    progress: Some(touched)
                }
            );
        }
        drop(marker);
        std::fs::remove_file(&endpoint).expect("remove endpoint marker");

        assert_eq!(
            observe_filesystem_endpoint(&endpoint).expect("observe removed endpoint"),
            EndpointObservation::Released
        );
    }

    #[test]
    fn a_stretch_in_which_the_client_could_not_run_counts_as_one_step() {
        let started = Instant::now();
        let mut wait = ObservedWait::new(started);

        wait.observe(started + Duration::from_millis(10));
        assert_eq!(wait.waited, Duration::from_millis(10));
        wait.observe(started + Duration::from_secs(30));
        assert_eq!(
            wait.waited,
            Duration::from_millis(10) + SERVER_ENDPOINT_CLEANUP_MAX_STEP
        );
        wait.restart();
        assert_eq!(wait.waited, Duration::ZERO);
    }

    #[test]
    fn filesystem_endpoint_cleanup_timeout_is_an_error() {
        let endpoint = unique_endpoint("timeout");
        std::fs::write(&endpoint, b"occupied").expect("create endpoint marker");

        // The real wait runs on its own thread, so a wait that never reaches its deadline fails
        // this test within the watchdog instead of hanging the test binary.
        let bound = Duration::from_millis(20);
        let (done, outcome) = mpsc::channel();
        let waited_endpoint = endpoint.clone();
        let waiter = std::thread::spawn(move || {
            let _ = done.send(wait_for_server_endpoint_cleanup_with(
                &waited_endpoint,
                bound,
                SERVER_ENDPOINT_PROBE_TIMEOUT,
            ));
        });
        let outcome = outcome.recv_timeout(REAL_WAIT_WATCHDOG);
        // Removing the path also lets a wait that missed its deadline end as released.
        let _ = std::fs::remove_file(&endpoint);
        let error = match outcome {
            Ok(result) => {
                waiter.join().expect("endpoint waiter");
                result.expect_err("present endpoint must time out")
            }
            Err(RecvTimeoutError::Timeout) => {
                panic!("the wait for a {bound:?} bound did not end within {REAL_WAIT_WATCHDOG:?}")
            }
            Err(RecvTimeoutError::Disconnected) => panic!("the endpoint waiter panicked"),
        };

        assert!(
            matches!(
                error,
                ClientError::Io(ref io_error)
                    if io_error.kind() == std::io::ErrorKind::TimedOut
                        && io_error.to_string().contains(&endpoint.display().to_string())
            ),
            "unexpected cleanup error: {error}"
        );
    }

    #[test]
    fn filesystem_endpoint_cleanup_reports_a_stuck_server_at_the_published_deadline() {
        // `kill-server` budgets 5 s for a foreground shell, 5 s for a lifecycle hook and 2 s for
        // background tasks, plus margin: a server that never shows progress is reported at 15 s.
        let published = Duration::from_secs(15);
        let (step, elapsed) = drive_wait(SERVER_ENDPOINT_CLEANUP_TIMEOUT, |_| {
            EndpointObservation::Present {
                progress: touched_every(TOUCH_INTERVAL, Duration::ZERO),
            }
        });

        assert_eq!(step, CleanupStep::TimedOut, "a stuck server is reported");
        assert!(
            elapsed >= published && elapsed <= published + SERVER_ENDPOINT_CLEANUP_MAX_POLL,
            "a stuck server is reported at the published {published:?} deadline, not at {elapsed:?}"
        );
    }

    const TOUCH_INTERVAL: Duration = Duration::from_millis(40);
    /// A real wait for a bound of milliseconds that has not ended after this long is reported as
    /// hung, far above any scheduling delay of a loaded host.
    const REAL_WAIT_WATCHDOG: Duration = Duration::from_secs(5);
    /// A driven wait that has not ended after this much virtual time fails instead of hanging.
    const VIRTUAL_WAIT_LIMIT: Duration = Duration::from_secs(60);

    /// Drives the socket-file wait against `server_at`, which says what the endpoint path shows
    /// at a virtual time. No real clock, sleep or disk is involved. Returns the final step and
    /// the virtual time it was reached at.
    fn drive_wait(
        timeout: Duration,
        server_at: impl Fn(Duration) -> EndpointObservation,
    ) -> (CleanupStep, Duration) {
        let started = Instant::now();
        let mut wait = FilesystemCleanupWait::new(timeout, started);
        let mut elapsed = Duration::ZERO;
        loop {
            match wait.step(started + elapsed, server_at(elapsed)) {
                CleanupStep::PollAfter(delay) => {
                    assert!(!delay.is_zero(), "a poll must advance the wait");
                    elapsed += delay;
                    assert!(
                        elapsed <= VIRTUAL_WAIT_LIMIT,
                        "the wait for a {timeout:?} bound never ended"
                    );
                }
                final_step => return (final_step, elapsed),
            }
        }
    }

    /// The mtime of a socket file that its server touches every `interval`.
    fn touched_every(interval: Duration, elapsed: Duration) -> Option<SystemTime> {
        let touches = u64::try_from(elapsed.as_nanos() / interval.as_nanos()).expect("touch count");
        Some(UNIX_EPOCH + Duration::from_secs(1_000 + touches))
    }

    fn unique_endpoint(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rmux-endpoint-cleanup-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after epoch")
                .as_nanos()
        ))
    }
}
