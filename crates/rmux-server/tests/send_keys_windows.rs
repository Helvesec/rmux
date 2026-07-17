#![cfg(windows)]

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[path = "../../../tests/support/windows_cargo_build.rs"]
mod windows_cargo_build;
#[path = "../../../tests/support/windows_cli_serial.rs"]
mod windows_cli_serial;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const STEP_TIMEOUT: Duration = Duration::from_secs(60);
static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[test]
fn send_keys_writes_to_the_correct_pane_through_the_windows_pipe() -> TestResult {
    let harness = CliHarness::new("sendkeysone")?;
    let cmd = cmd_exe();
    harness.success_quiet(&["new-session", "-d", "-s", "alpha", cmd.as_str(), "/d", "/q"])?;

    harness.success_quiet(&["send-keys", "-t", "alpha:0.0", "echo send-keys-ok", "Enter"])?;
    let capture = harness.capture_until_contains("alpha:0.0", "send-keys-ok")?;
    assert!(capture.contains("send-keys-ok"));

    harness.failure(&["send-keys", "-t", "missing:0.0", "x"])?;
    harness.finish()
}

#[test]
fn send_keys_targets_the_correct_pane_in_a_multi_pane_session_windows() -> TestResult {
    let harness = CliHarness::new("sendkeysmulti")?;
    let cmd = cmd_exe();
    harness.success_quiet(&["new-session", "-d", "-s", "beta", cmd.as_str(), "/d", "/q"])?;
    harness.success_quiet(&[
        "split-window",
        "-h",
        "-t",
        "beta:0",
        cmd.as_str(),
        "/d",
        "/q",
    ])?;

    harness.success_quiet(&["send-keys", "-t", "beta:0.0", "echo pane-zero", "Enter"])?;
    assert!(harness
        .capture_until_contains("beta:0.0", "pane-zero")?
        .contains("pane-zero"));

    harness.success_quiet(&["send-keys", "-t", "beta:0.1", "echo pane-one", "Enter"])?;
    assert!(harness
        .capture_until_contains("beta:0.1", "pane-one")?
        .contains("pane-one"));
    harness.finish()
}

struct CliHarness {
    _serial_guard: windows_cli_serial::WindowsCliSerialGuard,
    label: String,
    armed: bool,
}

impl CliHarness {
    fn new(label: &str) -> TestResult<Self> {
        let serial_guard = windows_cli_serial::acquire(label)?;
        Ok(Self {
            _serial_guard: serial_guard,
            label: format!("win{}{}", std::process::id(), unique_id(label)),
            armed: true,
        })
    }

    fn success(&self, args: &[&str]) -> TestResult<Output> {
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
        Ok(output)
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

    fn failure(&self, args: &[&str]) -> TestResult<Output> {
        let output = self.run(args)?;
        if output.status.success() {
            return Err(format!("rmux {:?} unexpectedly succeeded", args).into());
        }
        Ok(output)
    }

    fn run(&self, args: &[&str]) -> TestResult<Output> {
        let mut command = Command::new(rmux_binary()?);
        command.arg("-L").arg(&self.label).args(args);
        Ok(command.output()?)
    }

    fn capture_until_contains(&self, target: &str, needle: &str) -> TestResult<String> {
        let deadline = Instant::now() + STEP_TIMEOUT;
        let mut last = String::new();
        while Instant::now() < deadline {
            let output = self.success(&["capture-pane", "-p", "-t", target])?;
            last = String::from_utf8_lossy(&output.stdout).into_owned();
            if last.contains(needle) {
                return Ok(last);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(format!("capture-pane never surfaced {needle:?}; last capture: {last:?}").into())
    }

    fn finish(mut self) -> TestResult {
        self.armed = false;
        self.kill_server();
        Ok(())
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

impl Drop for CliHarness {
    fn drop(&mut self) {
        if self.armed {
            self.kill_server();
        }
    }
}

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
            "failed to build rmux binary for Windows send-keys smoke: {}",
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
        .expect("rmux-server manifest lives under crates/rmux-server")
        .to_path_buf()
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
