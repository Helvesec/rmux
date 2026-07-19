#![allow(dead_code)]
// This helper module is compiled into multiple Windows integration test
// binaries; each smoke owns a different subset of the shared harness.

use std::error::Error;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use rmux_sdk::{
    bootstrap::discovery::SDK_DAEMON_BINARY_ENV, PaneOutputChunk, PaneOutputStream, Rmux,
    RmuxBuilder, SessionName,
};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout, Instant};

#[path = "../../../../tests/support/windows_cargo_build.rs"]
mod windows_cargo_build;

pub type TestResult<T = ()> = Result<T, Box<dyn Error>>;

// Windows CI can be slow to start ConPTY-backed shells while the workspace
// test run is still compiling sibling crates. Keep this high enough to catch
// real prompt/output transitions without making successful tests slower.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
pub const OUTPUT_BUDGET: usize = 64 * 1024;
const DAEMON_UNAVAILABLE_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const DAEMON_UNAVAILABLE_POLL_INTERVAL: Duration = Duration::from_millis(25);
const SNAPSHOT_STABLE_PERIOD: Duration = Duration::from_millis(500);
const RMUX_SDK_WINDOWS_SMOKE_RMUX_BIN_ENV: &str = "RMUX_SDK_WINDOWS_SMOKE_RMUX_BIN";
const RMUX_SDK_WINDOWS_SMOKE_PIPE_ENV: &str = "RMUX_SDK_WINDOWS_SMOKE_PIPE";

pub static LIVE_DAEMON_LOCK: Mutex<()> = Mutex::const_new(());

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

pub struct Harness {
    pipe_name: String,
    rmux: Option<Rmux>,
    armed: bool,
}

impl Harness {
    pub async fn start(label: &str) -> TestResult<Self> {
        let package_pipe = std::env::var(RMUX_SDK_WINDOWS_SMOKE_PIPE_ENV)
            .ok()
            .filter(|pipe_name| !pipe_name.is_empty());
        let pipe_name = match &package_pipe {
            Some(pipe_name) => pipe_name.clone(),
            None => unique_pipe_name(label)?,
        };
        let daemon_binary = rmux_binary()?.to_path_buf();
        let _daemon_binary_env = EnvGuard::set(SDK_DAEMON_BINARY_ENV, daemon_binary.as_os_str());
        let rmux = if package_pipe.is_some() {
            builder(&pipe_name).connect().await?
        } else {
            builder(&pipe_name).connect_or_start().await?
        };
        Ok(Self {
            pipe_name,
            rmux: Some(rmux),
            armed: true,
        })
    }

    pub async fn start_via_cmd(label: &str, session_name: &SessionName) -> TestResult<Self> {
        let pipe_name = unique_pipe_name(label)?;
        let daemon_binary = rmux_binary()?.to_path_buf();
        let _daemon_binary_env = EnvGuard::set(SDK_DAEMON_BINARY_ENV, daemon_binary.as_os_str());
        let rmux = builder(&pipe_name).build();
        let run = rmux
            .cmd(["new-session", "-d", "-s", session_name.as_str()])
            .await?;
        if run.exit != Some(0) {
            return Err(format!(
                "cold-start command exited {:?}: {}",
                run.exit,
                String::from_utf8_lossy(&run.stderr)
            )
            .into());
        }
        Ok(Self {
            pipe_name,
            rmux: Some(rmux),
            armed: true,
        })
    }

    pub async fn start_with_default_config_environment(
        label: &str,
        appdata: &Path,
        caller_cwd: &Path,
    ) -> TestResult<Self> {
        let pipe_name = unique_pipe_name(label)?;
        let daemon_binary = rmux_binary()?.to_path_buf();
        let _daemon_binary_env = EnvGuard::set(SDK_DAEMON_BINARY_ENV, daemon_binary.as_os_str());
        let _appdata_env = EnvGuard::set("APPDATA", appdata.as_os_str());
        let _xdg_env = EnvGuard::remove("XDG_CONFIG_HOME");
        let _userprofile_env = EnvGuard::remove("USERPROFILE");
        let _explicit_config_env = EnvGuard::remove("RMUX_CONFIG_FILE");
        let _cwd = CurrentDirGuard::set(caller_cwd)?;
        let rmux = builder(&pipe_name).connect_or_start().await?;
        Ok(Self {
            pipe_name,
            rmux: Some(rmux),
            armed: true,
        })
    }

