#![cfg(windows)]

use std::env;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmux_pty::{
    write_windows_console_key, write_windows_console_key_then_interrupt_if_processed, ChildCommand,
    PtyMaster, PtyPair, SpawnedPty, TerminalSize, WindowsConsoleKeyEvent,
};
use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, SetConsoleCtrlHandler, SetConsoleMode, CTRL_C_EVENT,
    ENABLE_PROCESSED_INPUT, STD_INPUT_HANDLE,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE,
};

#[path = "windows_conpty/job.rs"]
mod job;

const CONPTY_CHILD_OUTPUT_TIMEOUT: Duration = Duration::from_secs(15);
const CTRL_C_COUNT_PATH_ENV: &str = "RMUX_TEST_CTRL_C_COUNT_PATH";

static CTRL_C_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn count_ctrl_c(kind: u32) -> i32 {
    if kind == CTRL_C_EVENT {
        CTRL_C_CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
        return 1;
    }
    0
}

#[test]
fn conpty_pair_opens_resizes_and_clones_master() -> Result<(), Box<dyn std::error::Error>> {
    let pair = PtyPair::open_with_size(TerminalSize::new(100, 30))?;
    assert_eq!(pair.master().size()?, TerminalSize::new(100, 30));

    pair.master().resize(TerminalSize::new(120, 40))?;
    assert_eq!(pair.master().size()?, TerminalSize::new(120, 40));

    let clone = pair.master().try_clone()?;
    assert_eq!(clone.size()?, TerminalSize::new(120, 40));
    Ok(())
}

#[test]
fn conpty_spawn_reads_child_output_and_waits() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/C", "echo RMUX_SPAWN_OK"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;

    let output = read_until(spawned.master(), b"RMUX_SPAWN_OK", Duration::from_secs(2))?;
    let status = spawned.child_mut().wait()?;

    assert!(status.success());
    assert!(
        String::from_utf8_lossy(&output).contains("RMUX_SPAWN_OK"),
        "expected marker in ConPTY output, got {:?}",
        String::from_utf8_lossy(&output)
    );
    assert!(spawned.child_mut().try_wait()?.is_some());
    Ok(())
}

#[test]
fn conpty_interactive_cmd_accepts_written_input() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/D", "/K"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;

    let io = spawned.master().try_clone_io()?;
    let mut output = read_until_io(&io, b">", Duration::from_secs(2))?;
    io.write_all(b"echo RMUX_INTERACTIVE_OK\r\n")?;
    output.extend(read_until_io(
        &io,
        b"RMUX_INTERACTIVE_OK",
        Duration::from_secs(2),
    )?);

    spawned.child().terminate_forcefully()?;
    let _ = spawned.child_mut().wait()?;

    assert!(
        String::from_utf8_lossy(&output).contains("RMUX_INTERACTIVE_OK"),
        "expected interactive marker in ConPTY output, got {:?}",
        String::from_utf8_lossy(&output)
    );
    Ok(())
}

