#![cfg(windows)]

#[path = "support/windows_cli_serial.rs"]
mod windows_cli_serial;

use std::error::Error;
use std::ffi::{c_void, OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const SYNCHRONIZE: u32 = 0x0010_0000;
const WAIT_TIMEOUT: u32 = 258;
const CASE_TIMEOUT: Duration = Duration::from_secs(10);
const DESCENDANT_START_TIMEOUT_SECONDS: u64 = 20;
const DESCENDANT_READY_TIMEOUT: Duration = Duration::from_secs(30);
const HOSTED_DAEMON_OPT_IN: (&str, &str) = ("RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB", "1");

type RawHandle = *mut c_void;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> RawHandle;
    fn WaitForSingleObject(handle: RawHandle, milliseconds: u32) -> u32;
    fn CloseHandle(object: RawHandle) -> i32;
}

#[test]
fn windows_pane_liveness_follows_descendant_console_job() -> Result<(), Box<dyn Error>> {
    let _serial = windows_cli_serial::acquire("windows-descendant-liveness")?;
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label();
    let case_dir = TestDirectory::new(&label)?;
    let helpers = DurableHelpers::write(case_dir.path())?;
    let _server = RmuxServerGuard::new(binary.clone(), label.clone());

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "bootstrap",
            "cmd.exe",
            "/D",
            "/Q",
            "/K",
        ],
    )?;
    run_rmux(
        &binary,
        &label,
        ["set-option", "-g", "remain-on-exit", "off"],
    )?;

    assert_descendant_input_and_explicit_kill(&binary, &label, case_dir.path(), &helpers)?;
    assert_natural_exit_remain_modes(&binary, &label)?;
    assert_respawn_tears_down_old_tree(&binary, &label, case_dir.path(), &helpers)?;
    assert_server_shutdown_tears_down_tree(&binary, &label, case_dir.path(), &helpers)?;
    Ok(())
}

fn assert_descendant_input_and_explicit_kill(
    binary: &Path,
    label: &str,
    case_dir: &Path,
    helpers: &DurableHelpers,
) -> Result<(), Box<dyn Error>> {
    let case = spawn_durable(binary, label, "inputcase", case_dir, helpers)?;
    let target = "inputcase:0.0";

    run_rmux(
        binary,
        label,
        ["send-keys", "-t", target, "-l", "W13_SEND_OK"],
    )?;
    run_rmux(binary, label, ["send-keys", "-t", target, "Enter"])?;
    wait_for_file_contains(&case.input_path, "W13_SEND_OK", CASE_TIMEOUT)?;

    run_rmux(
        binary,
        label,
        ["set-buffer", "-b", "w13-input", "W13_PASTE_OK"],
    )?;
    run_rmux(
        binary,
        label,
        ["paste-buffer", "-b", "w13-input", "-t", target],
    )?;
    run_rmux(binary, label, ["send-keys", "-t", target, "Enter"])?;
    wait_for_file_contains(&case.input_path, "W13_PASTE_OK", CASE_TIMEOUT)?;

    run_rmux(binary, label, ["kill-pane", "-t", target])?;
    wait_for_process_exit(case.descendant_pid, CASE_TIMEOUT)?;
    Ok(())
}

