#![cfg(unix)]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rmux_sdk::bootstrap::discovery::SDK_DAEMON_BINARY_ENV;
use rmux_sdk::{Rmux, RmuxError};

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn cmd_injects_resolved_unix_endpoint_and_preserves_process_output() {
    let _lock = ENV_LOCK.lock().await;
    let root = TestRoot::new("sdk-cmd");
    let fake_rmux = root.path().join("fake-rmux.sh");
    write_fake_rmux(&fake_rmux);
    let _binary = EnvGuard::set(SDK_DAEMON_BINARY_ENV, fake_rmux.as_os_str());

    let socket = root.path().join("daemon.sock");
    let rmux = Rmux::builder().unix_socket(&socket).build();
    let run = rmux
        .cmd(["list-sessions", "--json"])
        .await
        .expect("fake rmux process runs");

    assert_eq!(run.exit, Some(7));
    assert_eq!(String::from_utf8_lossy(&run.stderr), "stderr-line\n");
    let stdout = String::from_utf8(run.stdout).expect("fake stdout is UTF-8");
    let args = stdout.lines().collect::<Vec<_>>();
    assert_eq!(
        args,
        [
            "-S",
            socket.to_str().expect("test socket path is UTF-8"),
            "list-sessions",
            "--json"
        ]
    );
}

#[tokio::test]
async fn cmd_applies_the_facade_operation_timeout_to_the_child_tree() {
    let _lock = ENV_LOCK.lock().await;
    let root = TestRoot::new("sdk-cmd-timeout");
    let fake_rmux = root.path().join("fake-rmux.sh");
    write_fake_rmux(&fake_rmux);
    let _binary = EnvGuard::set(SDK_DAEMON_BINARY_ENV, fake_rmux.as_os_str());

    let socket = root.path().join("daemon.sock");
    let rmux = Rmux::builder()
        .unix_socket(&socket)
        .default_timeout(Duration::from_millis(100))
        .build();
    let started_at = Instant::now();
    let error = rmux
        .cmd(["hang"])
        .await
        .expect_err("blocking fake rmux exceeds the facade operation timeout");

    match error {
        RmuxError::Transport {
            operation, source, ..
        } => {
            assert_eq!(operation, "run rmux command");
            assert_eq!(source.kind(), std::io::ErrorKind::TimedOut);
        }
        error => panic!("expected command transport timeout, got {error:?}"),
    }
    assert!(
        started_at.elapsed() < Duration::from_secs(3),
        "public SDK command timeout should stay bounded: {:?}",
        started_at.elapsed()
    );
}

fn write_fake_rmux(path: &Path) {
    fs::write(
        path,
        "#!/bin/sh\nfor arg in \"$@\"; do printf '%s\\n' \"$arg\"; done\nprintf 'stderr-line\\n' >&2\nif [ \"${1-}\" = \"-S\" ]; then shift 2; fi\nif [ \"${1-}\" = \"hang\" ]; then sleep 30; fi\nexit 7\n",
    )
    .expect("write fake rmux");
    let mut permissions = fs::metadata(path).expect("fake metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make fake executable");
}

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &OsStr) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "rmux-{label}-{}-{}",
            std::process::id(),
            monotonic_suffix()
        ));
        fs::create_dir_all(&path).expect("create test root");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn monotonic_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
