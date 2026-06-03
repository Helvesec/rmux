#![cfg(unix)]

mod common;

use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use rmux_sdk::{
    bootstrap::discovery::SDK_DAEMON_BINARY_ENV, EnsureSession, EnsureSessionPolicy, PaneInfo,
    PaneOutputChunk, PaneOutputStart, PaneOutputStream, PaneProcessState, RmuxBuilder, SessionName,
};
use tokio::time::{sleep, timeout, Instant};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const MARKER: &str = "RMUX_SDK_SMOKE_V1_OK";
const SMOKE_ROOT_PREFIX: &str = "rmux-sdk-v1-smoke-";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

static LIVE_DAEMON_LOCK: common::unix_smoke::LiveDaemonLock =
    common::unix_smoke::LiveDaemonLock::new();

#[tokio::test]
async fn daemon_backed_sdk_happy_path_cleans_tmp_socket_lock_daemon_and_child() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let root = smoke_root()?;
    let socket_path = root.join("daemon.sock");
    let lock_path = root.join("daemon.sock.startup-lock");
    let session_name = SessionName::new("sdksmokev1")?;
    let cleanup = Cleanup::new(root.clone(), socket_path.clone());
    let daemon_binary = rmux_binary()?.to_path_buf();
    let _daemon_binary_env = EnvGuard::set(SDK_DAEMON_BINARY_ENV, daemon_binary.as_os_str());

    if let Some(parent) = socket_path.parent() {
        let _ = fs::remove_dir_all(parent);
    }

    let rmux = RmuxBuilder::new()
        .unix_socket(&socket_path)
        .default_timeout(DEFAULT_TIMEOUT)
        .connect_or_start()
        .await?;
    assert_socket(&socket_path)?;
    assert!(
        lock_path.is_file(),
        "SDK connect_or_start did not create startup lock {}",
        lock_path.display()
    );
    let daemon_pid = wait_for_daemon_pid(&socket_path).await?;

    let warm = RmuxBuilder::new()
        .unix_socket(&socket_path)
        .default_timeout(DEFAULT_TIMEOUT)
        .connect_or_start()
        .await?;
    assert!(
        warm.list_sessions().await?.is_empty(),
        "fresh smoke daemon should start without preexisting sessions"
    );
    drop(warm);

    let session = rmux
        .ensure_session(
            EnsureSession::named(session_name.clone())
                .policy(EnsureSessionPolicy::CreateOrReuse)
                .detached(true),
        )
        .await?;
    assert!(session.exists().await?);
    assert!(session.is_listed().await?);

    let pane = session.pane(0, 0);
    let pane_pid = wait_for_pane_pid(&pane).await?;
    let mut output = pane.output_stream_starting_at(PaneOutputStart::Now).await?;
    pane.send_text(format!("printf '{MARKER}\\n'\n")).await?;
    wait_for_output_marker(&mut output, MARKER.as_bytes()).await?;
    drop(output);
    pane.wait_for_text(MARKER).await?;
    assert!(pane.snapshot().await?.visible_text().contains(MARKER));

    rmux.shutdown().await?;
    wait_for_daemon_absent(&socket_path, daemon_pid).await?;
    wait_for_process_absent(pane_pid).await?;
    wait_for_path_absent(&socket_path).await?;
    wait_for_path_absent(&lock_path).await?;

    fs::remove_dir(&root)?;
    cleanup.disarm();

    assert!(!socket_path.exists(), "socket path remained after cleanup");
    assert!(!lock_path.exists(), "startup lock remained after cleanup");
    assert!(!root.exists(), "endpoint root remained after cleanup");
    Ok(())
}

fn assert_socket(path: &Path) -> TestResult {
    let metadata = fs::symlink_metadata(path)?;
    assert!(
        metadata.file_type().is_socket(),
        "{} exists but is not a Unix socket",
        path.display()
    );
    Ok(())
}