fn assert_natural_exit_remain_modes(binary: &Path, label: &str) -> Result<(), Box<dyn Error>> {
    run_rmux(
        binary,
        label,
        [
            "new-session",
            "-d",
            "-s",
            "naturaloff",
            "cmd.exe",
            "/D",
            "/Q",
            "/K",
        ],
    )?;
    run_rmux(
        binary,
        label,
        [
            "set-window-option",
            "-t",
            "naturaloff:0",
            "remain-on-exit",
            "off",
        ],
    )?;
    run_rmux(
        binary,
        label,
        ["send-keys", "-t", "naturaloff:0.0", "-l", "exit 0"],
    )?;
    run_rmux(
        binary,
        label,
        ["send-keys", "-t", "naturaloff:0.0", "Enter"],
    )?;
    wait_for_session_absent(binary, label, "naturaloff", CASE_TIMEOUT)?;

    run_rmux(
        binary,
        label,
        [
            "new-session",
            "-d",
            "-s",
            "naturalon",
            "cmd.exe",
            "/D",
            "/Q",
            "/K",
        ],
    )?;
    run_rmux(
        binary,
        label,
        [
            "set-window-option",
            "-t",
            "naturalon:0",
            "remain-on-exit",
            "on",
        ],
    )?;
    run_rmux(
        binary,
        label,
        ["send-keys", "-t", "naturalon:0.0", "-l", "exit 0"],
    )?;
    run_rmux(binary, label, ["send-keys", "-t", "naturalon:0.0", "Enter"])?;
    wait_for_pane_dead(binary, label, "naturalon:0.0", CASE_TIMEOUT)?;
    run_rmux(binary, label, ["kill-session", "-t", "naturalon"])?;
    Ok(())
}

fn assert_respawn_tears_down_old_tree(
    binary: &Path,
    label: &str,
    case_dir: &Path,
    helpers: &DurableHelpers,
) -> Result<(), Box<dyn Error>> {
    let pane_case = spawn_durable(binary, label, "respawnpane", case_dir, helpers)?;
    let pane_target = "respawnpane:0.0";
    let refused = rmux_output(
        binary,
        label,
        [
            "respawn-pane",
            "-t",
            pane_target,
            "cmd.exe",
            "/D",
            "/Q",
            "/K",
        ],
    )?;
    assert!(
        !refused.status.success(),
        "respawn-pane without -k must reject a pane whose descendant is alive"
    );
    run_rmux(
        binary,
        label,
        [
            "respawn-pane",
            "-k",
            "-t",
            pane_target,
            "cmd.exe",
            "/D",
            "/Q",
            "/K",
        ],
    )?;
    wait_for_process_exit(pane_case.descendant_pid, CASE_TIMEOUT)?;
    assert_eq!(
        display(binary, label, Some("respawnpane:0.0"), "#{pane_dead}")?,
        "0"
    );
    run_rmux(binary, label, ["kill-session", "-t", "respawnpane"])?;

    let window_case = spawn_durable(binary, label, "respawnwindow", case_dir, helpers)?;
    run_rmux(
        binary,
        label,
        [
            "respawn-window",
            "-k",
            "-t",
            "respawnwindow:0",
            "cmd.exe",
            "/D",
            "/Q",
            "/K",
        ],
    )?;
    wait_for_process_exit(window_case.descendant_pid, CASE_TIMEOUT)?;
    assert_eq!(
        display(binary, label, Some("respawnwindow:0.0"), "#{pane_dead}")?,
        "0"
    );
    run_rmux(binary, label, ["kill-session", "-t", "respawnwindow"])?;
    Ok(())
}

fn assert_server_shutdown_tears_down_tree(
    binary: &Path,
    label: &str,
    case_dir: &Path,
    helpers: &DurableHelpers,
) -> Result<(), Box<dyn Error>> {
    let shutdown_case = spawn_durable(binary, label, "shutdowncase", case_dir, helpers)?;
    let server_pid = display_u32(binary, label, None, "#{pid}")?;
    run_rmux(binary, label, ["kill-server"])?;
    wait_for_process_exit(shutdown_case.descendant_pid, CASE_TIMEOUT)?;
    wait_for_process_exit(server_pid, CASE_TIMEOUT)?;
    Ok(())
}

struct DurableCase {
    descendant_pid: u32,
    input_path: PathBuf,
}

