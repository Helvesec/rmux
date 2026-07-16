#![cfg(windows)]

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

#[path = "support/windows_cli_serial.rs"]
mod windows_cli_serial;

#[test]
fn windows_automation_wait_snapshot_and_locator_work_end_to_end() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("automation-cli-windows")?;
    let label = unique_label("automation-cli-windows")?;
    let _server = ServerGuard::new(label.clone());

    assert_success(
        rmux_command(&label)
            .args([
                "new-session",
                "-d",
                "-s",
                "alpha",
                "-x",
                "80",
                "-y",
                "24",
                "cmd.exe",
                "/D",
                "/K",
            ])
            .stdin(Stdio::null())
            .output()?,
        "create automation session",
    )?;
    assert_success(
        rmux_command(&label)
            .args([
                "send-keys",
                "-t",
                "alpha:0.0",
                "echo AUTOMATION_READY",
                "Enter",
            ])
            .stdin(Stdio::null())
            .output()?,
        "send automation marker",
    )?;

    let waited = run_json(
        &label,
        &[
            "wait-pane",
            "-t",
            "alpha:0.0",
            "--text",
            "AUTOMATION_READY",
            "--timeout",
            "5s",
            "--json",
        ],
    )?;
    assert_eq!(waited["schema_version"], 1);
    assert_eq!(waited["ok"], true);

    let snapshot = run_json(&label, &["pane-snapshot", "-t", "alpha:0.0", "--json"])?;
    assert_eq!(snapshot["schema_version"], 1);
    assert_eq!(snapshot["ok"], true);
    assert!(
        snapshot["text"]
            .as_str()
            .expect("snapshot text")
            .contains("AUTOMATION_READY"),
        "snapshot should expose rendered visible text: {snapshot}"
    );

    let locator = run_json(
        &label,
        &[
            "locator",
            "-t",
            "alpha:0.0",
            "--get-by-text",
            "AUTOMATION_READY",
            "--json",
        ],
    )?;
    assert_eq!(locator["schema_version"], 1);
    assert_eq!(locator["ok"], true);
    assert!(locator["count"].as_u64().unwrap_or_default() >= 1);

    assert_success(
        rmux_command(&label)
            .args([
                "expect-pane",
                "-t",
                "alpha:0.0",
                "--get-by-text",
                "AUTOMATION_READY",
                "--visible",
            ])
            .stdin(Stdio::null())
            .output()?,
        "expect automation marker",
    )?;
    Ok(())
}

