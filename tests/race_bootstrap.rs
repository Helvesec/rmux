#![cfg(unix)]

//! Parallel bootstrap race coverage for `rmux_bootstrap::startup_unix`.
//!
//! These tests prove the documented contract for `connect_or_start`: under N
//! concurrent callers per endpoint, exactly one daemon is created, and every
//! caller either connects to that daemon or surfaces a typed recoverable
//! error.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rmux_bootstrap::startup_unix::{
    connect_or_start_with, StartupError, StartupOutcome, DEFAULT_STARTUP_DEADLINE,
    STARTUP_POLL_INTERVAL,
};
use rmux_server::{DaemonConfig, ServerDaemon, ServerHandle};
use tokio::sync::Mutex;

const RACE_PARALLELISM: usize = 16;
static NEXT_SOCKET_ID: AtomicUsize = AtomicUsize::new(0);

fn unique_socket_path(label: &str) -> PathBuf {
    // macOS has a tighter sockaddr_un path limit than Linux, and TMPDIR can
    // already consume most of it under /var/folders. Keep race-test endpoints
    // intentionally short so the tests exercise startup semantics, not path
    // length behavior.
    let id = NEXT_SOCKET_ID.fetch_add(1, Ordering::Relaxed);
    let label = compact_socket_label(label);
    PathBuf::from("/tmp")
        .join(format!("rmux-race-{label}-{}-{id}", std::process::id()))
        .join("s")
}