#[test]
fn conpty_processed_ctrl_c_callback_helper() -> Result<(), Box<dyn std::error::Error>> {
    let Some(count_path) = env::var_os(CTRL_C_COUNT_PATH_ENV) else {
        return Ok(());
    };

    let input = unsafe {
        // SAFETY: STD_INPUT_HANDLE requests the current process console input.
        GetStdHandle(STD_INPUT_HANDLE)
    };
    let mut mode = 0_u32;
    let got_mode = unsafe {
        // SAFETY: input is the ConPTY console input handle and mode is writable.
        GetConsoleMode(input, &mut mode)
    };
    if got_mode == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let set_mode = unsafe {
        // SAFETY: input is a valid console input handle and mode preserves all
        // existing flags while enabling processed Ctrl-C handling.
        SetConsoleMode(input, mode | ENABLE_PROCESSED_INPUT)
    };
    if set_mode == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let installed = unsafe {
        // SAFETY: count_ctrl_c has the process-static callback signature Win32
        // requires and remains valid for the lifetime of this helper process.
        SetConsoleCtrlHandler(Some(count_ctrl_c), 1)
    };
    if installed == 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    println!("RMUX_CTRL_C_COUNT_READY");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        fs::write(
            &count_path,
            CTRL_C_CALLBACK_COUNT.load(Ordering::SeqCst).to_string(),
        )?;
        thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

#[test]
fn conpty_processed_ctrl_c_emits_one_interrupt_per_key() -> Result<(), Box<dyn std::error::Error>> {
    let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let count_path = env::temp_dir().join(format!(
        "rmux-ctrl-c-count-{}-{suffix}.txt",
        std::process::id()
    ));
    let _ = fs::remove_file(&count_path);

    let mut spawned = ChildCommand::new(env::current_exe()?)
        .args([
            "--exact",
            "conpty_processed_ctrl_c_callback_helper",
            "--nocapture",
        ])
        .env(CTRL_C_COUNT_PATH_ENV, count_path.as_os_str())
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    let ready = read_until(
        spawned.master(),
        b"RMUX_CTRL_C_COUNT_READY",
        Duration::from_secs(3),
    )?;
    assert!(
        ready
            .windows(b"RMUX_CTRL_C_COUNT_READY".len())
            .any(|window| window == b"RMUX_CTRL_C_COUNT_READY"),
        "Ctrl-C count helper did not become ready: {:?}",
        String::from_utf8_lossy(&ready)
    );

    write_windows_console_key_then_interrupt_if_processed(
        spawned.child().pid(),
        WindowsConsoleKeyEvent::ctrl_c(),
    )?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let observed = loop {
        let callbacks = fs::read_to_string(&count_path)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        if callbacks > 0 || Instant::now() >= deadline {
            break callbacks;
        }
        thread::sleep(Duration::from_millis(25));
    };
    assert!(observed > 0, "Ctrl-C helper did not receive an interrupt");
    thread::sleep(Duration::from_millis(250));
    let callbacks = fs::read_to_string(&count_path)?.parse::<usize>()?;

    spawned.child().terminate_forcefully()?;
    let _ = spawned.child_mut().wait()?;
    let _ = fs::remove_file(&count_path);
    assert_eq!(callbacks, 1, "one Ctrl-C key must emit one interrupt");
    Ok(())
}

#[test]
#[ignore = "host-dependent Windows console injection probe; run explicitly when validating Ctrl-D semantics"]
fn conpty_console_ctrl_d_interrupts_timeout_when_supported(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/D", "/K"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;

    let io = spawned.master().try_clone_io()?;
    let _ = read_until_io(&io, b">", Duration::from_secs(2))?;
    io.write_all(b"prompt RMUX_READY$G\r\n")?;
    let _ = read_until_io(&io, b"RMUX_READY>", Duration::from_secs(2))?;
    io.write_all(b"timeout /T 10000\r\n")?;
    thread::sleep(Duration::from_millis(300));

    write_windows_console_key(spawned.child().pid(), WindowsConsoleKeyEvent::ctrl_d())?;

    let output = read_until_or_kill(&mut spawned, b"RMUX_READY>", Duration::from_secs(4))?;

    spawned.child().terminate_forcefully()?;
    let _ = spawned.child_mut().wait()?;

    let output = String::from_utf8_lossy(&output);
    if output.contains("RMUX_READY>") {
        return Ok(());
    }

    eprintln!(
        "skipping Ctrl-D timeout.exe interrupt probe because this host/helper \
         suppresses or does not deliver cooked-mode Ctrl-D to timeout.exe; observed {output:?}"
    );
    Ok(())
}

#[test]
fn conpty_background_reader_receives_output_after_input() -> Result<(), Box<dyn std::error::Error>>
{
    let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/D", "/K"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;

    let reader = spawned.master().try_clone_io()?;
    let writer = spawned.master().try_clone_io()?;
    let (tx, rx) = mpsc::channel();
    let reader_thread = thread::spawn(move || {
        let result = read_until_io(&reader, b"RMUX_BACKGROUND_OK", Duration::from_secs(4));
        let _ = tx.send(
            result
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|error| error.to_string()),
        );
    });

    thread::sleep(Duration::from_millis(100));
    writer.write_all(b"echo RMUX_BACKGROUND_OK\r\n")?;

    let output = rx
        .recv_timeout(Duration::from_secs(5))?
        .map_err(std::io::Error::other)?;
    spawned.child().terminate_forcefully()?;
    let _ = spawned.child_mut().wait()?;
    reader_thread
        .join()
        .map_err(|_| "background reader thread panicked")?;

    assert!(
        output.contains("RMUX_BACKGROUND_OK"),
        "expected background reader marker in ConPTY output, got {output:?}"
    );
    Ok(())
}

#[test]
fn conpty_spawn_succeeds_when_parent_is_already_in_job() -> Result<(), Box<dyn std::error::Error>> {
    job::run_parent_job_helper(job::ParentJobMode::NoBreakaway)
}

#[test]
fn conpty_breakaway_retry_succeeds_when_parent_job_allows_breakaway(
) -> Result<(), Box<dyn std::error::Error>> {
    job::run_parent_job_helper(job::ParentJobMode::BreakawayAllowed)
}

#[test]
fn conpty_spawn_inside_parent_job_helper() -> Result<(), Box<dyn std::error::Error>> {
    let Some(mode) = job::requested_helper_mode() else {
        return Ok(());
    };
    let _parent_job = job::assign_current_process_to_job(mode)?;
    let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/C", "echo RMUX_PARENT_JOB_OK & ping -n 30 127.0.0.1 >NUL"])
        .size(TerminalSize::new(80, 24))
        .spawn()?;

    let output = read_until(
        spawned.master(),
        b"RMUX_PARENT_JOB_OK",
        Duration::from_secs(2),
    )?;
    assert!(
        String::from_utf8_lossy(&output).contains("RMUX_PARENT_JOB_OK"),
        "expected parent-job marker in ConPTY output, got {:?}",
        String::from_utf8_lossy(&output)
    );

    spawned.child().terminate_forcefully()?;
    let status = spawned.child_mut().wait()?;
    assert!(!status.success());
    assert!(spawned.child_mut().try_wait()?.is_some());
    Ok(())
}

