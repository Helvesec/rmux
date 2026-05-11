#![cfg(unix)]

use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::widgets::Widget;
use ratatui_rmux::{PaneDriver, PaneState, PaneWidget};
use rmux_sdk::{
    bootstrap::discovery::SDK_DAEMON_BINARY_ENV, EnsureSession, EnsureSessionPolicy, RmuxBuilder,
    SessionName,
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const MARKER: &str = "RMUX_RATATUI_REAL_RENDER";
const SMOKE_ROOT_PREFIX: &str = "rmux-ratatui-render-smoke-";

#[tokio::test]
async fn ratatui_real_daemon_render_smoke() -> TestResult {
    let root = smoke_root()?;
    let socket_path = root.join("daemon.sock");
    let cleanup = Cleanup::new(root.clone(), socket_path.clone());
    let daemon_binary = rmux_binary()?.to_path_buf();
    let _daemon_binary_env = EnvGuard::set(SDK_DAEMON_BINARY_ENV, daemon_binary.as_os_str());
    let _ = fs::remove_dir_all(&root);

    let rmux = RmuxBuilder::new()
        .unix_socket(&socket_path)
        .default_timeout(Duration::from_secs(5))
        .connect_or_start()
        .await?;
    assert_socket(&socket_path)?;

    let session = rmux
        .ensure_session(
            EnsureSession::named(SessionName::new("ratatui_render_smoke")?)
                .policy(EnsureSessionPolicy::CreateOrReuse)
                .detached(true),
        )
        .await?;
    let pane = session.pane(0, 0);
    let mut driver = PaneDriver::new(pane);

    driver.refresh().await?;
    let initial_generation = driver.state().generation;
    let initial_render = render_symbols(driver.state());

    driver.refresh().await?;
    assert_eq!(
        driver.state().generation,
        initial_generation,
        "same daemon snapshot revision should not advance redraw generation"
    );
    assert_eq!(
        render_symbols(driver.state()),
        initial_render,
        "same daemon snapshot revision should render identically"
    );

    driver
        .pane()
        .send_text(format!("printf '{MARKER}\\n'\n"))
        .await?;
    driver.pane().wait_for_text(MARKER).await?;
    driver.refresh().await?;

    let changed_generation = driver.state().generation;
    let changed_render = render_symbols(driver.state());
    assert!(
        changed_generation > initial_generation,
        "visible daemon transition should advance redraw generation"
    );
    assert_ne!(
        changed_render, initial_render,
        "visible daemon transition should change the rendered buffer"
    );
    assert!(
        changed_render.contains(MARKER),
        "rendered buffer did not contain marker {MARKER:?}: {changed_render:?}"
    );

    driver.refresh().await?;
    assert_eq!(
        driver.state().generation,
        changed_generation,
        "no-op refresh after marker should preserve generation"
    );
    assert_eq!(
        render_symbols(driver.state()),
        changed_render,
        "no-op refresh after marker should preserve rendered buffer"
    );

    rmux.shutdown().await?;
    wait_for_path_absent(&socket_path).await?;
    fs::remove_dir(&root)?;
    cleanup.disarm();
    Ok(())
}

fn render_symbols(state: &PaneState) -> String {
    let area = Rect::new(0, 0, 80, 8);
    let mut buffer = Buffer::empty(area);
    PaneWidget::new(state).render(area, &mut buffer);

    let mut symbols = String::new();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                symbols.push_str(cell.symbol());
            }
        }
        symbols.push('\n');
    }
    symbols
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

async fn wait_for_path_absent(path: &Path) -> TestResult {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if !path.exists() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("path remained after shutdown: {}", path.display()).into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn smoke_root() -> TestResult<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let root = PathBuf::from(format!(
        "/tmp/{SMOKE_ROOT_PREFIX}{}-{nanos}",
        std::process::id()
    ));
    if !is_tmp_smoke_root(&root) {
        return Err(format!(
            "ratatui smoke endpoint root must be an absolute /tmp/{SMOKE_ROOT_PREFIX}* path without '.' or '..' components, got {}",
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
    root.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(SMOKE_ROOT_PREFIX))
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
    if candidate.is_file() {
        return Ok(candidate);
    }

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
        return Err(format!("failed to build rmux binary for ratatui smoke: {status}").into());
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
        .expect("ratatui-rmux manifest lives under crates/ratatui-rmux")
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