fn spawn_durable(
    binary: &Path,
    label: &str,
    session: &str,
    case_dir: &Path,
    helpers: &DurableHelpers,
) -> Result<DurableCase, Box<dyn Error>> {
    let pid_path = case_dir.join(format!("{session}-descendant.pid"));
    let ready_path = case_dir.join(format!("{session}-ready.txt"));
    let input_path = case_dir.join(format!("{session}-input.txt"));
    let leader = helpers.leader.to_string_lossy().into_owned();
    let descendant = helpers.descendant.to_string_lossy().into_owned();
    let pid = pid_path.to_string_lossy().into_owned();
    let ready = ready_path.to_string_lossy().into_owned();
    let input = input_path.to_string_lossy().into_owned();
    let start_timeout = DESCENDANT_START_TIMEOUT_SECONDS.to_string();

    run_rmux(
        binary,
        label,
        [
            "new-session",
            "-d",
            "-s",
            session,
            "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            &leader,
            &descendant,
            &pid,
            &ready,
            &input,
            &start_timeout,
        ],
    )?;

    wait_for_path(&ready_path, DESCENDANT_READY_TIMEOUT)?;
    let descendant_pid = wait_for_pid_file(&pid_path, DESCENDANT_READY_TIMEOUT)?;
    let target = format!("{session}:0.0");
    let leader_pid = display_u32(binary, label, Some(&target), "#{pane_pid}")?;
    wait_for_process_exit(leader_pid, CASE_TIMEOUT)?;
    thread::sleep(Duration::from_millis(500));
    assert!(
        process_is_running(descendant_pid),
        "descendant {descendant_pid} died after direct pane child {leader_pid} exited"
    );
    let pane_dead = display(binary, label, Some(&target), "#{pane_dead}")?;
    assert_eq!(
        pane_dead, "0",
        "pane must remain alive while descendant {descendant_pid} owns its console"
    );

    Ok(DurableCase {
        descendant_pid,
        input_path,
    })
}

struct DurableHelpers {
    leader: PathBuf,
    descendant: PathBuf,
}

impl DurableHelpers {
    fn write(case_dir: &Path) -> io::Result<Self> {
        let leader = case_dir.join("leader.ps1");
        let descendant = case_dir.join("descendant.ps1");
        fs::write(
            &leader,
            r#"param(
    [Parameter(Mandatory = $true)][string]$DescendantScript,
    [Parameter(Mandatory = $true)][string]$PidFile,
    [Parameter(Mandatory = $true)][string]$ReadyFile,
    [Parameter(Mandatory = $true)][string]$InputFile,
    [Parameter(Mandatory = $true)][int]$StartTimeoutSeconds
)
$ErrorActionPreference = 'Stop'
$arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$DescendantScript`" `"$ReadyFile`" `"$InputFile`""
$child = Start-Process -FilePath "$PSHOME\powershell.exe" -ArgumentList $arguments -NoNewWindow -PassThru
[System.IO.File]::WriteAllText($PidFile, "$($child.Id)`n")
$deadline = [DateTime]::UtcNow.AddSeconds($StartTimeoutSeconds)
while (-not (Test-Path -LiteralPath $ReadyFile) -and [DateTime]::UtcNow -lt $deadline) {
    Start-Sleep -Milliseconds 10
}
if (-not (Test-Path -LiteralPath $ReadyFile)) {
    Stop-Process -Id $child.Id -Force -ErrorAction SilentlyContinue
    exit 3
}
"#,
        )?;
        fs::write(
            &descendant,
            r#"param(
    [Parameter(Mandatory = $true)][string]$ReadyFile,
    [Parameter(Mandatory = $true)][string]$InputFile
)
$ErrorActionPreference = 'Stop'
[System.IO.File]::WriteAllText($ReadyFile, "$PID`n")
[Console]::Out.WriteLine('W13_DESCENDANT_READY')
[Console]::Out.Flush()
while ($true) {
    $line = [Console]::In.ReadLine()
    if ($null -eq $line) {
        break
    }
    [System.IO.File]::AppendAllText($InputFile, $line + [Environment]::NewLine)
}
"#,
        )?;
        Ok(Self { leader, descendant })
    }
}