#[test]
fn windows_nonzero_pane_base_index_preserves_targeted_automation_and_percent_resize(
) -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("automation-cli-windows-pane-base-index")?;
    let label = unique_label("automation-cli-windows-pane-base-index")?;
    let _server = ServerGuard::new(label.clone());

    assert_success(
        rmux_command(&label)
            .args([
                "new-session",
                "-d",
                "-s",
                "alpha",
                "-x",
                "80",
                "-y",
                "24",
                "cmd.exe",
                "/D",
                "/K",
            ])
            .stdin(Stdio::null())
            .output()?,
        "create pane-base-index session",
    )?;
    assert_success(
        rmux_command(&label)
            .args(["set-window-option", "-t", "alpha:0", "pane-base-index", "1"])
            .stdin(Stdio::null())
            .output()?,
        "set pane-base-index",
    )?;
    assert_success(
        rmux_command(&label)
            .args(["split-window", "-h", "-t", "alpha:0.1"])
            .stdin(Stdio::null())
            .output()?,
        "split second visible pane",
    )?;

    assert_success(
        rmux_command(&label)
            .args([
                "send-keys",
                "-t",
                "alpha:0.1",
                "echo PANE_ONE_MARKER",
                "Enter",
            ])
            .stdin(Stdio::null())
            .output()?,
        "write first pane marker",
    )?;
    assert_success(
        rmux_command(&label)
            .args([
                "wait-pane",
                "-t",
                "alpha:0.1",
                "--text",
                "PANE_ONE_MARKER",
                "--timeout",
                "8s",
            ])
            .stdin(Stdio::null())
            .output()?,
        "wait for first pane marker",
    )?;
    assert_success(
        rmux_command(&label)
            .args([
                "send-keys",
                "-t",
                "alpha:0.2",
                "--wait-next-text",
                "PANE_TWO_MARKER",
                "--timeout",
                "8s",
                "--",
                "echo PANE_TWO_MARKER",
                "Enter",
            ])
            .stdin(Stdio::null())
            .output()?,
        "write and wait for second pane marker",
    )?;

    let pane_one = run_json(&label, &["pane-snapshot", "-t", "alpha:0.1", "--json"])?;
    let pane_two = run_json(&label, &["pane-snapshot", "-t", "alpha:0.2", "--json"])?;
    let pane_one_text = pane_one["text"].as_str().expect("pane one snapshot text");
    let pane_two_text = pane_two["text"].as_str().expect("pane two snapshot text");
    assert!(pane_one_text.contains("PANE_ONE_MARKER"), "{pane_one}");
    assert!(!pane_one_text.contains("PANE_TWO_MARKER"), "{pane_one}");
    assert!(pane_two_text.contains("PANE_TWO_MARKER"), "{pane_two}");
    assert!(!pane_two_text.contains("PANE_ONE_MARKER"), "{pane_two}");

    assert_success(
        rmux_command(&label)
            .args(["resize-pane", "-t", "alpha:0.1", "-x", "60%"])
            .stdin(Stdio::null())
            .output()?,
        "percentage resize first visible pane",
    )?;
    let panes = rmux_command(&label)
        .args([
            "list-panes",
            "-t",
            "alpha:0",
            "-F",
            "#{pane_index}:#{pane_width}",
        ])
        .stdin(Stdio::null())
        .output()?;
    if !panes.status.success() {
        return Err(format!(
            "list pane widths failed: status={:?}\nstdout={}\nstderr={}",
            panes.status.code(),
            String::from_utf8_lossy(&panes.stdout),
            String::from_utf8_lossy(&panes.stderr)
        )
        .into());
    }
    let panes = String::from_utf8(panes.stdout)?;
    let pane_one_width = pane_width(&panes, 1)?;
    let pane_two_width = pane_width(&panes, 2)?;
    assert_eq!(pane_one_width, 48, "unexpected pane widths: {panes:?}");
    assert_ne!(pane_two_width, 48, "resize targeted wrong pane: {panes:?}");

    Ok(())
}