fn compact_socket_label(label: &str) -> String {
    let compact = label
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .take(8)
        .map(char::from)
        .collect::<String>();

    if compact.is_empty() {
        "case".to_owned()
    } else {
        compact
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_connect_or_start_creates_exactly_one_serving_daemon() {
    let socket_path = unique_socket_path("happy");
    let parent = socket_path.parent().expect("socket parent").to_path_buf();
    let _cleanup = TempDirCleanup::new(parent.clone());

    let launcher_calls = Arc::new(AtomicUsize::new(0));
    let daemon_handle = Arc::new(Mutex::new(None::<ServerHandle>));

    let mut tasks = Vec::with_capacity(RACE_PARALLELISM);
    for _ in 0..RACE_PARALLELISM {
        let socket_path = socket_path.clone();
        let launcher_calls = Arc::clone(&launcher_calls);
        let daemon_handle = Arc::clone(&daemon_handle);
        tasks.push(tokio::spawn(async move {
            connect_or_start_with(
                &socket_path,
                {
                    let socket_path = socket_path.clone();
                    let launcher_calls = Arc::clone(&launcher_calls);
                    let daemon_handle = Arc::clone(&daemon_handle);
                    move || async move {
                        launcher_calls.fetch_add(1, Ordering::SeqCst);
                        let config = DaemonConfig::new(socket_path);
                        let handle = ServerDaemon::new(config).bind().await?;
                        daemon_handle.lock().await.replace(handle);
                        Ok(())
                    }
                },
                DEFAULT_STARTUP_DEADLINE,
                STARTUP_POLL_INTERVAL,
            )
            .await
        }));
    }

    let mut owners = 0_usize;
    let mut joined = 0_usize;
    let mut recoverable_losers = 0_usize;
    let mut other_errors = Vec::new();

    for task in tasks {
        match task.await.expect("connect_or_start task did not panic") {
            Ok(StartupOutcome::Started(_stream)) => owners += 1,
            Ok(StartupOutcome::JoinedExisting(_stream)) => joined += 1,
            Err(error) if error.is_recoverable() => {
                recoverable_losers += 1;
                eprintln!("documented recoverable loser: {error}");
            }
            Err(error) => other_errors.push(error.to_string()),
        }
    }

    assert!(
        other_errors.is_empty(),
        "non-recoverable errors observed: {other_errors:?}"
    );
    assert_eq!(
        launcher_calls.load(Ordering::SeqCst),
        1,
        "launcher must be invoked exactly once"
    );
    assert_eq!(owners, 1, "exactly one caller must own the startup");
    assert_eq!(
        owners + joined + recoverable_losers,
        RACE_PARALLELISM,
        "every caller must be accounted for"
    );

    let mut storage = daemon_handle.lock().await;
    if let Some(handle) = storage.take() {
        handle.shutdown().await.expect("shutdown daemon");
    }
}

#[tokio::test]
async fn loser_returns_recoverable_error_when_owner_launcher_fails() {
    let socket_path = unique_socket_path("launcher-fails");
    let parent = socket_path.parent().expect("socket parent").to_path_buf();
    let _cleanup = TempDirCleanup::new(parent);

    let launcher_calls = Arc::new(AtomicUsize::new(0));
    let launcher_calls_inner = Arc::clone(&launcher_calls);

    let result = connect_or_start_with(
        &socket_path,
        move || async move {
            launcher_calls_inner.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::other("launcher refused"))
        },
        Duration::from_millis(100),
        Duration::from_millis(10),
    )
    .await;

    assert_eq!(launcher_calls.load(Ordering::SeqCst), 1);
    match result {
        Err(error) if error.is_recoverable() => match &error {
            StartupError::Launcher { .. } => {}
            other => panic!("expected launcher failure, got {other}"),
        },
        Ok(_) => panic!("expected launcher failure, got success"),
        Err(error) => panic!("expected recoverable launcher error, got {error}"),
    }
}

#[tokio::test]
async fn rejects_socket_path_with_symlinked_target() {
    let socket_path = unique_socket_path("symlink");
    let parent = socket_path.parent().expect("socket parent").to_path_buf();
    let _cleanup = TempDirCleanup::new(parent.clone());

    fs::create_dir_all(&parent).expect("create parent");
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o700);
        fs::set_permissions(&parent, perms).expect("tighten parent");
    }

    let target = parent.join(".symlink-target");
    fs::write(&target, b"not a socket").expect("write target");
    std::os::unix::fs::symlink(&target, &socket_path).expect("create symlink at socket path");

    let result = connect_or_start_with(
        &socket_path,
        || async move { Err::<(), io::Error>(io::Error::other("never reached")) },
        Duration::from_millis(50),
        Duration::from_millis(10),
    )
    .await;

    match result {
        Err(StartupError::SymlinkRejected { path }) => {
            assert_eq!(path, socket_path);
        }
        other => panic!("expected SymlinkRejected, got {other:?}"),
    }
}

#[tokio::test]
async fn rejects_live_socket_reached_through_symlink_before_first_probe() {
    use std::os::unix::fs::PermissionsExt;

    let socket_path = unique_socket_path("live-symlink");
    let parent = socket_path.parent().expect("socket parent").to_path_buf();
    let _cleanup = TempDirCleanup::new(parent.clone());

    fs::create_dir_all(&parent).expect("create parent");
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).expect("tighten parent");

    let target = parent.join(".real-socket");
    let _listener = tokio::net::UnixListener::bind(&target).expect("bind real socket target");
    std::os::unix::fs::symlink(&target, &socket_path)
        .expect("create symlink at requested socket path");

    let launcher_calls = Arc::new(AtomicUsize::new(0));
    let launcher_calls_inner = Arc::clone(&launcher_calls);
    let result = connect_or_start_with(
        &socket_path,
        move || async move {
            launcher_calls_inner.fetch_add(1, Ordering::SeqCst);
            Err::<(), io::Error>(io::Error::other("never reached"))
        },
        Duration::from_millis(50),
        Duration::from_millis(10),
    )
    .await;

    assert_eq!(
        launcher_calls.load(Ordering::SeqCst),
        0,
        "symlink rejection must happen before startup ownership"
    );
    match result {
        Err(StartupError::SymlinkRejected { path }) => {
            assert_eq!(path, socket_path);
        }
        other => panic!("expected SymlinkRejected for live symlink, got {other:?}"),
    }
}

