#![cfg(windows)]

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmux_pty::{
    write_windows_console_key_batch, ChildCommand, SpawnedPty, TerminalSize, WindowsConsoleKeyEvent,
};

#[path = "support/windows_cli_serial.rs"]
mod windows_cli_serial;

const READY_TIMEOUT: Duration = Duration::from_secs(8);
const HIT_TIMEOUT: Duration = Duration::from_secs(5);
const EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const HIT_BUFFER: &str = "issue96-tmux-sgr-hit";

#[test]
fn tmux_parent_sgr_key_batch_dispatches_the_live_mouse_binding() -> Result<(), Box<dyn Error>> {
    let _serial = windows_cli_serial::acquire("issue96-tmux-sgr-key-batch")?;
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = format!("i96-tmux-sgr-{}", unique_suffix()?);
    let _server = ServerGuard::new(&binary, label.clone());

    assert_success(
        rmux_command(&binary, &label)
            .args(["new-session", "-d", "-s", "inner", "-x", "80", "-y", "24"])
            .arg("cmd.exe")
            .args(["/d", "/q"])
            .stdin(Stdio::null())
            .output()?,
        "create issue #96 session",
    )?;
    for args in [
        ["set-option", "-g", "mouse", "on"].as_slice(),
        ["set-option", "-g", "status", "off"].as_slice(),
        [
            "bind-key",
            "-T",
            "root",
            "MouseDown1Pane",
            "set-buffer",
            "-b",
            HIT_BUFFER,
            "hit",
        ]
        .as_slice(),
    ] {
        assert_success(
            rmux_command(&binary, &label)
                .args(args)
                .stdin(Stdio::null())
                .output()?,
            "configure issue #96 session",
        )?;
    }

    let mut attach = ChildCommand::new(&binary)
        .args(["-L", &label, "attach-session", "-t", "inner"])
        // A real outer tmux exports TMUX. Clear RMUX so the stricter parent
        // detector cannot mistake this for a nested RMUX client.
        .env("RMUX", "")
        .env("TMUX", "/tmp/tmux-reporter/default,1,0")
        .size(TerminalSize::new(80, 24))
        .spawn()?;
    wait_for_attach_client(&binary, &label, READY_TIMEOUT)?;

    let sgr = b"\x1b[<0;11;6M";
    let keys = sgr
        .iter()
        .copied()
        .map(|byte| WindowsConsoleKeyEvent::new(u16::from(byte), 0, u16::from(byte), 0, 1))
        .collect::<Vec<_>>();
    write_windows_console_key_batch(attach.child().pid(), &keys)?;
    wait_for_buffer(&binary, &label, HIT_BUFFER, HIT_TIMEOUT)?;

    let _ = rmux_command(&binary, &label)
        .arg("detach-client")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !wait_for_exit(&mut attach, EXIT_TIMEOUT) {
        let _ = attach.child().terminate_forcefully();
        let _ = attach.child_mut().wait();
    }
    Ok(())
}

fn wait_for_attach_client(
    binary: &Path,
    label: &str,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        let output = rmux_command(binary, label)
            .args(["list-clients", "-t", "inner", "-F", "#{client_session}"])
            .stdin(Stdio::null())
            .output()?;
        if output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.trim() == "inner")
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("issue #96 attach client did not become ready".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_buffer(
    binary: &Path,
    label: &str,
    buffer: &str,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        let output = rmux_command(binary, label)
            .args(["show-buffer", "-b", buffer])
            .stdin(Stdio::null())
            .output()?;
        if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "hit" {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "tmux-relayed SGR key batch did not dispatch the live mouse binding: status={:?}, stderr={:?}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_exit(child: &mut SpawnedPty, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.child_mut().try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            Ok(None) | Err(_) => return false,
        }
    }
}

fn rmux_command(binary: &Path, label: &str) -> Command {
    let mut command = Command::new(binary);
    command.args(["-L", label]);
    command
}

fn assert_success(output: Output, context: &str) -> Result<Output, Box<dyn Error>> {
    if output.status.success() {
        return Ok(output);
    }
    Err(format!(
        "{context} failed: status={:?}, stdout={:?}, stderr={:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

fn unique_suffix() -> Result<String, Box<dyn Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!("{}-{nanos}", std::process::id()))
}

struct ServerGuard<'a> {
    binary: &'a Path,
    label: String,
}

impl<'a> ServerGuard<'a> {
    fn new(binary: &'a Path, label: String) -> Self {
        Self { binary, label }
    }
}

impl Drop for ServerGuard<'_> {
    fn drop(&mut self) {
        let _ = rmux_command(self.binary, &self.label)
            .arg("kill-server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
