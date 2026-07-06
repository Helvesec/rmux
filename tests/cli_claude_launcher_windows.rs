#![cfg(windows)]

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rmux_pty::{ChildCommand, SpawnedPty, TerminalSize};

#[path = "support/windows_cli_serial.rs"]
mod windows_cli_serial;

static UNIQUE_TEST_ID: AtomicUsize = AtomicUsize::new(0);

#[test]
fn claude_launcher_attaches_rmux_session_windows() -> Result<(), Box<dyn Error>> {
    let root = unique_test_dir("attached");
    let bin = root.join("bin");
    fs::create_dir_all(&bin)?;
    compile_fake_claude(&bin.join("claude.exe"))?;
    run_attached_probe(root, bin, FakeClaudeMode::VersionOnly)
}

#[test]
fn claude_launcher_accepts_cmd_only_claude_windows() -> Result<(), Box<dyn Error>> {
    let root = unique_test_dir("attached-cmd");
    let bin = root.join("bin");
    fs::create_dir_all(&bin)?;
    compile_fake_claude(&bin.join("fake-claude.exe"))?;
    fs::write(
        bin.join("claude.cmd"),
        "@echo off\r\n\"%~dp0fake-claude.exe\" %*\r\n",
    )?;
    run_attached_probe(root, bin, FakeClaudeMode::VersionOnly)
}

#[test]
fn claude_launcher_creates_mate_through_tmux_inheritance() -> Result<(), Box<dyn Error>> {
    let root = unique_test_dir("attached-swarm-fallback");
    let bin = root.join("bin");
    fs::create_dir_all(&bin)?;
    compile_fake_claude(&bin.join("claude.exe"))?;
    run_attached_probe(root, bin, FakeClaudeMode::CreateMateThroughTmuxInheritance)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeClaudeMode {
    VersionOnly,
    CreateMateThroughTmuxInheritance,
}

fn run_attached_probe(
    root: PathBuf,
    bin: PathBuf,
    mode: FakeClaudeMode,
) -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("claude-launcher-windows")?;
    let rmux = std::env::var_os("RMUX_CLAUDE_TEST_RMUX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_rmux")));
    let path = prepend_to_path(&bin)?;
    let fake_claude_log = root.join("fake-claude.log");
    let local_app_data = root.join("localappdata");
    let mut command = ChildCommand::new(rmux)
        .arg("claude")
        .arg("windows-probe")
        .env("PATH", path)
        .env("LOCALAPPDATA", local_app_data.as_os_str())
        .env("RMUX_CLAUDE_TEST_LOG", fake_claude_log.as_os_str())
        .env("RMUX", r"\\.\pipe\outer-rmux,123,0")
        .env("TMUX", r"\\.\pipe\outer-tmux,123,0")
        .env("RMUX_PANE", "%outer")
        .env("TMUX_PANE", "%outer")
        .size(TerminalSize::new(100, 30));
    if mode == FakeClaudeMode::CreateMateThroughTmuxInheritance {
        command = command.env("RMUX_CLAUDE_TEST_CREATE_SWARM_FALLBACK", "1");
    }
    let mut attached = command.spawn()?;

    let mut needles = vec![b"\x1b[30m\x1b[42m".as_slice()];
    match mode {
        FakeClaudeMode::VersionOnly => {
            needles.push(b"RMUX_CLAUDE_ATTACHED_READY".as_slice());
            needles.push(b"tmux 3.4".as_slice());
            needles.push(b"claude*".as_slice());
        }
        FakeClaudeMode::CreateMateThroughTmuxInheritance => {
            needles.push(b"RMUX_CLAUDE_SWARM_READY".as_slice());
            needles.push(b"mate*".as_slice());
        }
    }

    let output = wait_for_needles_or_error(&mut attached, &needles, Duration::from_secs(45))?;

    let _ = wait_for_needles_or_terminate(&mut attached, &[b"[exited]"], Duration::from_secs(1))?;
    terminate_spawned(&mut attached);
    let fake_log = fs::read_to_string(&fake_claude_log).unwrap_or_default();

    assert!(
        fake_log.contains("--teammate-mode tmux windows-probe"),
        "fake Claude did not receive teammate-mode arguments; log: {}; output: {}",
        fake_log,
        escaped_output(&output)
    );
    if mode == FakeClaudeMode::CreateMateThroughTmuxInheritance {
        assert!(
            fake_log.contains("tmux_env=\\\\.\\pipe\\")
                && fake_log.contains("rmux-claude")
                && fake_log.contains("tmux_pane_env=%"),
            "Windows Claude lead should receive tmux-compatible in-pane env; log: {}; output: {}",
            fake_log,
            escaped_output(&output)
        );
        assert!(
            fake_log.contains("where_tmux_stdout=")
                && fake_log.contains("claude-tmux-shim")
                && fake_log.contains("tmux_version_stdout=tmux 3.4")
                && fake_log.contains("shell_env=")
                && fake_log.to_ascii_lowercase().contains("bash"),
            "fake Claude did not resolve the private tmux shim like Claude Code; log: {}; output: {}",
            fake_log,
            escaped_output(&output)
        );
        assert!(
            fake_log.contains("default_shell_stdout=")
                && fake_log.to_ascii_lowercase().contains("bash")
                && fake_log.contains("new_window_status=exit code: 0")
                && fake_log.contains("posix_send_status=exit code: 0"),
            "fake Claude did not get a POSIX teammate shell through bare tmux inheritance; log: {}; output: {}",
            fake_log,
            escaped_output(&output)
        );
    }
    let _ = fs::remove_dir_all(root);
    Ok(())
}

