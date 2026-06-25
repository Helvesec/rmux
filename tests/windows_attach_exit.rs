#![cfg(windows)]

use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rmux_pty::{
    write_windows_console_key, ChildCommand, SpawnedPty, TerminalSize, WindowsConsoleKeyEvent,
};

const SETUP_TIMEOUT: Duration = Duration::from_secs(6);
const EXIT_TIMEOUT: Duration = Duration::from_secs(6);
const EXIT_LATENCY_TIMEOUT: Duration = Duration::from_secs(2);
const EXIT_LATENCY_BUDGET: Duration = Duration::from_millis(250);

#[test]
fn windows_attach_exit_emits_exited_banner() -> Result<(), Box<dyn Error>> {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = format!("win-exit-{}", std::process::id());
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    run_rmux(
        &binary,
        &label,
        ["new-session", "-d", "-s", "exitcase", "cmd.exe", "/D", "/K"],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;

    let mut attach = ChildCommand::new(&binary)
        .args(["-L", &label, "attach-session", "-t", "exitcase"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    let io = attach.master().try_clone_io()?;

    wait_for_needle_or_error(&mut attach, b">", SETUP_TIMEOUT)?;
    io.write_all(b"exit\r\n")?;

    let (exited, output) = wait_for_needle_or_terminate(&mut attach, b"[exited]", EXIT_TIMEOUT)?;
    terminate_spawned(&mut attach);
    assert!(
        exited,
        "attached Windows exit must print [exited]; observed output: {}",
        escaped_output(&output)
    );

    Ok(())
}

#[test]
#[ignore = "timing-sensitive Windows attach latency probe"]
fn windows_attach_exit_command_returns_under_latency_budget() -> Result<(), Box<dyn Error>> {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = format!("win-exit-latency-{}", std::process::id());
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    run_rmux(
        &binary,
        &label,
        ["new-session", "-d", "-s", "exitcase", "cmd.exe", "/D", "/K"],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;

    let mut attach = ChildCommand::new(&binary)
        .args(["-L", &label, "attach-session", "-t", "exitcase"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    let io = attach.master().try_clone_io()?;

    wait_for_needle_or_error(&mut attach, b">", SETUP_TIMEOUT)?;
    let started = Instant::now();
    io.write_all(b"exit\r\n")?;
    let exited = wait_for_spawned_exit_or_terminate(&mut attach, EXIT_LATENCY_TIMEOUT)?;
    let elapsed = started.elapsed();

    assert!(
        exited,
        "attached Windows exit command did not return before timeout"
    );
    assert!(
        elapsed <= EXIT_LATENCY_BUDGET,
        "attached Windows exit command took {elapsed:?}, budget is {EXIT_LATENCY_BUDGET:?}"
    );

    Ok(())
}

#[test]
#[ignore = "timing-sensitive Windows attach latency probe"]
fn windows_attach_ctrl_d_returns_under_latency_budget_when_pwsh_available(
) -> Result<(), Box<dyn Error>> {
    if !pwsh_available() {
        eprintln!("skipping Ctrl-D latency probe because pwsh.exe is unavailable");
        return Ok(());
    }
    if !direct_pwsh_ctrl_d_exits()? {
        eprintln!(
            "skipping Ctrl-D latency probe because direct pwsh.exe ConPTY does not exit on the injected Ctrl-D"
        );
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = format!("win-ctrl-d-latency-{}", std::process::id());
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "ctrldcase",
            "pwsh.exe",
            "-NoLogo",
            "-NoProfile",
        ],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;

    let mut attach = ChildCommand::new(&binary)
        .args(["-L", &label, "attach-session", "-t", "ctrldcase"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;

    wait_for_needle_or_error(&mut attach, b"PS ", SETUP_TIMEOUT)?;
    let started = Instant::now();
    write_windows_console_key(
        attach.child().pid(),
        WindowsConsoleKeyEvent::new(0x44, 0x20, 0x04, 0x0008, 1),
    )?;
    let exited = wait_for_spawned_exit_or_terminate(&mut attach, EXIT_LATENCY_TIMEOUT)?;
    let elapsed = started.elapsed();

    assert!(
        exited,
        "attached Windows Ctrl-D did not return before timeout"
    );
    assert!(
        elapsed <= EXIT_LATENCY_BUDGET,
        "attached Windows Ctrl-D took {elapsed:?}, budget is {EXIT_LATENCY_BUDGET:?}"
    );

    Ok(())
}

struct RmuxServerGuard<'a> {
    binary: &'a Path,
    label: String,
}

impl<'a> RmuxServerGuard<'a> {
    fn new(binary: &'a Path, label: String) -> Self {
        Self { binary, label }
    }
}

impl Drop for RmuxServerGuard<'_> {
    fn drop(&mut self) {
        let _ = Command::new(self.binary)
            .arg("-L")
            .arg(&self.label)
            .arg("kill-server")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn run_rmux<const N: usize>(
    binary: &Path,
    label: &str,
    args: [&str; N],
) -> Result<(), Box<dyn Error>> {
    let status = Command::new(binary)
        .arg("-L")
        .arg(label)
        .args(args)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!("rmux command failed with {status}")).into());
    }
    Ok(())
}

fn pwsh_available() -> bool {
    Command::new("pwsh.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-Command",
            "$PSVersionTable.PSVersion",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn direct_pwsh_ctrl_d_exits() -> Result<bool, Box<dyn Error>> {
    let mut spawned = ChildCommand::new("pwsh.exe")
        .args(["-NoLogo", "-NoProfile"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;

    wait_for_needle_or_error(&mut spawned, b"PS ", SETUP_TIMEOUT)?;
    write_windows_console_key(
        spawned.child().pid(),
        WindowsConsoleKeyEvent::new(0x44, 0x20, 0x04, 0x0008, 1),
    )?;
    wait_for_spawned_exit_or_terminate(&mut spawned, Duration::from_secs(1))
}

fn wait_for_needle_or_error(
    spawned: &mut SpawnedPty,
    needle: &[u8],
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let (found, output) = wait_for_needle_or_terminate(spawned, needle, timeout)?;
    if found {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "timed out waiting for {:?}; observed output: {}",
            String::from_utf8_lossy(needle),
            escaped_output(&output)
        ),
    )
    .into())
}

fn wait_for_needle_or_terminate(
    spawned: &mut SpawnedPty,
    needle: &[u8],
    timeout: Duration,
) -> Result<(bool, Vec<u8>), Box<dyn Error>> {
    let io = spawned.master().try_clone_io()?;
    let needle = needle.to_vec();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = read_until_io(&io, &needle).map_err(|error| error.to_string());
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

fn read_until_io(io: &rmux_pty::PtyIo, needle: &[u8]) -> io::Result<(bool, Vec<u8>)> {
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
        if output.windows(needle.len()).any(|window| window == needle) {
            return Ok((true, output));
        }
    }
}

fn wait_for_spawned_exit_or_terminate(
    spawned: &mut SpawnedPty,
    timeout: Duration,
) -> Result<bool, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if spawned.child_mut().try_wait()?.is_some() {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            terminate_spawned(spawned);
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(5));
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