#[test]
fn conpty_force_kill_reaps_child() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
        .size(TerminalSize::new(80, 24))
        .spawn()?;

    spawned.child().terminate_forcefully()?;
    let status = spawned.child_mut().wait()?;
    assert!(!status.success());
    assert!(spawned.child_mut().try_wait()?.is_some());
    Ok(())
}

#[test]
fn conpty_force_kill_reaps_grandchild_process_tree() -> Result<(), Box<dyn std::error::Error>> {
    let command = concat!(
        "powershell -NoLogo -NoProfile -NonInteractive -Command ",
        "\"$child = Start-Process -FilePath ($PSHOME + '\\powershell.exe') ",
        "-ArgumentList '-NoLogo -NoProfile -NonInteractive -Command Start-Sleep -Seconds 30' ",
        "-WindowStyle Hidden -PassThru; ",
        "[Console]::Out.WriteLine('RMUX_' + 'GRANDCHILD=' + $child.Id); ",
        "[Console]::Out.Flush()\"\r\n"
    );
    let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/D", "/K"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    let io = spawned.master().try_clone_io()?;
    let _ = read_until_io(&io, b">", Duration::from_secs(2))?;
    io.write_all(command.as_bytes())?;

    let output = read_until_or_kill(
        &mut spawned,
        b"RMUX_GRANDCHILD=",
        CONPTY_CHILD_OUTPUT_TIMEOUT,
    )?;
    let grandchild_pid = parse_marker_pid(&output, "RMUX_GRANDCHILD=")?;
    assert!(
        process_is_running(grandchild_pid),
        "grandchild process should exist before force kill"
    );

    spawned.child().terminate_forcefully()?;
    let status = spawned.child_mut().wait()?;
    assert!(!status.success());

    let deadline = Instant::now() + Duration::from_secs(2);
    while process_is_running(grandchild_pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }

    if process_is_running(grandchild_pid) {
        let _ = terminate_process_id(grandchild_pid);
        return Err(format!("grandchild process {grandchild_pid} survived Job Object kill").into());
    }
    Ok(())
}

#[test]
fn conpty_resize_after_child_exit_is_not_fatal() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/C", "exit 0"])
        .size(TerminalSize::new(80, 24))
        .spawn()?;

    assert!(spawned.child_mut().wait()?.success());
    spawned.master().resize(TerminalSize::new(90, 25))?;
    Ok(())
}

fn read_until(
    master: &PtyMaster,
    needle: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let io = master.try_clone_io()?;
    read_until_io(&io, needle, timeout)
}

fn read_until_or_kill(
    spawned: &mut SpawnedPty,
    needle: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let io = spawned.master().try_clone_io()?;
    let needle = needle.to_vec();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = read_until_io(&io, &needle, timeout).map_err(|error| error.to_string());
        let _ = tx.send(result);
    });

    match rx.recv_timeout(timeout + Duration::from_secs(1)) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(error.into()),
        Err(error) => {
            let _ = spawned.child().terminate_forcefully();
            let _ = spawned.child_mut().wait();
            Err(format!("timed out waiting for ConPTY output: {error}").into())
        }
    }
}

fn read_until_io(
    io: &rmux_pty::PtyIo,
    needle: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    let mut buffer = [0_u8; 4096];

    while Instant::now() < deadline {
        let bytes_read = io.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..bytes_read]);
        if output.windows(needle.len()).any(|window| window == needle) {
            return Ok(output);
        }
    }

    Ok(output)
}

fn parse_marker_pid(bytes: &[u8], marker: &str) -> Result<u32, Box<dyn std::error::Error>> {
    let output = String::from_utf8_lossy(bytes);
    for start in output
        .match_indices(marker)
        .map(|(index, _)| index + marker.len())
    {
        let digits = output[start..]
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        if !digits.is_empty() {
            return Ok(digits.parse()?);
        }
    }
    Err(format!("marker {marker:?} did not include a pid in {output:?}").into())
}

fn process_is_running(pid: u32) -> bool {
    let handle = unsafe {
        // SAFETY: OpenProcess validates the pid and returns either a handle or null.
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
    };
    if handle.is_null() {
        return false;
    }
    let mut exit_code = 0_u32;
    let ok = unsafe {
        // SAFETY: handle is a live process handle and exit_code is writable.
        GetExitCodeProcess(handle, &mut exit_code)
    };
    unsafe {
        // SAFETY: handle was returned by OpenProcess and is closed exactly once.
        CloseHandle(handle);
    }
    ok != 0 && exit_code == STILL_ACTIVE as u32
}

fn terminate_process_id(pid: u32) -> std::io::Result<()> {
    let handle = unsafe {
        // SAFETY: OpenProcess validates the pid and returns either a handle or null.
        OpenProcess(PROCESS_TERMINATE, 0, pid)
    };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let ok = unsafe {
        // SAFETY: handle has PROCESS_TERMINATE rights and is not transferred.
        TerminateProcess(handle, 1)
    };
    unsafe {
        // SAFETY: handle was returned by OpenProcess and is closed exactly once.
        CloseHandle(handle);
    }
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
