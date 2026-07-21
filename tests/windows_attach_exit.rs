#![cfg(windows)]

use std::error::Error;
use std::fs;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmux_pty::{
    write_windows_console_key, ChildCommand, SpawnedPty, TerminalSize, WindowsConsoleKeyEvent,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

const SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const RAW_CONSOLE_PROBE_READY_TIMEOUT: Duration = Duration::from_secs(20);
const PYTHON_FOREGROUND_READY_TIMEOUT: Duration = Duration::from_secs(30);
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);
const CONPTY_STRESS_OUTPUT_TIMEOUT: Duration = Duration::from_secs(30);
const CTRL_C_SYNTHETIC_ATTEMPTS: usize = 3;
const CTRL_C_SYNTHETIC_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(4);
const CTRL_C_PROMPT_READY_TIMEOUT: Duration = Duration::from_secs(5);
const CTRL_D_SYNTHETIC_ATTEMPTS: usize = 3;
const EXIT_LATENCY_TIMEOUT: Duration = Duration::from_secs(2);
const EXIT_LATENCY_BUDGET: Duration = Duration::from_millis(250);
const EXIT_LATENCY_REGRESSION_BUDGET: Duration = Duration::from_millis(1_500);
const EXIT_LATENCY_REGRESSION_REPETITIONS: usize = 3;
const WINDOWS_CONSOLE_TEST_LOCK_TIMEOUT_MS: u32 = 300_000;

static WINDOWS_CONSOLE_TEST_LOCK: Mutex<()> = Mutex::new(());
static NEXT_LABEL_ID: AtomicUsize = AtomicUsize::new(0);

struct WindowsConsoleTestLock {
    handle: HANDLE,
    _thread_guard: MutexGuard<'static, ()>,
}

fn lock_windows_console_test() -> WindowsConsoleTestLock {
    let thread_guard = WINDOWS_CONSOLE_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let name = wide_null("Local\\RMUXWindowsConsoleTestLock");
    let handle = unsafe {
        // SAFETY: The name is a NUL-terminated UTF-16 string and the default
        // security attributes are intentionally null for a per-user test mutex.
        CreateMutexW(std::ptr::null(), 0, name.as_ptr())
    };
    assert!(
        !handle.is_null(),
        "failed to create Windows console test mutex: {}",
        io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
    );
    let wait = unsafe {
        // SAFETY: `handle` is a valid mutex handle returned by CreateMutexW.
        WaitForSingleObject(handle, WINDOWS_CONSOLE_TEST_LOCK_TIMEOUT_MS)
    };
    assert!(
        matches!(wait, WAIT_OBJECT_0 | WAIT_ABANDONED),
        "timed out waiting for Windows console test mutex: wait={wait}"
    );
    WindowsConsoleTestLock {
        handle,
        _thread_guard: thread_guard,
    }
}

impl Drop for WindowsConsoleTestLock {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: The guard only stores handles after a successful wait.
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn unique_label(prefix: &str) -> String {
    let sequence = NEXT_LABEL_ID.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    format!("{prefix}-{}-{now}-{sequence}", std::process::id())
}

#[derive(Debug)]
struct ControlKeyOutcome {
    returned_to_prompt: bool,
    saw_keyboard_interrupt: bool,
    output: Vec<u8>,
}

impl ControlKeyOutcome {
    fn from_output(returned_to_prompt: bool, output: Vec<u8>) -> Self {
        let saw_keyboard_interrupt = output_contains(&output, b"KeyboardInterrupt");
        Self {
            returned_to_prompt,
            saw_keyboard_interrupt,
            output,
        }
    }
}

#[test]
fn windows_attach_exit_emits_exited_banner() -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-exit");
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
    io.write_all(b"echo RMUX_EXIT_READY\r\n")?;
    wait_for_needle_or_error(&mut attach, b"RMUX_EXIT_READY", SETUP_TIMEOUT)?;
    io.write_all(b"echo RMUX_FINAL_TAIL & exit\r\n")?;

    let (exited, output) = wait_for_needle_or_terminate(&mut attach, b"[exited]", EXIT_TIMEOUT)?;
    terminate_spawned(&mut attach);
    assert!(
        exited,
        "attached Windows exit must print [exited]; observed output: {}",
        escaped_output(&output)
    );
    assert!(
        output_contains(&output, b"RMUX_FINAL_TAIL"),
        "attached Windows exit must drain final ConPTY output before [exited]; observed output: {}",
        escaped_output(&output)
    );

    Ok(())
}

#[test]
fn windows_full_helper_client_shell_handoff_starts_power_shell_pane_when_available(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    let Some(system_root) = std::env::var_os("SystemRoot") else {
        eprintln!("skipping full-helper shell handoff probe because SystemRoot is unavailable");
        return Ok(());
    };
    let powershell = PathBuf::from(system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    if !powershell.is_file() {
        eprintln!("skipping full-helper shell handoff probe because PowerShell is unavailable");
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-helper-shell-handoff");
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    let output = Command::new(&binary)
        .arg("-L")
        .arg(&label)
        .args(["new-session", "-d", "-s", "handoff"])
        .env("RMUX_INTERNAL_PUBLIC_BINARY_PATH", &binary)
        .env("RMUX_INTERNAL_CLIENT_SHELL", &powershell)
        .output()?;
    assert!(
        output.status.success(),
        "new-session with helper shell handoff failed: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let current = run_rmux_output(
        &binary,
        &label,
        [
            "display-message",
            "-p",
            "-t",
            "handoff:0.0",
            "#{pane_current_command}",
        ],
    )?;
    let command_name = String::from_utf8_lossy(&current.stdout);
    let command_name = command_name.trim();
    assert!(
        command_name.eq_ignore_ascii_case("powershell.exe")
            || command_name.eq_ignore_ascii_case("powershell"),
        "helper shell handoff should start a PowerShell pane, got {command_name:?}"
    );

    Ok(())
}

#[test]
fn windows_explicit_cmd_shim_pane_command_runs_through_cmd_wrapper() -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-cmd-shim-pane-command");
    let case_dir = std::env::temp_dir().join(&label);
    fs::create_dir_all(&case_dir)?;
    let script = case_dir.join("opencode.cmd");
    fs::write(
        &script,
        "@echo off\r\n\
         echo RMUX_CMD_SHIM_READY\r\n\
         echo RMUX_CMD_SHIM_ARG1:%~1\r\n\
         echo RMUX_CMD_SHIM_ARG2:%~2\r\n\
         ping -n 30 127.0.0.1 >NUL\r\n",
    )?;
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    let status = Command::new(&binary)
        .arg("-L")
        .arg(&label)
        .args(["new-session", "-d", "-s", "cmdshim"])
        .arg(&script)
        .args(["--pure", "RMUX_ARG_OK"])
        .status()?;
    assert!(
        status.success(),
        "new-session should spawn a .cmd shim pane command through cmd.exe, got {status}"
    );

    wait_for_capture_contains(
        &binary,
        &label,
        "cmdshim:0.0",
        b"RMUX_CMD_SHIM_READY",
        SETUP_TIMEOUT,
    )?;
    let output = wait_for_capture_contains(
        &binary,
        &label,
        "cmdshim:0.0",
        b"RMUX_CMD_SHIM_ARG2:RMUX_ARG_OK",
        SETUP_TIMEOUT,
    )?;
    assert!(
        output_contains(&output, b"RMUX_CMD_SHIM_ARG1:--pure")
            && output_contains(&output, b"RMUX_CMD_SHIM_ARG2:RMUX_ARG_OK"),
        ".cmd shim arguments were not forwarded correctly; observed output: {}",
        escaped_output(&output)
    );

    let _ = fs::remove_dir_all(case_dir);
    Ok(())
}

#[test]
fn windows_cmd_default_shell_script_runs_through_cmd_wrapper() -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-cmd-default-shell");
    let case_dir = std::env::temp_dir().join(&label).join("shell dir");
    fs::create_dir_all(&case_dir)?;
    let script = case_dir.join("custom default.cmd");
    fs::write(
        &script,
        "@echo off\r\n\
         echo RMUX_DEFAULT_CMD_READY\r\n\
         echo RMUX_DEFAULT_CMD_PATH:%~f0\r\n\
         ping -n 30 127.0.0.1 >NUL\r\n",
    )?;
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    run_rmux(&binary, &label, ["start-server"])?;
    let set_default = Command::new(&binary)
        .arg("-L")
        .arg(&label)
        .args(["set-option", "-g", "default-shell"])
        .arg(&script)
        .output()?;
    assert!(
        set_default.status.success(),
        "setting .cmd default-shell failed: {:?}\nstdout:\n{}\nstderr:\n{}",
        set_default.status,
        String::from_utf8_lossy(&set_default.stdout),
        String::from_utf8_lossy(&set_default.stderr)
    );
    run_rmux(&binary, &label, ["new-session", "-d", "-s", "cmddefault"])?;

    let output = wait_for_capture_contains(
        &binary,
        &label,
        "cmddefault:0.0",
        b"custom default.cmd",
        SETUP_TIMEOUT,
    )?;
    assert!(
        output_contains(&output, b"RMUX_DEFAULT_CMD_READY")
            && output_contains(&output, b"RMUX_DEFAULT_CMD_PATH:")
            && output_contains(&output, b"custom default.cmd"),
        ".cmd default-shell path with spaces was not executed through the wrapper; observed output: {}",
        escaped_output(&output)
    );

    let _ = fs::remove_dir_all(case_dir);
    Ok(())
}

