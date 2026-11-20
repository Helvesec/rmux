#![cfg(unix)]

//! A live daemon whose accept queue is full is busy, not stale.
//!
//! Startup must neither unlink its socket nor launch a second daemon beside
//! it: it joins the daemon once it accepts, or reports a recoverable timeout.

use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use rmux_bootstrap::startup_unix::{connect_or_start_with, StartupError, StartupOutcome};
use tokio::runtime::Runtime;

/// A probe that blocks forever is a failure, not a hung test run.
const WATCHDOG: Duration = Duration::from_secs(20);

#[test]
fn a_saturated_live_daemon_is_neither_unlinked_nor_replaced() {
    let endpoint = Endpoint::new("keep");
    let daemon = SaturatedDaemon::bind(&endpoint.socket_path);
    let inode = socket_inode(&endpoint.socket_path);

    let (result, launched, _) =
        connect_or_start_within_watchdog(&endpoint.socket_path, Duration::from_millis(400));

    let inode_after = socket_inode(&endpoint.socket_path);
    assert!(
        !launched && inode_after == inode,
        "the live daemon was replaced: launcher called {launched}, \
         socket inode {inode:?} before and {inode_after:?} after"
    );
    match result {
        Err(error @ StartupError::StartupTimeout { .. }) => assert!(error.is_recoverable()),
        other => panic!("expected a recoverable startup timeout, got {other:?}"),
    }
    drop(daemon);
}

#[test]
fn a_saturated_live_daemon_is_joined_once_it_accepts() {
    let endpoint = Endpoint::new("join");
    let daemon = SaturatedDaemon::bind(&endpoint.socket_path);
    let acceptor = daemon.accept_after(Duration::from_millis(200));

    let (result, launched, _) =
        connect_or_start_within_watchdog(&endpoint.socket_path, Duration::from_secs(5));

    assert!(
        !launched,
        "a second daemon was launched beside the live one"
    );
    match result {
        Ok(outcome @ StartupOutcome::JoinedExisting(_)) => assert!(!outcome.is_owner()),
        other => panic!("expected to join the live daemon, got {other:?}"),
    }
    acceptor.store(true, Ordering::SeqCst);
    drop(daemon);
}

#[test]
fn a_saturated_live_daemon_times_out_within_its_deadline() {
    let endpoint = Endpoint::new("deadline");
    let daemon = SaturatedDaemon::bind(&endpoint.socket_path);
    // Shorter than one connect probe, so that an unbounded probe overruns it.
    let deadline = Duration::from_millis(20);

    let (result, launched, elapsed) =
        connect_or_start_within_watchdog(&endpoint.socket_path, deadline);

    assert!(
        !launched,
        "a second daemon was launched beside the live one"
    );
    assert!(
        matches!(result, Err(StartupError::StartupTimeout { .. })),
        "expected a startup timeout, got {result:?}"
    );
    // The probe bound and its wiring into the blocking connect are guarded
    // without a clock by the startup_unix unit tests; this bound only catches
    // a startup that hangs.
    assert!(
        elapsed < deadline + Duration::from_secs(1),
        "a {deadline:?} startup deadline returned after {elapsed:?}"
    );
    drop(daemon);
}

/// Runs one startup on its own runtime and returns its result, whether the
/// launcher ran, and how long the startup itself took.
fn connect_or_start_within_watchdog(
    socket_path: &Path,
    deadline: Duration,
) -> (Result<StartupOutcome, StartupError>, bool, Duration) {
    let socket_path = socket_path.to_path_buf();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let launched = Arc::new(AtomicBool::new(false));
        let launched_by_launcher = Arc::clone(&launched);
        let runtime = runtime();
        let started = Instant::now();
        let result = runtime.block_on(connect_or_start_with(
            &socket_path,
            move || async move {
                launched_by_launcher.store(true, Ordering::SeqCst);
                Ok(())
            },
            deadline,
            Duration::from_millis(25),
        ));
        let elapsed = started.elapsed();
        let _ = sender.send((result, launched.load(Ordering::SeqCst), elapsed));
    });
    receiver
        .recv_timeout(WATCHDOG)
        .expect("connect_or_start blocked past its deadline on a saturated daemon")
}

/// A bound daemon socket with a backlog of one whose queue is already full.
struct SaturatedDaemon {
    listener: Arc<UnixListener>,
    _queued: Vec<tokio::net::UnixStream>,
    _runtime: Runtime,
}

impl SaturatedDaemon {
    fn bind(socket_path: &Path) -> Self {
        let listener = UnixListener::bind(socket_path).expect("bind daemon socket");
        // SAFETY: `listen` only adjusts the backlog of this valid listening fd.
        let rc = unsafe { libc::listen(listener.as_raw_fd(), 1) };
        assert_eq!(rc, 0, "shrink backlog: {}", io::Error::last_os_error());

        let runtime = runtime();
        let mut queued = Vec::new();
        // Darwin refuses the connect once the queue is full (ECONNREFUSED);
        // Linux rejects the non-blocking connect (EAGAIN).
        while let Ok(stream) = runtime.block_on(tokio::net::UnixStream::connect(socket_path)) {
            queued.push(stream);
            assert!(queued.len() < 64, "the accept queue never filled");
        }
        Self {
            listener: Arc::new(listener),
            _queued: queued,
            _runtime: runtime,
        }
    }

    /// Accepts every queued and future connection after `delay` until the
    /// returned flag is set.
    fn accept_after(&self, delay: Duration) -> Arc<AtomicBool> {
        let listener = Arc::clone(&self.listener);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        thread::spawn(move || {
            thread::sleep(delay);
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let mut accepted = Vec::new();
            while !stop_for_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => accepted.push(stream),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            }
        });
        stop
    }
}

struct Endpoint {
    socket_path: PathBuf,
}

impl Endpoint {
    fn new(label: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        // Darwin's sockaddr_un budget is short; TMPDIR may be a long path.
        let root = PathBuf::from("/tmp").join(format!(
            "rmux-bs-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create test root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("secure test root");
        Self {
            socket_path: root.join("s"),
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if let Some(root) = self.socket_path.parent() {
            let _ = fs::remove_dir_all(root);
        }
    }
}

fn socket_inode(socket_path: &Path) -> Option<u64> {
    fs::symlink_metadata(socket_path)
        .ok()
        .map(|metadata| metadata.ino())
}

fn runtime() -> Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}