    pub fn rmux(&self) -> &Rmux {
        self.rmux.as_ref().expect("harness rmux is available")
    }

    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    pub fn take_rmux(&mut self) -> TestResult<Rmux> {
        self.rmux
            .take()
            .ok_or_else(|| "harness rmux was already taken".into())
    }

    pub async fn finish(mut self) -> TestResult {
        if let Some(rmux) = self.rmux.take() {
            rmux.shutdown().await?;
            wait_for_daemon_unavailable(&self.pipe_name).await?;
        }
        self.armed = false;
        Ok(())
    }

    pub async fn disarm_after_shutdown(mut self) -> TestResult {
        wait_for_daemon_unavailable(&self.pipe_name).await?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = Command::new(rmux_binary().unwrap_or_else(|_| Path::new("rmux")))
            .arg("-S")
            .arg(&self.pipe_name)
            .arg("kill-server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

pub fn builder(pipe_name: &str) -> RmuxBuilder {
    builder_with_timeout(pipe_name, DEFAULT_TIMEOUT)
}

fn builder_with_timeout(pipe_name: &str, default_timeout: Duration) -> RmuxBuilder {
    RmuxBuilder::new()
        .windows_pipe(pipe_name.to_owned())
        .default_timeout(default_timeout)
}

pub fn session_name(prefix: &str) -> SessionName {
    SessionName::new(format!("{prefix}{}", unique_id())).expect("valid smoke session name")
}

pub fn cmd_interactive_command() -> Vec<String> {
    vec![cmd_exe(), "/d".to_owned(), "/q".to_owned()]
}

pub fn cmd_echo_text(marker: &str) -> String {
    format!("echo {marker}\r")
}

pub fn cmd_echo_once_command(text: &str) -> Vec<String> {
    vec![
        cmd_exe(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        format!("echo {text}"),
    ]
}

pub fn cmd_burst_once_command(start: &str, end: &str, exit_code: i32) -> Vec<String> {
    vec![
        cmd_exe(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        format!(
            "echo {start} & (for /L %i in (1,1,300) do @echo line-%i) & echo {end} & exit /b {exit_code}"
        ),
    ]
}

pub fn cmd_delayed_echo_once_command(text: &str) -> Vec<String> {
    vec![
        cmd_exe(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        format!("ping -n 2 127.0.0.1 >NUL & echo {text}"),
    ]
}

pub fn cmd_long_running_command(started_marker: &str) -> String {
    format!("echo {started_marker} && ping -n 30 127.0.0.1 >NUL\r")
}

pub async fn wait_for_output_marker(stream: &mut PaneOutputStream, marker: &[u8]) -> TestResult {
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("pane output stream did not emit expected marker".into());
        }
        match timeout(remaining, stream.next()).await?? {
            Some(PaneOutputChunk::Bytes { bytes, .. })
                if bytes.windows(marker.len()).any(|window| window == marker) =>
            {
                return Ok(());
            }
            Some(_) => {}
            None => return Err("pane output stream closed before expected marker".into()),
        }
    }
}

pub async fn wait_for_snapshot_text_after_revision(
    pane: &rmux_sdk::Pane,
    previous_revision: u64,
    marker: &str,
) -> TestResult<rmux_sdk::PaneSnapshot> {
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    loop {
        let snapshot = pane.snapshot().await?;
        if snapshot.revision > previous_revision && snapshot.visible_text().contains(marker) {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "snapshot did not advance past revision {previous_revision} with marker {marker:?}"
            )
            .into());
        }
        sleep(Duration::from_millis(25)).await;
    }
}

pub async fn wait_for_stable_snapshot(
    pane: &rmux_sdk::Pane,
    minimum_revision: u64,
) -> TestResult<rmux_sdk::PaneSnapshot> {
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    let mut previous = pane.snapshot().await?;
    let mut stable_since = Instant::now();
    loop {
        sleep(Duration::from_millis(100)).await;
        let current = pane.snapshot().await?;
        let last_revision = current.revision;
        let now = Instant::now();
        if current == previous {
            if current.revision >= minimum_revision
                && now.duration_since(stable_since) >= SNAPSHOT_STABLE_PERIOD
            {
                return Ok(current);
            }
        } else {
            previous = current;
            stable_since = now;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "snapshot did not stabilize after revision {minimum_revision}; last revision was {}",
                last_revision
            )
            .into());
        }
    }
}