#[tokio::test]
async fn second_caller_joins_already_running_daemon() {
    let socket_path = unique_socket_path("already-running");
    let parent = socket_path.parent().expect("socket parent").to_path_buf();
    let _cleanup = TempDirCleanup::new(parent);

    let launcher_calls = Arc::new(AtomicUsize::new(0));

    let first = connect_or_start_with(
        &socket_path,
        {
            let socket_path = socket_path.clone();
            let launcher_calls = Arc::clone(&launcher_calls);
            move || async move {
                launcher_calls.fetch_add(1, Ordering::SeqCst);
                let config = DaemonConfig::new(socket_path);
                let handle = ServerDaemon::new(config).bind().await?;
                let _ = Box::leak(Box::new(handle));
                Ok(())
            }
        },
        DEFAULT_STARTUP_DEADLINE,
        STARTUP_POLL_INTERVAL,
    )
    .await
    .expect("first connect_or_start");

    assert!(matches!(first, StartupOutcome::Started(_)));
    drop(first);

    let second = connect_or_start_with(
        &socket_path,
        || async move {
            panic!("launcher must not run when a daemon is already serving the endpoint");
        },
        DEFAULT_STARTUP_DEADLINE,
        STARTUP_POLL_INTERVAL,
    )
    .await
    .expect("second connect_or_start");

    assert!(matches!(second, StartupOutcome::JoinedExisting(_)));
    assert_eq!(launcher_calls.load(Ordering::SeqCst), 1);
    drop(second);

    let _ = std::fs::remove_file(&socket_path);
}

#[tokio::test]
async fn stale_socket_residue_is_recovered_under_lock() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener as StdUnixListener;

    let socket_path = unique_socket_path("stale-residue");
    let parent = socket_path.parent().expect("socket parent").to_path_buf();
    let _cleanup = TempDirCleanup::new(parent.clone());

    fs::create_dir_all(&parent).expect("create parent");
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).expect("tighten parent");

    let listener = StdUnixListener::bind(&socket_path).expect("bind stale listener");
    drop(listener);
    assert!(socket_path.exists(), "stale socket file must persist");

    let launcher_calls = Arc::new(AtomicUsize::new(0));
    let daemon_handle = Arc::new(Mutex::new(None::<ServerHandle>));

    let outcome = connect_or_start_with(
        &socket_path,
        {
            let socket_path = socket_path.clone();
            let launcher_calls = Arc::clone(&launcher_calls);
            let daemon_handle = Arc::clone(&daemon_handle);
            move || async move {
                launcher_calls.fetch_add(1, Ordering::SeqCst);
                let config = DaemonConfig::new(socket_path);
                let handle = ServerDaemon::new(config).bind().await?;
                daemon_handle.lock().await.replace(handle);
                Ok(())
            }
        },
        DEFAULT_STARTUP_DEADLINE,
        STARTUP_POLL_INTERVAL,
    )
    .await
    .expect("stale socket recovery should succeed under lock");

    assert!(matches!(outcome, StartupOutcome::Started(_)));
    assert_eq!(launcher_calls.load(Ordering::SeqCst), 1);

    let mut storage = daemon_handle.lock().await;
    if let Some(handle) = storage.take() {
        handle.shutdown().await.expect("shutdown daemon");
    }
}

