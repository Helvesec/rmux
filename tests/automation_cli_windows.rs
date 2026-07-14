#![cfg(windows)]

use std::error::Error;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

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