#[test]
fn windows_send_keys_wait_pane_exit_preserves_full_process_status() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("automation-cli-windows-exit-status")?;
    let label = unique_label("automation-cli-windows-exit-status")?;
    let _server = ServerGuard::new(label.clone());

    for exit_status in [0, 7, 513] {
        let session = format!("exit-{exit_status}");
        assert_success(
            rmux_command(&label)
                .args([
                    "new-session",
                    "-d",
                    "-s",
                    &session,
                    "cmd.exe",
                    "/D",
                    "/Q",
                    "/K",
                ])
                .stdin(Stdio::null())
                .output()?,
            format!("create {session}"),
        )?;
        assert_success(
            rmux_command(&label)
                .args([
                    "set-window-option",
                    "-t",
                    &format!("{session}:0"),
                    "remain-on-exit",
                    "on",
                ])
                .stdin(Stdio::null())
                .output()?,
            format!("enable remain-on-exit for {session}"),
        )?;

        let output = rmux_command(&label)
            .args([
                "send-keys",
                "-t",
                &format!("{session}:0.0"),
                "--wait-pane-exit",
                "--timeout",
                "8s",
                "--",
                &format!("exit {exit_status}"),
                "Enter",
            ])
            .stdin(Stdio::null())
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(exit_status),
            "Windows process status must cross the CLI without 8-bit truncation\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

#[test]
fn windows_cmd_c_tail_preserves_quoted_executables_across_spawn_paths() -> Result<(), Box<dyn Error>>
{
    let _serial_guard = windows_cli_serial::acquire("windows-cmd-c-tail")?;
    let label = unique_label("windows-cmd-c-tail")?;
    let _server = ForegroundServerGuard::start(label.clone())?;
    let root = TestRoot::new("rmux cmd tail quoted executable")?;
    let executable = root.path().join("where probe.exe");
    let system_root = std::env::var_os("SystemRoot").ok_or("SystemRoot is not set")?;
    fs::copy(
        PathBuf::from(system_root).join("System32/where.exe"),
        &executable,
    )?;

    assert_success(
        rmux_command(&label)
            .args(["set-option", "-g", "default-shell", "cmd.exe"])
            .output()?,
        "set cmd default shell",
    )?;

    let run_shell_marker = root.path().join("run shell.txt");
    let run_shell = quoted_probe_command(&executable, &run_shell_marker);
    assert_success(
        rmux_command(&label)
            .args(["run-shell", &run_shell])
            .output()?,
        "run-shell quoted executable",
    )?;
    wait_for_file(&run_shell_marker)?;

    let startup_marker = root.path().join("startup shell.txt");
    let startup = quoted_probe_command(&executable, &startup_marker);
    assert_success(
        rmux_command(&label).args(["-c", &startup]).output()?,
        "top-level shell-command quoted executable",
    )?;
    wait_for_file(&startup_marker)?;

    assert_success(
        rmux_command(&label)
            .args(["new-session", "-d", "-s", "cmd-tail", "cmd.exe", "/D", "/K"])
            .output()?,
        "create cmd tail session",
    )?;
    let conpty_marker = root.path().join("conpty shell.txt");
    let conpty = quoted_probe_command(&executable, &conpty_marker);
    assert_success(
        rmux_command(&label)
            .args(["new-window", "-d", "-t", "cmd-tail", &conpty])
            .output()?,
        "ConPTY quoted executable",
    )?;
    wait_for_file(&conpty_marker)?;

    Ok(())
}

fn quoted_probe_command(executable: &Path, output: &Path) -> String {
    format!(
        r#""{}" cmd.exe > "{}""#,
        executable.display(),
        output.display()
    )
}

fn wait_for_file(path: &Path) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.is_file() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(format!("timed out waiting for '{}'", path.display()).into())
}

fn run_json(label: &str, args: &[&str]) -> Result<Value, Box<dyn Error>> {
    let output = rmux_command(label)
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    assert_success(output, args.join(" ")).and_then(|output| {
        serde_json::from_slice::<Value>(&output.stdout)
            .map_err(|error| format!("invalid JSON output for {args:?}: {error}").into())
    })
}

fn pane_width(output: &str, pane_index: u32) -> Result<u16, Box<dyn Error>> {
    let prefix = format!("{pane_index}:");
    let width = output
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .ok_or_else(|| format!("pane {pane_index} missing from {output:?}"))?;
    Ok(width.parse()?)
}

fn rmux_command(label: &str) -> Command {
    let mut command = Command::new(rmux_binary());
    command.arg("-L").arg(label);
    command
}

fn rmux_binary() -> &'static str {
    env!("CARGO_BIN_EXE_rmux")
}

fn unique_label(prefix: &str) -> Result<String, Box<dyn Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!("{prefix}-{}-{nanos}", std::process::id()))
}

fn assert_success(output: Output, context: impl AsRef<str>) -> Result<Output, Box<dyn Error>> {
    if output.status.success() {
        return Ok(output);
    }
    Err(format!(
        "{} failed: status={:?}\nstdout={}\nstderr={}",
        context.as_ref(),
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

struct ServerGuard {
    label: String,
}

impl ServerGuard {
    fn new(label: String) -> Self {
        Self { label }
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = rmux_command(&self.label)
            .arg("kill-server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

struct ForegroundServerGuard {
    label: String,
    child: Child,
}

impl ForegroundServerGuard {
    fn start(label: String) -> Result<Self, Box<dyn Error>> {
        let mut child = Command::new(rmux_binary())
            .args(["-L", &label, "-f", "NUL", "-D"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if rmux_command(&label)
                .args(["-N", "list-sessions"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
                .success()
            {
                return Ok(Self { label, child });
            }
            if let Some(status) = child.try_wait()? {
                return Err(format!("foreground server exited during startup: {status}").into());
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("foreground server did not become ready".into());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for ForegroundServerGuard {
    fn drop(&mut self) {
        let _ = rmux_command(&self.label)
            .arg("kill-server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let path = std::env::temp_dir().join(unique_label(label)?);
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