#[tokio::test]
async fn non_socket_residue_is_refused_without_unlink() {
    use std::os::unix::fs::PermissionsExt;

    let socket_path = unique_socket_path("non-socket");
    let parent = socket_path.parent().expect("socket parent").to_path_buf();
    let _cleanup = TempDirCleanup::new(parent.clone());

    fs::create_dir_all(&parent).expect("create parent");
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).expect("tighten parent");
    fs::write(&socket_path, b"not a socket").expect("write regular file at socket path");

    let launcher_calls = Arc::new(AtomicUsize::new(0));
    let launcher_calls_inner = Arc::clone(&launcher_calls);
    let result = connect_or_start_with(
        &socket_path,
        move || async move {
            launcher_calls_inner.fetch_add(1, Ordering::SeqCst);
            Err::<(), io::Error>(io::Error::other("never reached"))
        },
        Duration::from_millis(50),
        Duration::from_millis(10),
    )
    .await;

    match result {
        Err(StartupError::Filesystem {
            operation, path, ..
        }) => {
            assert_eq!(path, socket_path);
            assert!(
                operation.contains("non-socket")
                    || operation.contains("residue")
                    || operation == "connect to daemon socket",
                "expected non-socket residue rejection or platform connect refusal, got operation = {operation:?}"
            );
        }
        other => panic!("expected non-socket Filesystem rejection, got {other:?}"),
    }
    assert_eq!(
        launcher_calls.load(Ordering::SeqCst),
        0,
        "non-socket residue must fail before launching a daemon"
    );
    assert!(
        socket_path.exists(),
        "non-socket residue must not be unlinked by startup"
    );
}

#[tokio::test]
async fn lock_file_with_unsafe_permissions_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let socket_path = unique_socket_path("loose-lock");
    let parent = socket_path.parent().expect("socket parent").to_path_buf();
    let _cleanup = TempDirCleanup::new(parent.clone());

    fs::create_dir_all(&parent).expect("create parent");
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).expect("tighten parent");

    let lock_path = parent.join(format!(
        "{}.startup-lock",
        socket_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("socket file name")
    ));
    fs::write(&lock_path, b"").expect("create lock file");
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644))
        .expect("set unsafe permissions");

    let result = connect_or_start_with(
        &socket_path,
        || async move { Err::<(), io::Error>(io::Error::other("never reached")) },
        Duration::from_millis(50),
        Duration::from_millis(10),
    )
    .await;

    match result {
        Err(StartupError::UnsafePermissions { path, mode }) => {
            assert_eq!(path, lock_path);
            assert_ne!(mode & 0o077, 0, "expected mode to expose group/other bits");
        }
        other => panic!("expected UnsafePermissions for lock file, got {other:?}"),
    }
}

#[tokio::test]
async fn lock_path_pointing_at_fifo_is_refused() {
    use std::ffi::CString;
    use std::os::unix::fs::PermissionsExt;

    let socket_path = unique_socket_path("fifo-lock");
    let parent = socket_path.parent().expect("socket parent").to_path_buf();
    let _cleanup = TempDirCleanup::new(parent.clone());

    fs::create_dir_all(&parent).expect("create parent");
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).expect("tighten parent");

    let lock_path = parent.join(format!(
        "{}.startup-lock",
        socket_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("socket file name")
    ));
    let lock_cstr = CString::new(lock_path.as_os_str().as_encoded_bytes())
        .expect("lock path is interior-nul-free");
    let rc = unsafe { libc::mkfifo(lock_cstr.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo failed: {}", io::Error::last_os_error());

    let result = connect_or_start_with(
        &socket_path,
        || async move { Err::<(), io::Error>(io::Error::other("never reached")) },
        Duration::from_millis(50),
        Duration::from_millis(10),
    )
    .await;

    match result {
        Err(StartupError::Filesystem {
            operation, path, ..
        }) => {
            assert_eq!(path, lock_path);
            assert!(
                operation.contains("regular file"),
                "expected regular-file rejection, got operation = {operation:?}"
            );
        }
        other => panic!("expected Filesystem rejection for FIFO lock path, got {other:?}"),
    }
}

struct TempDirCleanup {
    path: PathBuf,
}

impl TempDirCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TempDirCleanup {
    fn drop(&mut self) {
        let _ = remove_tree(&self.path);
    }
}

fn remove_tree(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