async fn wait_for_pane_pid(pane: &rmux_sdk::Pane) -> TestResult<u32> {
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    loop {
        let info = only_pane_info(&pane.info().await?);
        if let PaneProcessState::Running { pid: Some(pid) } = info.process {
            return Ok(pid);
        }
        if Instant::now() >= deadline {
            return Err(format!("pane did not report a running child pid: {info:?}").into());
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn only_pane_info(info: &rmux_sdk::InfoSnapshot) -> PaneInfo {
    assert_eq!(info.panes.len(), 1, "expected one pane in smoke session");
    info.panes[0].clone()
}

async fn wait_for_output_marker(stream: &mut PaneOutputStream, marker: &[u8]) -> TestResult {
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("pane output stream did not emit smoke marker".into());
        }

        match timeout(remaining, stream.next()).await?? {
            Some(PaneOutputChunk::Bytes { bytes, .. })
                if bytes.windows(marker.len()).any(|window| window == marker) =>
            {
                return Ok(());
            }
            Some(_) => {}
            None => return Err("pane output stream closed before smoke marker".into()),
        }
    }
}

async fn wait_for_daemon_pid(socket_path: &Path) -> TestResult<u32> {
    let needle = socket_path.to_string_lossy().into_owned();
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    loop {
        if let Some(pid) = daemon_pid_for_socket(&needle)? {
            return Ok(pid);
        }
        if Instant::now() >= deadline {
            return Err(format!("daemon pid for {} was not visible", socket_path.display()).into());
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_daemon_absent(socket_path: &Path, original_pid: u32) -> TestResult {
    let needle = socket_path.to_string_lossy().into_owned();
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    loop {
        match daemon_pid_for_socket(&needle)? {
            None => return Ok(()),
            Some(pid) if pid == original_pid => {}
            Some(pid) => {
                return Err(format!(
                    "daemon process {pid} still references {} after shutdown",
                    socket_path.display()
                )
                .into());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "daemon process {original_pid} for {} remained visible after SDK smoke cleanup",
                socket_path.display()
            )
            .into());
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn daemon_pid_for_socket(socket_needle: &str) -> TestResult<Option<u32>> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if !line.contains("--__internal-daemon") || !line.contains(socket_needle) {
            continue;
        }
        let Some(pid) = line.split_whitespace().next() else {
            continue;
        };
        if let Ok(pid) = pid.parse::<u32>() {
            return Ok(Some(pid));
        }
    }
    Ok(None)
}

async fn wait_for_process_absent(pid: u32) -> TestResult {
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    loop {
        if !process_exists(pid)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("process {pid} remained alive after SDK smoke cleanup").into());
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn process_exists(pid: u32) -> TestResult<bool> {
    Ok(Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success())
}

async fn wait_for_path_absent(path: &Path) -> TestResult {
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    loop {
        if !path.exists() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("path remained after shutdown: {}", path.display()).into());
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn smoke_root() -> TestResult<PathBuf> {
    let root = if let Some(root) = std::env::var_os("RMUX_SDK_SMOKE_ROOT") {
        PathBuf::from(root)
    } else {
        PathBuf::from(format!("/tmp/rmux-sdk-v1-smoke-{}", std::process::id()))
    };

    if !is_tmp_smoke_root(&root) {
        return Err(format!(
            "SDK smoke endpoint root must be an absolute /tmp/{SMOKE_ROOT_PREFIX}* path without '.' or '..' components, got {}",
            root.display()
        )
        .into());
    }

    Ok(root)
}

fn is_tmp_smoke_root(root: &Path) -> bool {
    if !root.is_absolute() || !root.starts_with(Path::new("/tmp")) {
        return false;
    }
    if root
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return false;
    }

    match root.file_name().and_then(|name| name.to_str()) {
        Some(name) => name.starts_with(SMOKE_ROOT_PREFIX) && name.len() > SMOKE_ROOT_PREFIX.len(),
        None => false,
    }
}

fn rmux_binary() -> TestResult<&'static Path> {
    static RMUX_BINARY: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    match RMUX_BINARY.get_or_init(|| resolve_rmux_binary().map_err(|error| error.to_string())) {
        Ok(path) => Ok(path.as_path()),
        Err(error) => Err(std::io::Error::other(error.clone()).into()),
    }
}

fn resolve_rmux_binary() -> TestResult<PathBuf> {
    if let Some(path) = option_env!("CARGO_BIN_EXE_rmux") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    let target_dir = target_dir()?;
    let candidate = target_dir.join("debug").join("rmux");

    let status = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .arg("build")
        .arg("--bin")
        .arg("rmux")
        .arg("--locked")
        .arg("--manifest-path")
        .arg(workspace_root().join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target_dir)
        .status()?;
    if !status.success() {
        return Err(format!("failed to build rmux binary for SDK smoke: {status}").into());
    }
    if !candidate.is_file() {
        return Err(format!(
            "rmux binary build succeeded but '{}' was not created",
            candidate.display()
        )
        .into());
    }

    Ok(candidate)
}

fn target_dir() -> TestResult<PathBuf> {
    if let Some(target_dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(target_dir));
    }

    let current = std::env::current_exe()?;
    current
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "test executable is not under a target directory".into())
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("rmux-sdk manifest lives under crates/rmux-sdk")
        .to_path_buf()
}

struct Cleanup {
    root: PathBuf,
    socket_path: PathBuf,
    armed: bool,
}

impl Cleanup {
    fn new(root: PathBuf, socket_path: PathBuf) -> Self {
        Self {
            root,
            socket_path,
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.socket_path.exists() {
            let _ = Command::new(rmux_binary().unwrap_or_else(|_| Path::new("rmux")))
                .arg("-S")
                .arg(&self.socket_path)
                .arg("kill-server")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &std::ffi::OsStr) -> Self {
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