fn compile_fake_claude(path: &Path) -> Result<(), Box<dyn Error>> {
    let source = path.with_extension("rs");
    fs::write(
        &source,
        r##"
use std::io::{self, Write};
use std::process::Command;
use std::thread;
use std::time::Duration;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let tmux_env = std::env::var("TMUX").unwrap_or_default();
    let tmux_pane_env = std::env::var("TMUX_PANE").unwrap_or_default();
    let shell_env = std::env::var("SHELL").unwrap_or_default();
    let path_keys = std::env::vars()
        .filter(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("|");
    let where_tmux = Command::new("where.exe")
        .arg("tmux")
        .output()
        .expect("where.exe tmux");
    if let Some(log) = std::env::var_os("RMUX_CLAUDE_TEST_LOG") {
        std::fs::write(
            log,
            format!(
                "args={args}\ntmux_env={tmux_env}\ntmux_pane_env={tmux_pane_env}\nshell_env={shell_env}\npath_keys={path_keys}\nwhere_tmux_status={}\nwhere_tmux_stdout={}\nwhere_tmux_stderr={}\n",
                where_tmux.status,
                String::from_utf8_lossy(&where_tmux.stdout).replace('\r', ""),
                String::from_utf8_lossy(&where_tmux.stderr).replace('\r', ""),
            ),
        )
        .expect("write fake Claude log");
    }
    let version = Command::new("tmux")
        .arg("-V")
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).replace('\r', ""))
        .unwrap_or_else(|error| format!("tmux error: {error}"));
    println!("RMUX_CLAUDE_ATTACHED_READY");
    println!("args={args}");
    println!("teams={}", std::env::var("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS").unwrap_or_default());
    println!("tmux_version={}", version.trim());
    if let Some(log) = std::env::var_os("RMUX_CLAUDE_TEST_LOG") {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(log)
            .expect("append fake Claude log");
        writeln!(file, "tmux_version_stdout={}", version.trim()).expect("append version");
    }
    let _ = io::stdout().flush();
    if std::env::var_os("RMUX_CLAUDE_TEST_CREATE_SWARM_FALLBACK").is_some() {
        let default_shell = Command::new("tmux")
            .arg("show-options")
            .arg("-gqv")
            .arg("default-shell")
            .output()
            .expect("tmux show-options default-shell");
        if let Some(log) = std::env::var_os("RMUX_CLAUDE_TEST_LOG") {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(log)
                .expect("append fake Claude log");
            writeln!(file, "default_shell_status={}", default_shell.status).expect("append default shell status");
            writeln!(
                file,
                "default_shell_stdout={}",
                String::from_utf8_lossy(&default_shell.stdout).replace('\r', "").trim()
            )
            .expect("append default shell stdout");
            writeln!(
                file,
                "default_shell_stderr={}",
                String::from_utf8_lossy(&default_shell.stderr).replace('\r', "").trim()
            )
            .expect("append default shell stderr");
        }
        let output = Command::new("tmux")
            .arg("new-window")
            .arg("-d")
            .arg("-P")
            .arg("-n")
            .arg("mate")
            .output()
            .expect("tmux new-window command");
        let window_stdout = String::from_utf8_lossy(&output.stdout).replace('\r', "");
        println!("new_window_status={}", output.status);
        println!("new_window_stdout={window_stdout}");
        if let Some(log) = std::env::var_os("RMUX_CLAUDE_TEST_LOG") {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(log)
                .expect("append fake Claude log");
            writeln!(file, "new_window_status={}", output.status).expect("append new-window status");
            writeln!(file, "new_window_stdout={}", window_stdout.trim()).expect("append new-window stdout");
        }
        if output.status.success() {
            let pane_target = window_stdout
                .split_whitespace()
                .rev()
                .find(|part| part.starts_with('%'))
                .unwrap_or("mate");
            let send_status = Command::new("tmux")
                .arg("send-keys")
                .arg("-t")
                .arg(pane_target)
                .arg("env RMUX_PROBE=ok sh -c 'printf RMUX_CLAUDE_SWARM_READY; sleep 30'")
                .arg("Enter")
                .status()
                .expect("tmux send-keys POSIX command");
            println!("posix_send_status={send_status}");
            if let Some(log) = std::env::var_os("RMUX_CLAUDE_TEST_LOG") {
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(log)
                    .expect("append fake Claude log");
                writeln!(file, "posix_send_status={send_status}").expect("append send status");
            }
        }
        let _ = io::stdout().flush();
    }
    thread::sleep(Duration::from_secs(30));
}
"##,
    )?;
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let status = Command::new(rustc)
        .arg(&source)
        .arg("-O")
        .arg("-o")
        .arg(path)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("rustc failed to compile fake claude with {status}").into())
    }
}

