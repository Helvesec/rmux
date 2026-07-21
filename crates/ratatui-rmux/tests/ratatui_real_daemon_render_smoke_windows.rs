#![cfg(windows)]

use std::error::Error;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::widgets::Widget;
use ratatui_rmux::{PaneDriver, PaneState, PaneWidget};
use rmux_sdk::{
    bootstrap::discovery::SDK_DAEMON_BINARY_ENV, EnsureSession, EnsureSessionPolicy, RmuxBuilder,
    SessionName,
};

#[path = "../../../tests/support/windows_cargo_build.rs"]
mod windows_cargo_build;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const MARKER: &str = "RMUX_RATATUI_WINDOWS_RENDER";

#[tokio::test]
async fn ratatui_real_daemon_render_smoke_windows() -> TestResult {
    let pipe_name = unique_pipe_name()?;
    let cleanup = Cleanup::new(pipe_name.clone());
    let daemon_binary = rmux_binary()?.to_path_buf();
    let _daemon_binary_env = EnvGuard::set(SDK_DAEMON_BINARY_ENV, daemon_binary.as_os_str());

    let rmux = builder(&pipe_name).connect_or_start().await?;
    let session = rmux
        .ensure_session(
            EnsureSession::named(SessionName::new("ratatui_render_smoke_windows")?)
                .policy(EnsureSessionPolicy::CreateOrReuse)
                .detached(true)
                .command(cmd_interactive_command()),
        )
        .await?;
    let pane = session.pane(0, 0);
    let mut driver = PaneDriver::new(pane);

    let (initial_generation, initial_render) =
        refresh_until_stable(&mut driver, "initial Windows shell prompt").await?;

    driver.pane().send_text(format!("echo {MARKER}\r")).await?;
    driver.pane().wait_for_text(MARKER).await?;
    let (changed_generation, changed_render) =
        refresh_until_stable(&mut driver, "marker output").await?;

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

    rmux.shutdown().await?;
    wait_for_daemon_unavailable(&pipe_name).await?;
    cleanup.disarm();
    Ok(())
}

async fn refresh_until_stable(driver: &mut PaneDriver, label: &str) -> TestResult<(u64, String)> {
    let mut previous: Option<(u64, String)> = None;

    for _ in 0..80 {
        driver.refresh().await?;
        let current = (driver.state().generation, render_symbols(driver.state()));
        if previous.as_ref() == Some(&current) {
            return Ok(current);
        }
        previous = Some(current);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    Err(format!("daemon snapshot did not stabilize while waiting for {label}").into())
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

fn builder(pipe_name: &str) -> RmuxBuilder {
    RmuxBuilder::new()
        .windows_pipe(pipe_name.to_owned())
        .default_timeout(Duration::from_secs(5))
}

async fn wait_for_daemon_unavailable(pipe_name: &str) -> TestResult {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if builder(pipe_name).connect().await.is_err() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("daemon endpoint remained reachable: {pipe_name}").into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn unique_pipe_name() -> TestResult<String> {
    let label = format!("ratatuiwin{}{}", std::process::id(), unique_suffix());
    let output = Command::new(rmux_binary()?)
        .arg("-L")
        .arg(label)
        .arg("diagnose")
        .arg("--json")
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "rmux diagnose failed while resolving Windows pipe name: {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let json = String::from_utf8(output.stdout)?;
    json_string_field(&json, "socket_path")
        .ok_or_else(|| "rmux diagnose output did not include socket_path".into())
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn json_string_field(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":");
    let start = json.find(&needle)? + needle.len();
    let mut chars = json[start..].trim_start().chars();
    if chars.next()? != '"' {
        return None;
    }

    let mut value = String::new();
    let mut escaped = false;
    for ch in chars {
        if escaped {
            value.push(match ch {
                '"' => '"',
                '\\' => '\\',
                '/' => '/',
                'b' => '\u{0008}',
                'f' => '\u{000c}',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                _ => ch,
            });
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(value);
        } else {
            value.push(ch);
        }
    }
    None
}

fn cmd_interactive_command() -> Vec<String> {
    vec![cmd_exe(), "/d".to_owned(), "/q".to_owned()]
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
            "failed to build rmux binary for ratatui Windows smoke: {}",
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
    pipe_name: String,
    armed: bool,
}

impl Cleanup {
    fn new(pipe_name: String) -> Self {
        Self {
            pipe_name,
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