#[test]
#[ignore = "timing-sensitive Windows attach latency probe"]
fn windows_attach_exit_command_returns_under_latency_budget() -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-exit-latency");
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
    let _serial = lock_windows_console_test();
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
    let label = unique_label("win-ctrl-d-latency");
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
    write_windows_console_key(attach.child().pid(), WindowsConsoleKeyEvent::ctrl_d())?;
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

#[test]
fn windows_attach_exit_latency_regression_stays_bounded_across_repeats(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-exit-latency-regression");
    let _guard = RmuxServerGuard::new(&binary, label.clone());
    let mut samples = Vec::new();

    for iteration in 0..EXIT_LATENCY_REGRESSION_REPETITIONS {
        let session = format!("exitlat{iteration}");
        run_rmux(
            &binary,
            &label,
            [
                "new-session",
                "-d",
                "-s",
                session.as_str(),
                "cmd.exe",
                "/D",
                "/K",
            ],
        )?;
        run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;

        let mut attach = ChildCommand::new(&binary)
            .args(["-L", &label, "attach-session", "-t", session.as_str()])
            .size(TerminalSize::new(100, 30))
            .spawn()?;
        let io = attach.master().try_clone_io()?;

        wait_for_needle_or_error(&mut attach, b">", SETUP_TIMEOUT)?;
        let started = Instant::now();
        io.write_all(b"exit\r\n")?;
        let exited = wait_for_spawned_exit_or_terminate(&mut attach, EXIT_LATENCY_TIMEOUT)?;
        let elapsed = started.elapsed();
        samples.push(elapsed);

        assert!(
            exited,
            "attached Windows exit command did not return before timeout; samples={samples:?}"
        );
        assert!(
            elapsed <= EXIT_LATENCY_REGRESSION_BUDGET,
            "attached Windows exit command took {elapsed:?}, regression budget is {EXIT_LATENCY_REGRESSION_BUDGET:?}; samples={samples:?}"
        );
    }

    Ok(())
}

#[test]
fn windows_deferred_initial_pane_accepts_immediate_control_actions_and_mutations(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-deferred-controls");
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "deferredctl",
            "cmd.exe",
            "/D",
            "/Q",
            "/K",
        ],
    )?;

    run_rmux(
        &binary,
        &label,
        [
            "send-keys",
            "-t",
            "deferredctl:0.0",
            "C-c",
            "C-d",
            "Enter",
            "echo DEFERRED_CONTROL_AFTER",
            "Enter",
        ],
    )?;
    wait_for_capture_contains(
        &binary,
        &label,
        "deferredctl:0.0",
        b"DEFERRED_CONTROL_AFTER",
        SETUP_TIMEOUT,
    )?;

    let panes = run_rmux_output(
        &binary,
        &label,
        [
            "list-panes",
            "-t",
            "deferredctl",
            "-F",
            "#{pane_pid}:#{pane_current_command}",
        ],
    )?;
    let panes = String::from_utf8_lossy(&panes.stdout);
    let first_pid = panes
        .lines()
        .next()
        .and_then(|line| line.split_once(':'))
        .map(|(pid, _)| pid)
        .unwrap_or_default();
    assert!(
        first_pid.bytes().all(|byte| byte.is_ascii_digit()) && first_pid != "0",
        "deferred pane did not publish a running pane_pid; list-panes output: {panes:?}"
    );

    run_rmux(
        &binary,
        &label,
        [
            "split-window",
            "-h",
            "-t",
            "deferredctl:0.0",
            "cmd.exe /D /Q /K echo DEFERRED_SPLIT_READY",
        ],
    )?;
    wait_for_capture_contains(
        &binary,
        &label,
        "deferredctl:0.1",
        b"DEFERRED_SPLIT_READY",
        SETUP_TIMEOUT,
    )?;
    let panes = run_rmux_output(
        &binary,
        &label,
        ["list-panes", "-t", "deferredctl", "-F", "#{pane_index}"],
    )?;
    assert_eq!(
        String::from_utf8_lossy(&panes.stdout).lines().count(),
        2,
        "split-window after deferred startup should leave two panes"
    );

    Ok(())
}

