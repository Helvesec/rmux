#![cfg(windows)]

use std::error::Error;
use std::io;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmux_pty::{
    write_windows_console_key, ChildCommand, SpawnedPty, TerminalSize, WindowsConsoleKeyEvent,
};
use windows_sys::Win32::System::Console::LEFT_CTRL_PRESSED;

#[path = "support/windows_cli_serial.rs"]
mod windows_cli_serial;

const ATTACH_READY_TIMEOUT: Duration = Duration::from_secs(4);
const ATTACH_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const IO_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn command_prompt_can_chain_choose_tree_through_windows_attach() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("prompt-overlay-chain")?;
    let label = unique_label("prompt-overlay-chain")?;
    let _server = ServerGuard::new(label.clone());

    assert_success(
        rmux_command(&label)
            .args(["new-session", "-d", "-s", "alpha", "cmd.exe", "/D", "/K"])
            .stdin(Stdio::null())
            .output()?,
        "create alpha",
    )?;
    assert_success(
        rmux_command(&label)
            .args([
                "bind-key",
                "X",
                "command-prompt",
                "-p",
                "name:",
                "new-session -d -s '%%' ; choose-tree -Zs",
            ])
            .stdin(Stdio::null())
            .output()?,
        "bind prompt chain",
    )?;

    let attach = AttachGuard::spawn(&label, "alpha")?;
    let io = attach.child.master().try_clone_io()?;
    io.write_all(b"\x02X")?;
    wait_for_output_containing_all(&io, &["name:"], IO_TIMEOUT)?;

    io.write_all(b"prompted\r")?;
    let tree = wait_for_output_containing_all(&io, &["sort:", "alpha", "prompted"], IO_TIMEOUT)?;
    assert!(
        tree.contains("sort:") && tree.contains("alpha") && tree.contains("prompted"),
        "chained choose-tree overlay did not render both sessions; output={tree:?}"
    );

    io.write_all(b"q")?;
    Ok(())
}

#[test]
fn ctrl_semicolon_prefix_dispatches_through_windows_attach() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("ctrl-semicolon-prefix")?;
    let label = unique_label("ctrl-semicolon-prefix")?;
    let _server = ServerGuard::new(label.clone());

    assert_success(
        rmux_command(&label)
            .args(["new-session", "-d", "-s", "alpha", "cmd.exe", "/D", "/K"])
            .stdin(Stdio::null())
            .output()?,
        "create alpha",
    )?;
    assert_success(
        rmux_command(&label)
            .args(["set-option", "-g", "prefix", r"C-\;"])
            .stdin(Stdio::null())
            .output()?,
        "set Ctrl+semicolon prefix",
    )?;
    assert_success(
        rmux_command(&label)
            .args(["bind-key", "X", "new-window", "-d", "-n", "ctrlsemi"])
            .stdin(Stdio::null())
            .output()?,
        "bind prefix X",
    )?;

    let attach = AttachGuard::spawn(&label, "alpha")?;
    let io = attach.child.master().try_clone_io()?;
    write_windows_console_key(
        attach.child.child().pid(),
        WindowsConsoleKeyEvent::new(0xba, 0x27, b';' as u16, LEFT_CTRL_PRESSED, 1),
    )?;
    io.write_all(b"X")?;
    wait_for_window_name(&label, "ctrlsemi", IO_TIMEOUT)?;

    Ok(())
}

struct AttachGuard {
    label: String,
    child: SpawnedPty,
}

impl AttachGuard {
    fn spawn(label: &str, session: &str) -> Result<Self, Box<dyn Error>> {
        let child = ChildCommand::new(rmux_binary())
            .args(["-L", label, "attach-session", "-t", session])
            .size(TerminalSize::new(100, 30))
            .spawn()?;
        wait_for_attach_client(label, session, ATTACH_READY_TIMEOUT)?;
        Ok(Self {
            label: label.to_owned(),
            child,
        })
    }
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        if wait_for_child_exit(&mut self.child, ATTACH_EXIT_TIMEOUT) {
            return;
        }
        let _ = rmux_command(&self.label)
            .arg("detach-client")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if wait_for_child_exit(&mut self.child, ATTACH_EXIT_TIMEOUT) {
            return;
        }
        let _ = self.child.child_mut().terminate_forcefully();
        let _ = self.child.child_mut().wait();
    }
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

fn wait_for_output_containing_all(
    io: &rmux_pty::PtyIo,
    needles: &[&str],
    timeout: Duration,
) -> Result<String, Box<dyn Error>> {
    let io = io.try_clone()?;
    let needles = needles
        .iter()
        .map(|needle| needle.as_bytes().to_vec())
        .collect::<Vec<_>>();
    let needle_names = needles
        .iter()
        .map(|needle| String::from_utf8_lossy(needle).into_owned())
        .collect::<Vec<_>>();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match io.read(&mut buffer) {
                Ok(0) => {
                    let _ = tx.send(Ok(String::from_utf8_lossy(&output).into_owned()));
                    return;
                }
                Ok(bytes_read) => {
                    output.extend_from_slice(&buffer[..bytes_read]);
                    let text = String::from_utf8_lossy(&output).into_owned();
                    let matched = needles.iter().all(|needle| {
                        output
                            .windows(needle.len())
                            .any(|window| window == needle.as_slice())
                    });
                    if tx.send(Ok(text)).is_err() || matched {
                        return;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(error.to_string()));
                    return;
                }
            }
        }
    });

    let deadline = Instant::now() + timeout;
    let mut last_output = String::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining.min(Duration::from_millis(50))) {
            Ok(Ok(output)) => {
                let matched = needle_names.iter().all(|needle| output.contains(needle));
                last_output = output;
                if matched {
                    return Ok(last_output);
                }
            }
            Ok(Err(error)) => return Err(io::Error::other(error).into()),
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "timed out waiting for attach output containing {:?}; last output={last_output:?}",
                        needle_names
                    ),
                )
                .into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("ConPTY reader thread disconnected").into());
            }
        }
    }
}

fn wait_for_attach_client(
    label: &str,
    session: &str,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        let output = rmux_command(label)
            .args(["list-clients", "-t", session, "-F", "#{client_session}"])
            .stdin(Stdio::null())
            .output()?;
        if output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.trim() == session)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "attach client for session {session:?} did not appear before timeout; stdout={:?} stderr={:?}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_window_name(
    label: &str,
    expected_name: &str,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        let output = rmux_command(label)
            .args(["list-windows", "-F", "#{window_name}"])
            .stdin(Stdio::null())
            .output()?;
        if output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.trim() == expected_name)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "window {expected_name:?} did not appear before timeout; stdout={:?} stderr={:?}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_child_exit(child: &mut SpawnedPty, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.child_mut().try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(_) => return true,
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn rmux_command(label: &str) -> Command {
    let mut command = Command::new(rmux_binary());
    command.args(["-L", label]);
    command
}

fn rmux_binary() -> std::path::PathBuf {
    std::env::var_os("RMUX_PROMPT_OVERLAY_RMUX_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_rmux").into())
}

fn assert_success(output: Output, context: &str) -> Result<Output, Box<dyn Error>> {
    if output.status.success() {
        return Ok(output);
    }
    Err(format!(
        "{context} failed: status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

fn unique_label(prefix: &str) -> Result<String, Box<dyn Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!("{prefix}-{}-{nanos}", std::process::id()))
}