fn prepend_to_path(dir: &Path) -> Result<String, Box<dyn Error>> {
    let original = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&original));
    Ok(std::env::join_paths(paths)?.to_string_lossy().into_owned())
}

fn wait_for_needles_or_error(
    spawned: &mut SpawnedPty,
    needles: &[&[u8]],
    timeout: Duration,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let (found, output) = wait_for_needles_or_terminate(spawned, needles, timeout)?;
    if found {
        return Ok(output);
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "timed out waiting for {:?}; observed output: {}",
            needles
                .iter()
                .map(|needle| String::from_utf8_lossy(needle).into_owned())
                .collect::<Vec<_>>(),
            escaped_output(&output)
        ),
    )
    .into())
}

fn wait_for_needles_or_terminate(
    spawned: &mut SpawnedPty,
    needles: &[&[u8]],
    timeout: Duration,
) -> Result<(bool, Vec<u8>), Box<dyn Error>> {
    let io = spawned.master().try_clone_io()?;
    let needles: Vec<Vec<u8>> = needles.iter().map(|needle| needle.to_vec()).collect();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = read_until_needles(&io, &needles, timeout).map_err(|error| error.to_string());
        let _ = tx.send(result);
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(found)) => Ok(found),
        Ok(Err(error)) => Err(io::Error::other(error).into()),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            terminate_spawned(spawned);
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Ok(result)) => Ok(result),
                Ok(Err(error)) => Err(io::Error::other(error).into()),
                Err(_) => Ok((false, Vec::new())),
            }
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("ConPTY reader thread disconnected").into())
        }
    }
}

fn read_until_needles(
    io: &rmux_pty::PtyIo,
    needles: &[Vec<u8>],
    timeout: Duration,
) -> io::Result<(bool, Vec<u8>)> {
    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let bytes_read = match io.read(&mut buffer) {
            Ok(bytes_read) => bytes_read,
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok((false, output)),
            Err(error) => return Err(error),
        };
        if bytes_read == 0 {
            return Ok((false, output));
        }
        output.extend_from_slice(&buffer[..bytes_read]);
        if needles
            .iter()
            .all(|needle| output.windows(needle.len()).any(|window| window == needle))
        {
            return Ok((true, output));
        }
        if Instant::now() >= deadline {
            return Ok((false, output));
        }
    }
}

fn escaped_output(output: &[u8]) -> String {
    String::from_utf8_lossy(output)
        .chars()
        .flat_map(char::escape_default)
        .collect()
}

fn terminate_spawned(spawned: &mut SpawnedPty) {
    let _ = spawned.child().terminate_forcefully();
    let _ = spawned.child_mut().wait();
}

fn unique_test_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "rmux-claude-{label}-{}-{}",
        std::process::id(),
        UNIQUE_TEST_ID.fetch_add(1, Ordering::Relaxed)
    ))
}