#[test]
fn windows_conpty_stress_survives_large_output_resize_mouse_toggles_and_detach(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() {
        eprintln!("skipping ConPTY stress probe because pwsh.exe is unavailable");
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-conpty-stress");
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-x",
            "100",
            "-y",
            "30",
            "-s",
            "stress",
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;
    wait_for_capture_contains(&binary, &label, "stress:0.0", b"PS ", SETUP_TIMEOUT)?;

    let command = "Write-Output RMUX_STRESS_BEGIN; 1..800 | ForEach-Object { Write-Output ('RMUX_STRESS_LINE_' + $_) }; $e=[char]27; [Console]::Write($e + '[?1000h' + $e + '[?1006h'); Write-Output RMUX_STRESS_MOUSE_ON; [Console]::Write($e + '[?1006l' + $e + '[?1000l'); Write-Output RMUX_STRESS_DONE";
    run_rmux(
        &binary,
        &label,
        ["send-keys", "-t", "stress:0.0", command, "Enter"],
    )?;

    for (columns, rows) in [(120, 36), (80, 24), (132, 40), (100, 30)] {
        let columns = columns.to_string();
        let rows = rows.to_string();
        run_rmux(
            &binary,
            &label,
            [
                "resize-window",
                "-t",
                "stress:0",
                "-x",
                columns.as_str(),
                "-y",
                rows.as_str(),
            ],
        )?;
    }

    wait_for_capture_contains(
        &binary,
        &label,
        "stress:0.0",
        b"RMUX_STRESS_DONE",
        CONPTY_STRESS_OUTPUT_TIMEOUT,
    )?;

    let mut attach = ChildCommand::new(&binary)
        .args(["-L", &label, "attach-session", "-t", "stress"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    wait_for_needle_or_error(&mut attach, b"RMUX_STRESS_DONE", SETUP_TIMEOUT)?;
    run_rmux(&binary, &label, ["detach-client"])?;
    let detached = wait_for_spawned_exit_or_terminate(&mut attach, EXIT_LATENCY_TIMEOUT)?;
    assert!(
        detached,
        "attached client did not exit after detach-client during ConPTY stress"
    );

    Ok(())
}

#[test]
fn windows_attach_ctrl_c_interrupts_pwsh_foreground_python_when_available(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() || !python_available() {
        eprintln!(
            "skipping Ctrl-C foreground interrupt probe because pwsh.exe or python.exe is unavailable"
        );
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-ctrl-c-interrupt");
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "ctrlccase",
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;

    let mut attach = ChildCommand::new(&binary)
        .args(["-L", &label, "attach-session", "-t", "ctrlccase"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    let io = attach.master().try_clone_io()?;

    wait_for_needle_or_error(&mut attach, b"PS ", SETUP_TIMEOUT)?;
    io.write_all(
        b"python -c \"import time; print('RMUX_CTRL_C_READY', flush=True); time.sleep(10**6)\"\r\n",
    )?;
    wait_for_needle_or_error(
        &mut attach,
        b"RMUX_CTRL_C_READY",
        PYTHON_FOREGROUND_READY_TIMEOUT,
    )?;
    let rmux = send_attach_ctrl_c_and_wait_for_marker(
        &binary,
        &label,
        "ctrlccase:0.0",
        &attach,
        &io,
        "RMUX_CTRL_C_DONE",
    )?;
    terminate_spawned(&mut attach);
    assert!(
        rmux.returned_to_prompt,
        "attached Ctrl-C did not return to the PowerShell prompt\n{}",
        String::from_utf8_lossy(&rmux.output)
    );
    assert_foreground_ctrl_c_observed("attached Ctrl-C", &rmux);

    Ok(())
}

#[test]
fn windows_attach_ctrl_c_interrupts_foreground_python_descendant_when_available(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() || !python_available() {
        eprintln!(
            "skipping Ctrl-C descendant interrupt probe because pwsh.exe or python.exe is unavailable"
        );
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-ctrl-c-descendant");
    let _guard = RmuxServerGuard::new(&binary, label.clone());
    let script = write_python_descendant_sleep_script(&label)?;

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "descctrlc",
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;

    let mut attach = ChildCommand::new(&binary)
        .args(["-L", &label, "attach-session", "-t", "descctrlc"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    let io = attach.master().try_clone_io()?;

    wait_for_needle_or_error(&mut attach, b"PS ", SETUP_TIMEOUT)?;
    io.write_all(format!("python \"{}\"\r\n", script.display()).as_bytes())?;
    wait_for_needle_or_error(
        &mut attach,
        b"RMUX_DESC_CHILD_READY",
        PYTHON_FOREGROUND_READY_TIMEOUT,
    )?;
    let rmux = send_attach_ctrl_c_and_wait_for_marker(
        &binary,
        &label,
        "descctrlc:0.0",
        &attach,
        &io,
        "RMUX_DESC_CTRL_C_DONE",
    )?;
    terminate_spawned(&mut attach);
    assert!(
        rmux.returned_to_prompt,
        "attached Ctrl-C did not return after descendant foreground process\n{}",
        String::from_utf8_lossy(&rmux.output)
    );
    assert_foreground_ctrl_c_observed("attached descendant Ctrl-C", &rmux);

    Ok(())
}

#[test]
fn windows_send_keys_ctrl_c_interrupts_pwsh_foreground_python_when_available(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() || !python_available() {
        eprintln!("skipping send-keys Ctrl-C foreground interrupt probe because pwsh.exe or python.exe is unavailable");
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-send-ctrl-c");
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "sendctrlc",
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;
    wait_for_capture_contains(&binary, &label, "sendctrlc:0.0", b"PS ", SETUP_TIMEOUT)?;

    run_rmux(
        &binary,
        &label,
        [
            "send-keys",
            "-t",
            "sendctrlc:0.0",
            "python -c \"import time; print('RMUX_PY_READY', flush=True); time.sleep(10**6)\"",
            "Enter",
        ],
    )?;
    wait_for_capture_contains(
        &binary,
        &label,
        "sendctrlc:0.0",
        b"RMUX_PY_READY",
        SETUP_TIMEOUT,
    )?;
    let rmux = send_rmux_ctrl_c_and_wait_for_marker(
        &binary,
        &label,
        "sendctrlc:0.0",
        "RMUX_SEND_KEYS_CTRL_C_DONE",
    )?;
    assert!(
        rmux.returned_to_prompt,
        "send-keys Ctrl-C did not return to the PowerShell prompt\n{}",
        String::from_utf8_lossy(&rmux.output)
    );
    assert_foreground_ctrl_c_observed("send-keys Ctrl-C", &rmux);

    Ok(())
}

#[test]
fn windows_send_keys_ctrl_d_releases_cmd_timeout() -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-send-ctrl-d-timeout");
    let _guard = RmuxServerGuard::new(&binary, label.clone());
    let target = "ctrldtimeout:0.0";
    let prompt = b"RMUX_TIMEOUT_READY>";

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "ctrldtimeout",
            "cmd.exe",
            "/D",
            "/Q",
            "/K",
        ],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;

    run_rmux(
        &binary,
        &label,
        [
            "send-keys",
            "-t",
            target,
            "prompt RMUX_TIMEOUT_READY$G",
            "Enter",
        ],
    )?;
    wait_for_capture_contains(&binary, &label, target, prompt, SETUP_TIMEOUT)?;

    run_rmux(
        &binary,
        &label,
        ["send-keys", "-t", target, "timeout /T 10000", "Enter"],
    )?;
    wait_for_timeout_countdown_started(&binary, &label, target)?;

    let (returned, output) =
        send_ctrl_d_until_cmd_timeout_releases(&binary, &label, target, prompt)?;
    assert!(
        returned,
        "send-keys C-d did not release cmd.exe/timeout.exe; observed output: {}",
        escaped_output(&output)
    );

    Ok(())
}

#[test]
fn windows_send_keys_ctrl_d_releases_cmd_wrapped_shell_command() -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-send-ctrl-d-shell-timeout");
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "ctrldshelltimeout",
            "timeout.exe /T 10000",
        ],
    )?;
    thread::sleep(Duration::from_millis(500));
    run_rmux(
        &binary,
        &label,
        ["send-keys", "-t", "ctrldshelltimeout:0.0", "C-d"],
    )?;

    let deadline = Instant::now() + EXIT_TIMEOUT;
    loop {
        let status = Command::new(&binary)
            .arg("-L")
            .arg(&label)
            .args(["has-session", "-t", "ctrldshelltimeout"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "send-keys C-d did not release a shell command wrapped by cmd.exe"
        );
        thread::sleep(Duration::from_millis(50));
    }

    Ok(())
}

#[test]
fn windows_send_keys_ctrl_d_does_not_release_pwsh_timeout() -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() {
        eprintln!("skipping pwsh Ctrl-D timeout parity probe because pwsh.exe is unavailable");
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-send-ctrl-d-pwsh-timeout");
    let _guard = RmuxServerGuard::new(&binary, label.clone());
    let target = "ctrldpwshtimeout:0.0";
    let prompt = b"RMUX_TIMEOUT_READY>";

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "ctrldpwshtimeout",
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;

    run_rmux(
        &binary,
        &label,
        [
            "send-keys",
            "-t",
            target,
            "function global:prompt { 'RMUX_' + 'TIMEOUT_READY>' }",
            "Enter",
        ],
    )?;
    wait_for_capture_contains(&binary, &label, target, prompt, SETUP_TIMEOUT)?;

    run_rmux(
        &binary,
        &label,
        ["send-keys", "-t", target, "timeout.exe /T 10000", "Enter"],
    )?;
    wait_for_timeout_countdown_started(&binary, &label, target)?;
    run_rmux(&binary, &label, ["send-keys", "-t", target, "C-d"])?;

    let (returned, output) =
        capture_until_occurrences(&binary, &label, target, prompt, 2, Duration::from_secs(2))?;
    assert!(
        !returned,
        "send-keys C-d unexpectedly released pwsh.exe/timeout.exe; observed output: {}",
        escaped_output(&output)
    );

    Ok(())
}

#[test]
fn windows_send_keys_ctrl_d_releases_wsl_python_stdin_under_pwsh_when_available(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() || !wsl_python_available() {
        eprintln!(
            "skipping pwsh -> WSL Ctrl-D stdin probe because pwsh.exe or WSL python3 is unavailable"
        );
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-send-ctrl-d-pwsh-wsl");
    let case_dir = std::env::temp_dir().join(&label);
    fs::create_dir_all(&case_dir)?;
    let script = case_dir.join("wsl_stdin.py");
    let start_marker = case_dir.join("wsl.start");
    let wsl_script = windows_path_to_wsl(&script)?;
    let wsl_start_marker = windows_path_to_wsl(&start_marker)?;
    fs::write(
        &script,
        format!(
            "import pathlib\nimport sys\npathlib.Path({}).write_text('START')\nprint('RMUX_WSL_STDIN_READY', flush=True)\nsys.stdin.read()\nprint('RMUX_WSL_EOF_DONE', flush=True)\n",
            python_literal(&wsl_start_marker)
        ),
    )?;

    let _guard = RmuxServerGuard::new(&binary, label.clone());
    let target = "ctrldpwshwsl:0.0";
    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "ctrldpwshwsl",
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;
    run_rmux(
        &binary,
        &label,
        [
            "send-keys",
            "-t",
            target,
            format!(
                "wsl.exe --exec python3 '{}'",
                wsl_script.replace('\'', "''")
            )
            .as_str(),
            "Enter",
        ],
    )?;
    wait_for_capture_contains(
        &binary,
        &label,
        target,
        b"RMUX_WSL_STDIN_READY",
        SETUP_TIMEOUT,
    )?;
    run_rmux(&binary, &label, ["send-keys", "-t", target, "C-d"])?;

    let (returned, output) =
        capture_until_contains(&binary, &label, target, b"RMUX_WSL_EOF_DONE", EXIT_TIMEOUT)?;
    assert!(
        returned,
        "send-keys C-d did not deliver EOF to WSL stdin under pwsh.exe; observed output: {}",
        escaped_output(&output)
    );

    Ok(())
}

#[test]
fn windows_send_keys_ctrl_d_multi_token_does_not_leak_following_key_after_pwsh_timeout(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() {
        eprintln!("skipping pwsh multi-token Ctrl-D timeout probe because pwsh.exe is unavailable");
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-send-ctrl-d-pwsh-multi");
    let _guard = RmuxServerGuard::new(&binary, label.clone());
    let target = "ctrldpwshmulti:0.0";
    let prompt = b"RMUX_TIMEOUT_READY>";

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "ctrldpwshmulti",
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;

    run_rmux(
        &binary,
        &label,
        [
            "send-keys",
            "-t",
            target,
            "function global:prompt { 'RMUX_' + 'TIMEOUT_READY>' }",
            "Enter",
        ],
    )?;
    wait_for_capture_contains(&binary, &label, target, prompt, SETUP_TIMEOUT)?;

    run_rmux(
        &binary,
        &label,
        ["send-keys", "-t", target, "timeout.exe /T 10000", "Enter"],
    )?;
    wait_for_timeout_countdown_started(&binary, &label, target)?;
    run_rmux(&binary, &label, ["send-keys", "-t", target, "C-d", "A"])?;

    let (_, output) =
        capture_until_occurrences(&binary, &label, target, prompt, 2, Duration::from_secs(3))?;
    assert!(
        !output_contains(&output, b"RMUX_TIMEOUT_READY>A"),
        "send-keys C-d released pwsh.exe/timeout.exe before the following A; observed output: {}",
        escaped_output(&output)
    );

    Ok(())
}

#[test]
fn windows_send_keys_ctrl_d_recomputes_shell_policy_for_each_synchronized_pane(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() {
        eprintln!("skipping mixed-shell synchronized Ctrl-D probe because pwsh.exe is unavailable");
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-sync-mixed-ctrl-d");
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    assert_mixed_shell_synchronized_ctrl_d(&binary, &label, "syncmixpwsh", "syncmixpwsh:0.1")?;
    assert_mixed_shell_synchronized_ctrl_d(&binary, &label, "syncmixcmd", "syncmixcmd:0.0")?;

    Ok(())
}

fn assert_mixed_shell_synchronized_ctrl_d(
    binary: &Path,
    label: &str,
    session: &str,
    control_target: &str,
) -> Result<(), Box<dyn Error>> {
    let cmd_target = format!("{session}:0.0");
    let pwsh_target = format!("{session}:0.1");
    let cmd_prompt = b"CMD_TIMEOUT_READY>";
    let pwsh_prompt = b"PWSH_TIMEOUT_READY>";

    run_rmux(
        binary,
        label,
        [
            "new-session",
            "-d",
            "-s",
            session,
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
            "split-window",
            "-h",
            "-t",
            cmd_target.as_str(),
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(binary, label, ["set-option", "-g", "status", "off"])?;

    run_rmux(
        binary,
        label,
        [
            "send-keys",
            "-t",
            cmd_target.as_str(),
            "prompt CMD_TIMEOUT_READY$G",
            "Enter",
        ],
    )?;
    wait_for_capture_contains(
        binary,
        label,
        cmd_target.as_str(),
        cmd_prompt,
        SETUP_TIMEOUT,
    )?;
    run_rmux(
        binary,
        label,
        [
            "send-keys",
            "-t",
            pwsh_target.as_str(),
            "function global:prompt { 'PWSH_' + 'TIMEOUT_READY>' }",
            "Enter",
        ],
    )?;
    wait_for_capture_contains(
        binary,
        label,
        pwsh_target.as_str(),
        pwsh_prompt,
        SETUP_TIMEOUT,
    )?;

    run_rmux(
        binary,
        label,
        [
            "set-window-option",
            "-t",
            format!("{session}:0").as_str(),
            "synchronize-panes",
            "on",
        ],
    )?;
    run_rmux(
        binary,
        label,
        [
            "send-keys",
            "-t",
            cmd_target.as_str(),
            "timeout.exe /T 10000",
            "Enter",
        ],
    )?;
    wait_for_timeout_countdown_started(binary, label, cmd_target.as_str())?;
    wait_for_timeout_countdown_started(binary, label, pwsh_target.as_str())?;

    let (cmd_returned, cmd_output) = send_synchronized_ctrl_d_until_cmd_timeout_releases(
        binary,
        label,
        control_target,
        cmd_target.as_str(),
        cmd_prompt,
        pwsh_target.as_str(),
        pwsh_prompt,
    )?;
    assert!(
        cmd_returned,
        "synchronized C-d targeted at {control_target} did not release cmd.exe/timeout.exe; observed cmd output: {}",
        escaped_output(&cmd_output)
    );

    Ok(())
}

fn send_synchronized_ctrl_d_until_cmd_timeout_releases(
    binary: &Path,
    label: &str,
    control_target: &str,
    cmd_target: &str,
    cmd_prompt: &[u8],
    pwsh_target: &str,
    pwsh_prompt: &[u8],
) -> Result<(bool, Vec<u8>), Box<dyn Error>> {
    let mut last_cmd_output = Vec::new();

    for attempt in 1..=CTRL_D_SYNTHETIC_ATTEMPTS {
        run_rmux(binary, label, ["send-keys", "-t", control_target, "C-d"])?;

        let (cmd_returned, cmd_output) = capture_until_occurrences(
            binary,
            label,
            cmd_target,
            cmd_prompt,
            2,
            Duration::from_secs(2),
        )?;
        last_cmd_output = cmd_output;

        let (pwsh_returned, pwsh_output) = capture_until_occurrences(
            binary,
            label,
            pwsh_target,
            pwsh_prompt,
            2,
            Duration::from_millis(500),
        )?;
        assert!(
            !pwsh_returned,
            "synchronized C-d attempt {attempt} targeted at {control_target} unexpectedly released pwsh.exe/timeout.exe; observed pwsh output: {}",
            escaped_output(&pwsh_output)
        );

        if cmd_returned {
            return Ok((true, last_cmd_output));
        }
    }

    Ok((false, last_cmd_output))
}

fn send_ctrl_d_until_cmd_timeout_releases(
    binary: &Path,
    label: &str,
    target: &str,
    prompt: &[u8],
) -> Result<(bool, Vec<u8>), Box<dyn Error>> {
    let mut last_output = Vec::new();

    for _ in 0..CTRL_D_SYNTHETIC_ATTEMPTS {
        run_rmux(binary, label, ["send-keys", "-t", target, "C-d"])?;
        let (returned, output) =
            capture_until_occurrences(binary, label, target, prompt, 2, Duration::from_secs(2))?;
        last_output = output;
        if returned {
            return Ok((true, last_output));
        }
    }

    Ok((false, last_output))
}

#[test]
fn windows_send_keys_ctrl_c_targets_only_selected_pane_and_preserves_sibling_and_daemon(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() {
        eprintln!("skipping target-only Ctrl-C probe because pwsh.exe is unavailable");
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-ctrl-c-target-only");
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "targetonly",
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(
        &binary,
        &label,
        [
            "split-window",
            "-h",
            "-t",
            "targetonly:0.0",
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;
    wait_for_capture_contains(&binary, &label, "targetonly:0.0", b"PS ", SETUP_TIMEOUT)?;
    wait_for_capture_contains(&binary, &label, "targetonly:0.1", b"PS ", SETUP_TIMEOUT)?;

    for target in ["targetonly:0.0", "targetonly:0.1"] {
        run_rmux(
            &binary,
            &label,
            ["send-keys", "-t", target, "ping -t 127.0.0.1", "Enter"],
        )?;
        wait_for_capture_contains(&binary, &label, target, b"TTL=", SETUP_TIMEOUT)?;
    }

    let target = send_rmux_ctrl_c_and_wait_for_marker(
        &binary,
        &label,
        "targetonly:0.0",
        "RMUX_TARGET_CTRL_C_DONE",
    )?;
    assert!(
        target.returned_to_prompt,
        "send-keys Ctrl-C did not stop ping in the selected pane\n{}",
        String::from_utf8_lossy(&target.output)
    );

    run_rmux(
        &binary,
        &label,
        [
            "send-keys",
            "-t",
            "targetonly:0.1",
            "Write-Output RMUX_SIBLING_SHOULD_NOT_RUN",
            "Enter",
        ],
    )?;
    let (sibling_returned, sibling_output) = capture_until_contains(
        &binary,
        &label,
        "targetonly:0.1",
        b"RMUX_SIBLING_SHOULD_NOT_RUN",
        Duration::from_millis(900),
    )?;
    assert!(
        !sibling_returned,
        "Ctrl-C sent to pane 0 also released sibling pane 1\n{}",
        String::from_utf8_lossy(&sibling_output)
    );

    let sessions = run_rmux_output(&binary, &label, ["list-sessions", "-F", "#{session_name}"])?;
    assert!(
        String::from_utf8_lossy(&sessions.stdout).contains("targetonly"),
        "daemon/session should remain alive after target-only Ctrl-C"
    );

    Ok(())
}

#[test]
fn windows_attach_ctrl_c_stops_ping_when_available() -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() {
        eprintln!("skipping attach ping Ctrl-C probe because pwsh.exe is unavailable");
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-attach-ping-ctrl-c");
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "pingattach",
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;

    let mut attach = ChildCommand::new(&binary)
        .args(["-L", &label, "attach-session", "-t", "pingattach"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    let io = attach.master().try_clone_io()?;

    wait_for_needle_or_error(&mut attach, b"PS ", SETUP_TIMEOUT)?;
    io.write_all(b"ping -t 127.0.0.1\r\n")?;
    wait_for_needle_or_error(&mut attach, b"TTL=", SETUP_TIMEOUT)?;
    let outcome = send_attach_ctrl_c_and_wait_for_marker(
        &binary,
        &label,
        "pingattach:0.0",
        &attach,
        &io,
        "RMUX_PING_CTRL_C_DONE",
    )?;
    let (displayed, attach_output) = if outcome.returned_to_prompt {
        wait_for_needle_or_terminate(&mut attach, b"RMUX_PING_CTRL_C_DONE", EXIT_TIMEOUT)?
    } else {
        (false, Vec::new())
    };
    terminate_spawned(&mut attach);
    assert!(
        outcome.returned_to_prompt,
        "attached Ctrl-C did not stop ping and return to PowerShell\n{}",
        String::from_utf8_lossy(&outcome.output)
    );
    assert!(
        displayed,
        "attached client did not display the post-Ctrl-C marker\n{}",
        String::from_utf8_lossy(&attach_output)
    );

    Ok(())
}

#[test]
fn windows_send_keys_ctrl_c_multi_token_stops_ping_when_available() -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() {
        eprintln!("skipping send-keys ping Ctrl-C probe because pwsh.exe is unavailable");
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-send-ping-ctrl-c");
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "sendping",
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;
    let initial_capture =
        wait_for_capture_contains(&binary, &label, "sendping:0.0", b"PS ", SETUP_TIMEOUT)?;
    let initial_prompt_occurrences = count_occurrences(&initial_capture, b"PS ");

    run_rmux(
        &binary,
        &label,
        [
            "send-keys",
            "-t",
            "sendping:0.0",
            "ping -t 127.0.0.1",
            "Enter",
        ],
    )?;
    wait_for_capture_contains(&binary, &label, "sendping:0.0", b"TTL=", SETUP_TIMEOUT)?;
    run_rmux(
        &binary,
        &label,
        ["send-keys", "-t", "sendping:0.0", "C-c", "Enter"],
    )?;
    let (prompt_returned, output) = capture_until_occurrences(
        &binary,
        &label,
        "sendping:0.0",
        b"PS ",
        initial_prompt_occurrences + 1,
        CTRL_C_PROMPT_READY_TIMEOUT,
    )?;
    assert!(
        prompt_returned,
        "send-keys Ctrl-C did not return to a fresh PowerShell prompt\n{}",
        String::from_utf8_lossy(&output)
    );
    run_rmux(
        &binary,
        &label,
        [
            "send-keys",
            "-t",
            "sendping:0.0",
            "Write-Output RMUX_PING_CTRL_C_DONE",
            "Enter",
        ],
    )?;

    let (returned, output) = capture_until_contains(
        &binary,
        &label,
        "sendping:0.0",
        b"RMUX_PING_CTRL_C_DONE",
        EXIT_TIMEOUT,
    )?;
    assert!(
        returned,
        "send-keys Ctrl-C did not stop ping and return to PowerShell\n{}",
        String::from_utf8_lossy(&output)
    );

    Ok(())
}

#[test]
fn windows_send_keys_ctrl_c_double_token_interrupts_synchronized_panes_when_available(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() {
        eprintln!("skipping synchronized Ctrl-C probe because pwsh.exe is unavailable");
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-sync-ping-ctrl-c");
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "syncping",
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(
        &binary,
        &label,
        [
            "split-window",
            "-h",
            "-t",
            "syncping:0.0",
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(
        &binary,
        &label,
        [
            "set-window-option",
            "-t",
            "syncping:0",
            "synchronize-panes",
            "on",
        ],
    )?;
    let first_initial_capture =
        wait_for_capture_contains(&binary, &label, "syncping:0.0", b"PS ", SETUP_TIMEOUT)?;
    let second_initial_capture =
        wait_for_capture_contains(&binary, &label, "syncping:0.1", b"PS ", SETUP_TIMEOUT)?;
    let first_initial_prompts = count_occurrences(&first_initial_capture, b"PS ");
    let second_initial_prompts = count_occurrences(&second_initial_capture, b"PS ");

    run_rmux(
        &binary,
        &label,
        [
            "send-keys",
            "-t",
            "syncping:0.0",
            "ping -t 127.0.0.1",
            "Enter",
        ],
    )?;
    wait_for_capture_contains(&binary, &label, "syncping:0.0", b"TTL=", SETUP_TIMEOUT)?;
    wait_for_capture_contains(&binary, &label, "syncping:0.1", b"TTL=", SETUP_TIMEOUT)?;

    run_rmux(
        &binary,
        &label,
        ["send-keys", "-t", "syncping:0.0", "C-c", "C-c"],
    )?;
    for (target, initial_prompts) in [
        ("syncping:0.0", first_initial_prompts),
        ("syncping:0.1", second_initial_prompts),
    ] {
        let (prompt_returned, output) = capture_until_occurrences(
            &binary,
            &label,
            target,
            b"PS ",
            initial_prompts + 1,
            CTRL_C_PROMPT_READY_TIMEOUT,
        )?;
        assert!(
            prompt_returned,
            "synchronized Ctrl-C did not return {target} to a fresh PowerShell prompt\n{}",
            String::from_utf8_lossy(&output)
        );
    }
    run_rmux(
        &binary,
        &label,
        [
            "send-keys",
            "-t",
            "syncping:0.0",
            "Write-Output RMUX_SYNC_CTRL_C_DONE",
            "Enter",
        ],
    )?;
    wait_for_capture_contains(
        &binary,
        &label,
        "syncping:0.0",
        b"RMUX_SYNC_CTRL_C_DONE",
        EXIT_TIMEOUT,
    )?;
    wait_for_capture_contains(
        &binary,
        &label,
        "syncping:0.1",
        b"RMUX_SYNC_CTRL_C_DONE",
        EXIT_TIMEOUT,
    )?;

    Ok(())
}

#[test]
fn windows_attach_ctrl_c_preserves_raw_console_character_when_processed_input_is_off(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() {
        eprintln!("skipping raw Ctrl-C attach probe because pwsh.exe is unavailable");
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-attach-raw-ctrl-c");
    let _guard = RmuxServerGuard::new(&binary, label.clone());
    let script = write_raw_console_probe_script(&label)?;
    let command = format!("pwsh.exe -NoLogo -NoProfile -File {}", script.display());

    run_rmux(
        &binary,
        &label,
        ["new-session", "-d", "-s", "rawattach", command.as_str()],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;

    let mut attach = ChildCommand::new(&binary)
        .args(["-L", &label, "attach-session", "-t", "rawattach"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    let io = attach.master().try_clone_io()?;

    wait_for_needle_or_error(&mut attach, b"RAW_READY", RAW_CONSOLE_PROBE_READY_TIMEOUT)?;
    write_windows_console_key(attach.child().pid(), WindowsConsoleKeyEvent::ctrl_c())?;
    thread::sleep(Duration::from_millis(150));
    io.write_all(b"x")?;

    let (returned, output) =
        wait_for_needle_or_terminate(&mut attach, b"CHAR_0078", EXIT_LATENCY_TIMEOUT)?;
    terminate_spawned(&mut attach);
    assert_raw_ctrl_c_character_observed("attached raw Ctrl-C", returned, &output);

    Ok(())
}

#[test]
fn windows_attach_ctrl_d_preserves_raw_console_character_when_processed_input_is_off(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() {
        eprintln!("skipping raw Ctrl-D attach probe because pwsh.exe is unavailable");
        return Ok(());
    }

    let label = unique_label("win-attach-raw-ctrl-d");
    let script = write_raw_console_probe_script(&label)?;

    let mut direct = ChildCommand::new("pwsh.exe")
        .args(["-NoLogo", "-NoProfile", "-File"])
        .arg(&script)
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    wait_for_needle_or_error(&mut direct, b"RAW_READY", RAW_CONSOLE_PROBE_READY_TIMEOUT)?;
    write_windows_console_key(direct.child().pid(), WindowsConsoleKeyEvent::ctrl_d())?;
    let (direct_received, direct_output) =
        wait_for_needle_or_terminate(&mut direct, b"CHAR_0004", EXIT_LATENCY_TIMEOUT)?;
    terminate_spawned(&mut direct);
    assert!(
        direct_received,
        "direct ConPTY raw PowerShell did not receive Ctrl-D: {}",
        escaped_output(&direct_output)
    );

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let _guard = RmuxServerGuard::new(&binary, label.clone());
    let command = format!("pwsh.exe -NoLogo -NoProfile -File {}", script.display());
    run_rmux(
        &binary,
        &label,
        ["new-session", "-d", "-s", "rawctrld", command.as_str()],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;

    let mut attach = ChildCommand::new(&binary)
        .args(["-L", &label, "attach-session", "-t", "rawctrld"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    let io = attach.master().try_clone_io()?;
    wait_for_needle_or_error(&mut attach, b"RAW_READY", RAW_CONSOLE_PROBE_READY_TIMEOUT)?;
    write_windows_console_key(attach.child().pid(), WindowsConsoleKeyEvent::ctrl_d())?;
    let (rmux_received, rmux_output) = capture_until_contains(
        &binary,
        &label,
        "rawctrld:0.0",
        b"CHAR_0004",
        EXIT_LATENCY_TIMEOUT,
    )?;

    io.write_all(b"x")?;
    wait_for_capture_contains(
        &binary,
        &label,
        "rawctrld:0.0",
        b"CHAR_0078",
        EXIT_LATENCY_TIMEOUT,
    )?;
    terminate_spawned(&mut attach);
    let _ = fs::remove_file(&script);

    assert!(
        rmux_received,
        "attached raw PowerShell did not receive Ctrl-D: {}",
        escaped_output(&rmux_output)
    );
    Ok(())
}

#[test]
fn windows_send_keys_ctrl_d_preserves_raw_console_character_when_processed_input_is_off(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() {
        eprintln!("skipping raw Ctrl-D send-keys probe because pwsh.exe is unavailable");
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-send-raw-ctrl-d");
    let _guard = RmuxServerGuard::new(&binary, label.clone());
    let script = write_raw_console_probe_script(&label)?;
    let command = format!("pwsh.exe -NoLogo -NoProfile -File {}", script.display());

    run_rmux(
        &binary,
        &label,
        ["new-session", "-d", "-s", "rawsendd", command.as_str()],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;
    wait_for_capture_contains(
        &binary,
        &label,
        "rawsendd:0.0",
        b"RAW_READY",
        RAW_CONSOLE_PROBE_READY_TIMEOUT,
    )?;

    run_rmux(
        &binary,
        &label,
        ["send-keys", "-t", "rawsendd:0.0", "C-d", "x"],
    )?;
    let (ctrl_d_received, output) = capture_until_contains(
        &binary,
        &label,
        "rawsendd:0.0",
        b"CHAR_0004",
        EXIT_LATENCY_TIMEOUT,
    )?;
    wait_for_capture_contains(
        &binary,
        &label,
        "rawsendd:0.0",
        b"CHAR_0078",
        EXIT_LATENCY_TIMEOUT,
    )?;
    let _ = fs::remove_file(&script);

    assert!(
        ctrl_d_received,
        "send-keys did not deliver Ctrl-D to raw PowerShell: {}",
        escaped_output(&output)
    );
    Ok(())
}

#[test]
fn windows_send_keys_ctrl_c_preserves_raw_console_character_when_processed_input_is_off(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() {
        eprintln!("skipping raw Ctrl-C send-keys probe because pwsh.exe is unavailable");
        return Ok(());
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-send-raw-ctrl-c");
    let _guard = RmuxServerGuard::new(&binary, label.clone());
    let script = write_raw_console_probe_script(&label)?;
    let command = format!("pwsh.exe -NoLogo -NoProfile -File {}", script.display());

    run_rmux(
        &binary,
        &label,
        ["new-session", "-d", "-s", "rawsend", command.as_str()],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;
    wait_for_capture_contains(
        &binary,
        &label,
        "rawsend:0.0",
        b"RAW_READY",
        RAW_CONSOLE_PROBE_READY_TIMEOUT,
    )?;

    run_rmux(&binary, &label, ["send-keys", "-t", "rawsend:0.0", "C-c"])?;
    wait_for_capture_contains(
        &binary,
        &label,
        "rawsend:0.0",
        b"CHAR_0003",
        EXIT_LATENCY_TIMEOUT,
    )?;
    run_rmux(&binary, &label, ["send-keys", "-t", "rawsend:0.0", "x"])?;

    let (returned, output) = capture_until_contains(
        &binary,
        &label,
        "rawsend:0.0",
        b"CHAR_0078",
        EXIT_LATENCY_TIMEOUT,
    )?;
    assert_raw_ctrl_c_character_observed("send-keys raw Ctrl-C", returned, &output);

    Ok(())
}

#[test]
#[ignore = "host-dependent Windows ConPTY Ctrl-D parity probe"]
fn windows_attach_ctrl_d_matches_direct_pwsh_foreground_python_behavior_when_available(
) -> Result<(), Box<dyn Error>> {
    let _serial = lock_windows_console_test();
    if !pwsh_available() || !python_available() {
        eprintln!(
            "skipping Ctrl-D foreground parity probe because pwsh.exe or python.exe is unavailable"
        );
        return Ok(());
    }
    let native = direct_python_stdin_ctrl_d_outcome()?;

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let label = unique_label("win-ctrl-d-python-eof");
    let _guard = RmuxServerGuard::new(&binary, label.clone());

    run_rmux(
        &binary,
        &label,
        [
            "new-session",
            "-d",
            "-s",
            "ctrldpython",
            "pwsh.exe -NoLogo -NoProfile",
        ],
    )?;
    run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;

    let mut attach = ChildCommand::new(&binary)
        .args(["-L", &label, "attach-session", "-t", "ctrldpython"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    let io = attach.master().try_clone_io()?;

    wait_for_needle_or_error(&mut attach, b"PS ", SETUP_TIMEOUT)?;
    io.write_all(
        b"python -c \"import sys; print('RMUX_EOF_READY', flush=True); sys.stdin.read(); print('EOF_DONE', flush=True)\"\r\n",
    )?;
    wait_for_needle_or_error(&mut attach, b"RMUX_EOF_READY", SETUP_TIMEOUT)?;
    write_windows_console_key(attach.child().pid(), WindowsConsoleKeyEvent::ctrl_d())?;

    let (returned, output) =
        wait_for_needle_or_terminate(&mut attach, b"EOF_DONE", EXIT_LATENCY_TIMEOUT)?;
    terminate_spawned(&mut attach);
    let rmux = ControlKeyOutcome::from_output(returned, output);
    if rmux.returned_to_prompt != native.returned_to_prompt {
        let native_retry = direct_python_stdin_ctrl_d_outcome()?;
        if native_retry.returned_to_prompt != native.returned_to_prompt {
            eprintln!(
                "skipping attached Ctrl-D parity assertion because native Windows ConPTY Ctrl-D behavior is unstable; first={}, retry={}",
                native.returned_to_prompt, native_retry.returned_to_prompt
            );
            return Ok(());
        }
    }
    assert_control_key_parity("attached Ctrl-D", &native, &rmux);

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
        return Err(io::Error::other(format!("rmux command {args:?} failed with {status}")).into());
    }
    Ok(())
}

fn run_rmux_output<const N: usize>(
    binary: &Path,
    label: &str,
    args: [&str; N],
) -> Result<std::process::Output, Box<dyn Error>> {
    let output = Command::new(binary)
        .arg("-L")
        .arg(label)
        .args(args)
        .output()?;
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

fn wait_for_capture_contains(
    binary: &Path,
    label: &str,
    target: &str,
    needle: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut last = Vec::new();
    while Instant::now() < deadline {
        let output = run_rmux_output(binary, label, ["capture-pane", "-p", "-t", target])?;
        last = output.stdout;
        if output_contains(&last, needle) {
            return Ok(last);
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "timed out waiting for {:?} in capture-pane; last capture: {}",
            String::from_utf8_lossy(needle),
            escaped_output(&last)
        ),
    )
    .into())
}

fn capture_until_contains(
    binary: &Path,
    label: &str,
    target: &str,
    needle: &[u8],
    timeout: Duration,
) -> Result<(bool, Vec<u8>), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut last = Vec::new();
    while Instant::now() < deadline {
        let output = run_rmux_output(binary, label, ["capture-pane", "-p", "-t", target])?;
        last = output.stdout;
        if output_contains(&last, needle) {
            return Ok((true, last));
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok((false, last))
}

fn capture_until_occurrences(
    binary: &Path,
    label: &str,
    target: &str,
    needle: &[u8],
    min_occurrences: usize,
    timeout: Duration,
) -> Result<(bool, Vec<u8>), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut last = Vec::new();
    while Instant::now() < deadline {
        let output = run_rmux_output(binary, label, ["capture-pane", "-p", "-t", target])?;
        last = output.stdout;
        if count_occurrences(&last, needle) >= min_occurrences {
            return Ok((true, last));
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok((false, last))
}

fn wait_for_timeout_countdown_started(
    binary: &Path,
    label: &str,
    target: &str,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let deadline = Instant::now() + SETUP_TIMEOUT;
    let mut last = Vec::new();
    while Instant::now() < deadline {
        let output = run_rmux_output(binary, label, ["capture-pane", "-p", "-t", target])?;
        last = output.stdout;
        if timeout_countdown_started(&last) {
            return Ok(last);
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "timed out waiting for timeout.exe countdown to start in {target}; last capture: {}",
            escaped_output(&last)
        ),
    )
    .into())
}

fn timeout_countdown_started(output: &[u8]) -> bool {
    output_contains(output, b"Waiting for") || contains_timeout_countdown_number(output)
}

fn contains_timeout_countdown_number(output: &[u8]) -> bool {
    let mut index = 0;
    while index < output.len() {
        if !output[index].is_ascii_digit() {
            index += 1;
            continue;
        }

        let start = index;
        while index < output.len() && output[index].is_ascii_digit() {
            index += 1;
        }
        if index - start >= 4 && output[start] == b'9' {
            return true;
        }
    }
    false
}

#[test]
fn timeout_countdown_detection_accepts_localized_countdown_text() {
    assert!(timeout_countdown_started(
        b"timeout.exe /T 10000\r\n\r\nAttendre  9994 secondes, appuyez sur une touche pour continuer..."
    ));
    assert!(timeout_countdown_started(
        b"timeout.exe /T 10000\r\n\r\nWaiting for 9999 seconds, press a key to continue ..."
    ));
}

#[test]
fn timeout_countdown_detection_does_not_match_only_the_command_line() {
    assert!(!timeout_countdown_started(b"timeout.exe /T 10000\r\n"));
}

#[test]
fn capture_matching_accepts_soft_wrapped_prompt_markers() {
    let wrapped = b"RMUX_TIMEOUT\r\n_READY>\r\nRMUX_TIMEOUT\r\n_READY>";
    assert!(output_contains(wrapped, b"RMUX_TIMEOUT_READY>"));
    assert_eq!(count_occurrences(wrapped, b"RMUX_TIMEOUT_READY>"), 2);
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    let direct = haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count();
    let unwrapped = terminal_unwrapped_bytes(haystack);
    let unwrapped_count = unwrapped
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count();
    direct.max(unwrapped_count)
}

fn send_attach_ctrl_c_and_wait_for_marker(
    binary: &Path,
    label: &str,
    target: &str,
    attach: &SpawnedPty,
    io: &rmux_pty::PtyIo,
    marker: &str,
) -> Result<ControlKeyOutcome, Box<dyn Error>> {
    let marker_command = format!("\r\nWrite-Output {marker}\r\n");
    let marker_bytes = marker.as_bytes();
    let initial_capture = wait_for_capture_contains(binary, label, target, b"PS ", SETUP_TIMEOUT)?;
    let mut last_capture = initial_capture;

    for _ in 0..CTRL_C_SYNTHETIC_ATTEMPTS {
        // WriteConsoleInput-based Ctrl-C injection is the host-dependent part of
        // this probe: under suite load Windows can occasionally drop one
        // synthetic outer-terminal key event even though the attach/runtime path
        // is healthy. Keep the oracle strict by requiring the pane to execute the
        // marker and to show KeyboardInterrupt/^C, but allow a small number of
        // synthetic injection attempts before declaring the attach path broken.
        let prompt_baseline = count_occurrences(&last_capture, b"PS ");
        write_windows_console_key(attach.child().pid(), WindowsConsoleKeyEvent::ctrl_c())?;
        let (prompt_returned, capture) = capture_until_occurrences(
            binary,
            label,
            target,
            b"PS ",
            prompt_baseline + 1,
            CTRL_C_PROMPT_READY_TIMEOUT,
        )?;
        last_capture = capture;
        if !prompt_returned {
            continue;
        }
        io.write_all(marker_command.as_bytes())?;

        let (found, capture) = capture_until_contains(
            binary,
            label,
            target,
            marker_bytes,
            CTRL_C_SYNTHETIC_ATTEMPT_TIMEOUT,
        )?;
        last_capture = capture;
        if found {
            return Ok(ControlKeyOutcome::from_output(true, last_capture));
        }
    }

    Ok(ControlKeyOutcome::from_output(false, last_capture))
}

fn send_rmux_ctrl_c_and_wait_for_marker(
    binary: &Path,
    label: &str,
    target: &str,
    marker: &str,
) -> Result<ControlKeyOutcome, Box<dyn Error>> {
    let marker_command = format!("Write-Output {marker}");
    let marker_bytes = marker.as_bytes();
    let initial_capture = wait_for_capture_contains(binary, label, target, b"PS ", SETUP_TIMEOUT)?;
    let mut last_capture = initial_capture;

    for _ in 0..CTRL_C_SYNTHETIC_ATTEMPTS {
        let prompt_baseline = count_occurrences(&last_capture, b"PS ");
        run_rmux(binary, label, ["send-keys", "-t", target, "C-c"])?;
        let (prompt_returned, capture) = capture_until_occurrences(
            binary,
            label,
            target,
            b"PS ",
            prompt_baseline + 1,
            CTRL_C_PROMPT_READY_TIMEOUT,
        )?;
        last_capture = capture;
        if !prompt_returned {
            continue;
        }
        run_rmux(
            binary,
            label,
            ["send-keys", "-t", target, marker_command.as_str(), "Enter"],
        )?;

        let (found, capture) = capture_until_contains(
            binary,
            label,
            target,
            marker_bytes,
            CTRL_C_SYNTHETIC_ATTEMPT_TIMEOUT,
        )?;
        last_capture = capture;
        if found {
            return Ok(ControlKeyOutcome::from_output(true, last_capture));
        }
    }

    Ok(ControlKeyOutcome::from_output(false, last_capture))
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

fn python_available() -> bool {
    Command::new("python.exe")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn wsl_python_available() -> bool {
    Command::new("wsl.exe")
        .args(["--exec", "python3", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn windows_path_to_wsl(path: &Path) -> Result<String, Box<dyn Error>> {
    let canonical = if path.exists() {
        fs::canonicalize(path)?
    } else if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let path = canonical.to_string_lossy().replace('\\', "/");
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'/' && bytes[0].is_ascii_alphabetic() {
        let drive = char::from(bytes[0]).to_ascii_lowercase();
        return Ok(format!("/mnt/{drive}/{}", &path[3..]));
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("cannot convert Windows path to WSL path: {path}"),
    )
    .into())
}

fn python_literal(value: &str) -> String {
    format!("{value:?}")
}

fn write_raw_console_probe_script(label: &str) -> Result<PathBuf, Box<dyn Error>> {
    let path = std::env::temp_dir().join(format!("{label}.ps1"));
    fs::write(&path, RAW_CONSOLE_PROBE_SCRIPT)?;
    Ok(path)
}

fn write_python_descendant_sleep_script(label: &str) -> Result<PathBuf, Box<dyn Error>> {
    let path = std::env::temp_dir().join(format!("{label}.py"));
    fs::write(&path, PYTHON_DESCENDANT_SLEEP_SCRIPT)?;
    Ok(path)
}

const RAW_CONSOLE_PROBE_SCRIPT: &str = r#"
[Console]::TreatControlCAsInput = $true
[Console]::Out.WriteLine("RAW_READY")
[Console]::Out.Flush()
while ($true) {
    $key = [Console]::ReadKey($true)
    $code = [int][char]$key.KeyChar
    [Console]::Out.WriteLine(("CHAR_{0:X4}" -f $code))
    [Console]::Out.Flush()
    if ($key.KeyChar -eq 'x') {
        Start-Sleep -Seconds 5
        break
    }
}
"#;

const PYTHON_DESCENDANT_SLEEP_SCRIPT: &str = r#"
import subprocess
import sys

print("RMUX_DESC_PARENT_READY", flush=True)
try:
    subprocess.call([
        sys.executable,
        "-c",
        "import time; print('RMUX_DESC_CHILD_READY', flush=True); time.sleep(10**6)",
    ])
except KeyboardInterrupt:
    print("RMUX_DESC_PARENT_INTERRUPTED", flush=True)
"#;

fn direct_pwsh_ctrl_d_exits() -> Result<bool, Box<dyn Error>> {
    let mut spawned = ChildCommand::new("pwsh.exe")
        .args(["-NoLogo", "-NoProfile"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;

    wait_for_needle_or_error(&mut spawned, b"PS ", SETUP_TIMEOUT)?;
    write_windows_console_key(spawned.child().pid(), WindowsConsoleKeyEvent::ctrl_d())?;
    wait_for_spawned_exit_or_terminate(&mut spawned, Duration::from_secs(1))
}

fn direct_python_stdin_ctrl_d_outcome() -> Result<ControlKeyOutcome, Box<dyn Error>> {
    let mut spawned = ChildCommand::new("pwsh.exe")
        .args(["-NoLogo", "-NoProfile"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    let io = spawned.master().try_clone_io()?;

    wait_for_needle_or_error(&mut spawned, b"PS ", SETUP_TIMEOUT)?;
    io.write_all(
        b"python -c \"import sys; print('RMUX_EOF_READY', flush=True); sys.stdin.read(); print('EOF_DONE', flush=True)\"\r\n",
    )?;
    wait_for_needle_or_error(&mut spawned, b"RMUX_EOF_READY", SETUP_TIMEOUT)?;
    write_windows_console_key(spawned.child().pid(), WindowsConsoleKeyEvent::ctrl_d())?;
    let (returned, output) =
        wait_for_needle_or_terminate(&mut spawned, b"EOF_DONE", EXIT_LATENCY_TIMEOUT)?;
    terminate_spawned(&mut spawned);
    Ok(ControlKeyOutcome::from_output(returned, output))
}

fn assert_control_key_parity(kind: &str, native: &ControlKeyOutcome, rmux: &ControlKeyOutcome) {
    assert_eq!(
        rmux.returned_to_prompt, native.returned_to_prompt,
        "{kind} changed native Windows ConPTY completion behavior\nnative output: {}\nrmux output: {}",
        escaped_output(&native.output),
        escaped_output(&rmux.output)
    );
    assert_eq!(
        rmux.saw_keyboard_interrupt, native.saw_keyboard_interrupt,
        "{kind} changed native Windows Ctrl-C KeyboardInterrupt behavior\nnative output: {}\nrmux output: {}",
        escaped_output(&native.output),
        escaped_output(&rmux.output)
    );
}

fn assert_foreground_ctrl_c_observed(kind: &str, outcome: &ControlKeyOutcome) {
    let output = String::from_utf8_lossy(&outcome.output);
    assert!(
        outcome.returned_to_prompt || outcome.saw_keyboard_interrupt || output.contains("^C"),
        "{kind} did not show a foreground Ctrl-C interruption\n{}",
        output
    );
}

fn assert_raw_ctrl_c_character_observed(kind: &str, returned: bool, output: &[u8]) {
    let output = String::from_utf8_lossy(output);
    assert!(
        returned,
        "{kind} raw console probe did not finish after x\n{output}"
    );
    assert!(
        output.contains("CHAR_0003"),
        "{kind} should deliver Ctrl-C as a raw character when processed input is off\n{output}"
    );
    assert!(
        !output.contains("KeyboardInterrupt") && !output.contains("SIGINT"),
        "{kind} should not synthesize an interrupt for a raw console app\n{output}"
    );
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

fn output_contains(output: &[u8], needle: &[u8]) -> bool {
    output.windows(needle.len()).any(|window| window == needle)
        || terminal_unwrapped_bytes(output)
            .windows(needle.len())
            .any(|window| window == needle)
}

fn terminal_unwrapped_bytes(output: &[u8]) -> Vec<u8> {
    output
        .iter()
        .copied()
        .filter(|byte| !matches!(byte, b'\r' | b'\n'))
        .collect()
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