fn run_rmux<I, S>(binary: &Path, label: &str, args: I) -> Result<Output, Box<dyn Error>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = rmux_output(binary, label, args)?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "rmux command failed with {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    Ok(output)
}

fn rmux_output<I, S>(binary: &Path, label: &str, args: I) -> io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect::<Vec<OsString>>();
    Command::new(binary)
        .arg("-L")
        .arg(label)
        .args(args)
        .env(HOSTED_DAEMON_OPT_IN.0, HOSTED_DAEMON_OPT_IN.1)
        .output()
}

fn display(
    binary: &Path,
    label: &str,
    target: Option<&str>,
    format: &str,
) -> Result<String, Box<dyn Error>> {
    let mut args = vec![OsString::from("display-message"), OsString::from("-p")];
    if let Some(target) = target {
        args.extend([OsString::from("-t"), OsString::from(target)]);
    }
    args.push(OsString::from(format));
    let output = run_rmux(binary, label, args)?;
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn display_u32(
    binary: &Path,
    label: &str,
    target: Option<&str>,
    format: &str,
) -> Result<u32, Box<dyn Error>> {
    Ok(display(binary, label, target, format)?.parse()?)
}

fn wait_for_pane_dead(
    binary: &Path,
    label: &str,
    target: &str,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut last = String::new();
    while Instant::now() < deadline {
        last = display(binary, label, Some(target), "#{pane_dead}")?;
        if last == "1" {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("pane {target} did not become dead; last pane_dead={last:?}"),
    )
    .into())
}

fn wait_for_session_absent(
    binary: &Path,
    label: &str,
    session: &str,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let output = rmux_output(binary, label, ["has-session", "-t", session])?;
        if !output.status.success() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("session {session} survived natural exit"),
    )
    .into())
}

fn wait_for_path(path: &Path, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.is_file() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("timed out waiting for {}", path.display()),
    ))
}

fn wait_for_pid_file(path: &Path, timeout: Duration) -> Result<u32, Box<dyn Error>> {
    wait_for_path(path, timeout)?;
    Ok(fs::read_to_string(path)?.trim().parse()?)
}

fn wait_for_file_contains(path: &Path, needle: &str, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut last = String::new();
    while Instant::now() < deadline {
        last = fs::read_to_string(path).unwrap_or_default();
        if last.contains(needle) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "timed out waiting for {needle:?} in {}; last={last:?}",
            path.display()
        ),
    ))
}

fn wait_for_process_exit(process_id: u32, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_is_running(process_id) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("process {process_id} did not exit"),
    ))
}

fn process_is_running(process_id: u32) -> bool {
    let handle = unsafe {
        // SAFETY: OpenProcess validates the PID and returns either a live
        // process handle or null.
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE,
            0,
            process_id,
        )
    };
    if handle.is_null() {
        return false;
    }
    let wait = unsafe {
        // SAFETY: `handle` is live and a zero-timeout wait only observes state.
        WaitForSingleObject(handle, 0)
    };
    unsafe {
        // SAFETY: `handle` was returned by OpenProcess and is closed once.
        let _ = CloseHandle(handle);
    }
    wait == WAIT_TIMEOUT
}

fn unique_label() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("w13-descendant-{}-{nanos}", std::process::id())
}

struct RmuxServerGuard {
    binary: PathBuf,
    label: String,
}

impl RmuxServerGuard {
    fn new(binary: PathBuf, label: String) -> Self {
        Self { binary, label }
    }
}

impl Drop for RmuxServerGuard {
    fn drop(&mut self) {
        let _ = Command::new(&self.binary)
            .arg("-L")
            .arg(&self.label)
            .arg("kill-server")
            .env(HOSTED_DAEMON_OPT_IN.0, HOSTED_DAEMON_OPT_IN.1)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> io::Result<Self> {
        let path = std::env::temp_dir().join(label);
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