pub async fn wait_for_daemon_unavailable(pipe_name: &str) -> TestResult {
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("daemon endpoint remained reachable: {pipe_name}").into());
        }

        let connect =
            builder_with_timeout(pipe_name, DAEMON_UNAVAILABLE_CONNECT_TIMEOUT.min(remaining))
                .connect();
        match timeout(remaining, connect).await {
            Ok(Err(_)) => return Ok(()),
            Ok(Ok(_)) => {}
            Err(_) => {
                return Err(format!("daemon endpoint remained reachable: {pipe_name}").into());
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("daemon endpoint remained reachable: {pipe_name}").into());
        }
        sleep(DAEMON_UNAVAILABLE_POLL_INTERVAL.min(remaining)).await;
    }
}

pub async fn wait_for_pane_absent(pane: &rmux_sdk::Pane) -> TestResult {
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    loop {
        if pane.id().await?.is_none() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("pane remained listed after expected process exit".into());
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn unique_pipe_name(label: &str) -> TestResult<String> {
    let local = format!("sdkv1win{}{}", std::process::id(), unique_id());
    let endpoint = rmux_ipc::endpoint_for_label(format!("{local}{label}"))?;
    Ok(endpoint
        .as_path()
        .as_os_str()
        .to_string_lossy()
        .into_owned())
}

fn unique_id() -> usize {
    UNIQUE_ID.fetch_add(1, Ordering::Relaxed)
}

fn cmd_exe() -> String {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("cmd.exe")
        .to_string_lossy()
        .into_owned()
}

fn rmux_binary() -> TestResult<&'static Path> {
    static RMUX_BINARY: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    match RMUX_BINARY.get_or_init(|| resolve_rmux_binary().map_err(|error| error.to_string())) {
        Ok(path) => Ok(path.as_path()),
        Err(error) => Err(std::io::Error::other(error.clone()).into()),
    }
}

fn resolve_rmux_binary() -> TestResult<PathBuf> {
    if let Some(path) = std::env::var_os(RMUX_SDK_WINDOWS_SMOKE_RMUX_BIN_ENV) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "{RMUX_SDK_WINDOWS_SMOKE_RMUX_BIN_ENV} points to a missing rmux binary: {}",
            path.display()
        )
        .into());
    }

    if let Some(path) = windows_cargo_build::prebuilt_rmux_binary()? {
        return Ok(path);
    }

    if let Some(path) = option_env!("CARGO_BIN_EXE_rmux") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    let target_dir = target_dir()?;
    let build_target_dir = windows_cargo_build::private_target_dir(&target_dir);
    let candidate = build_target_dir.join("debug").join("rmux.exe");

    let _cargo_build_guard = windows_cargo_build::acquire(&target_dir)?;

    let output = windows_cargo_build::run_cargo_build_with_lnk1104_retry(|| {
        let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        command
            .arg("build")
            .arg("--bin")
            .arg("rmux")
            .arg("--locked")
            .arg("--manifest-path")
            .arg(workspace_root().join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", &build_target_dir);
        command
    })?;
    windows_cargo_build::emit_command_output(&output)?;
    if !output.status.success() {
        return Err(format!(
            "failed to build rmux binary for Windows SDK smoke: {}",
            output.status
        )
        .into());
    }
    if !candidate.is_file() {
        return Err(format!(
            "rmux binary build succeeded but '{}' was not created",
            candidate.display()
        )
        .into());
    }

    Ok(windows_cargo_build::copy_binary_for_current_process(
        &candidate,
        &target_dir,
    )?)
}

fn target_dir() -> TestResult<PathBuf> {
    if let Some(target_dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return Ok(absolutize_target_dir(PathBuf::from(target_dir)));
    }

    let current = std::env::current_exe()?;
    current
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "test executable is not under a target directory".into())
}

fn absolutize_target_dir(target_dir: PathBuf) -> PathBuf {
    if target_dir.is_absolute() {
        target_dir
    } else {
        workspace_root().join(target_dir)
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("rmux-sdk manifest lives under crates/rmux-sdk")
        .to_path_buf()
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

    fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
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

struct CurrentDirGuard {
    previous: PathBuf,
}

impl CurrentDirGuard {
    fn set(path: &Path) -> std::io::Result<Self> {
        let previous = std::env::current_dir()?;
        std::env::set_current_dir(path)?;
        Ok(Self { previous })
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.previous);
    }
}
