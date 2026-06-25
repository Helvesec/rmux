#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
use std::collections::BTreeSet;
#[cfg(windows)]
use std::error::Error;
#[cfg(windows)]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::{Child, Command, Output, Stdio};
#[cfg(windows)]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(windows)]
use std::sync::OnceLock;
#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(windows)]
#[path = "../../../tests/support/windows_cargo_build.rs"]
mod windows_cargo_build;
#[cfg(windows)]
#[path = "../../../tests/support/windows_cli_serial.rs"]
mod windows_cli_serial;

#[cfg(windows)]
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

#[cfg(windows)]
const STEP_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(windows)]
static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[cfg(windows)]
#[test]
fn status_interval_refreshes_attached_status_bar_windows() -> TestResult {
    let mut harness = CliHarness::new("statusinterval")?;
    let cmd = cmd_exe();
    harness.success_quiet(&["new-session", "-d", "-s", "alpha", cmd.as_str(), "/d", "/q"])?;

    for (option, value) in [
        ("status-interval", "1"),
        ("status-left", "[#{session_name}] "),
        ("status-right", "tick=%S"),
    ] {
        harness.success_quiet(&["set-option", "-t", "alpha", option, value])?;
    }

    let attach = harness.spawn_attach("alpha")?;
    std::thread::sleep(Duration::from_secs(4));
    harness.kill_server();

    let output = attach.wait_with_timeout(STEP_TIMEOUT)?;
    if !output.status.success() {
        return Err(format!(
            "attach exited with {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let ticks = extract_tick_seconds(&stdout);
    if ticks.len() < 2 {
        return Err(format!(
            "attached status did not refresh tick seconds; ticks={ticks:?}; stdout={stdout:?}"
        )
        .into());
    }

    harness.disarm();
    Ok(())
}

#[cfg(windows)]
struct CliHarness {
    _serial_guard: windows_cli_serial::WindowsCliSerialGuard,
    label: String,
    armed: bool,
}

#[cfg(windows)]
impl CliHarness {
    fn new(label: &str) -> TestResult<Self> {
        let serial_guard = windows_cli_serial::acquire(label)?;
        Ok(Self {
            _serial_guard: serial_guard,
            label: format!("win{}{}", std::process::id(), unique_id(label)),
            armed: true,
        })
    }

    fn success_quiet(&self, args: &[&str]) -> TestResult {
        let output = self.run(args)?;
        if !output.status.success() {
            return Err(format!(
                "rmux {:?} failed with {:?}\nstdout:\n{}\nstderr:\n{}",
                args,
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(())
    }

    fn run(&self, args: &[&str]) -> TestResult<Output> {
        let mut command = Command::new(rmux_binary()?);
        command.arg("-L").arg(&self.label).args(args);
        Ok(command.output()?)
    }

    fn spawn_attach(&self, target: &str) -> TestResult<AttachChild> {
        let child = Command::new(rmux_binary()?)
            .arg("-L")
            .arg(&self.label)
            .arg("attach-session")
            .arg("-t")
            .arg(target)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok(AttachChild { child: Some(child) })
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn kill_server(&self) {
        let _ = Command::new(rmux_binary().unwrap_or_else(|_| Path::new("rmux")))
            .arg("-L")
            .arg(&self.label)
            .arg("kill-server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(windows)]
impl Drop for CliHarness {
    fn drop(&mut self) {
        if self.armed {
            self.kill_server();
        }
    }
}

#[cfg(windows)]
struct AttachChild {
    child: Option<Child>,
}

#[cfg(windows)]
impl AttachChild {
    fn wait_with_timeout(mut self, timeout: Duration) -> TestResult<Output> {
        let deadline = Instant::now() + timeout;
        let mut child = self.child.take().expect("child is present");
        while Instant::now() < deadline {
            if child.try_wait()?.is_some() {
                return Ok(child.wait_with_output()?);
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let _ = child.kill();
        let output = child.wait_with_output()?;
        Err(format!(
            "attach process did not exit before timeout\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
    }
}

#[cfg(windows)]
impl Drop for AttachChild {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
    }
}

#[cfg(windows)]
fn extract_tick_seconds(output: &str) -> BTreeSet<String> {
    let mut ticks = BTreeSet::new();
    let mut remaining = output;
    while let Some(start) = remaining.find("tick=") {
        let tick_start = start + "tick=".len();
        if let Some(tick) = remaining.get(tick_start..tick_start + 2) {
            if tick.bytes().all(|byte| byte.is_ascii_digit()) {
                ticks.insert(tick.to_owned());
            }
        }
        remaining = &remaining[tick_start..];
    }
    ticks
}

#[cfg(windows)]
fn unique_id(label: &str) -> String {
    format!(
        "{}{}",
        UNIQUE_ID.fetch_add(1, Ordering::Relaxed),
        label
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .collect::<String>()
    )
}

#[cfg(windows)]
fn rmux_binary() -> TestResult<&'static Path> {
    static RMUX_BINARY: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    match RMUX_BINARY.get_or_init(|| resolve_rmux_binary().map_err(|error| error.to_string())) {
        Ok(path) => Ok(path.as_path()),
        Err(error) => Err(std::io::Error::other(error.clone()).into()),
    }
}

#[cfg(windows)]
fn resolve_rmux_binary() -> TestResult<PathBuf> {
    if let Some(path) = option_env!("CARGO_BIN_EXE_rmux") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    let target_dir = target_dir()?;
    let candidate = target_dir.join("debug").join("rmux.exe");
    let _cargo_build_guard = windows_cargo_build::acquire()?;
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
        return Err(
            format!("failed to build rmux binary for Windows status smoke: {status}").into(),
        );
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

#[cfg(windows)]
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

#[cfg(windows)]
fn absolutize_target_dir(target_dir: PathBuf) -> PathBuf {
    if target_dir.is_absolute() {
        target_dir
    } else {
        workspace_root().join(target_dir)
    }
}

#[cfg(windows)]
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("rmux-server manifest lives under crates/rmux-server")
        .to_path_buf()
}

#[cfg(windows)]
fn cmd_exe() -> String {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("cmd.exe")
        .to_string_lossy()
        .into_owned()
}

#[cfg(not(windows))]
#[test]
fn status_windows_tests_are_windows_only() {}
