#![cfg(windows)]
//! D413: run unchanged in SSH's nested job and the /IT non-breakaway job.
//! Only an actually detached daemon promises independence from enclosing jobs.
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

fn binary() -> std::path::PathBuf {
    rmux_os::build_identity::cargo_binary(env!("CARGO_BIN_EXE_rmux")).into()
}

#[test]
fn windows_job_bound_cli_capture_declares_actual_lifetime() {
    let label = format!("d413-capture-{}", std::process::id());
    let mut command = Command::new(binary());
    command
        .args([
            "-L",
            &label,
            "-f",
            "NUL",
            "new-session",
            "-d",
            "-s",
            "probe",
            "cmd.exe",
            "/d",
            "/k",
        ])
        .env_remove("RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB");
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    let worker = std::thread::spawn(move || {
        let _ = tx.send(command.output());
    });
    let result = rx.recv_timeout(Duration::from_secs(20));
    if result.is_err() {
        let _ = Command::new(binary())
            .args(["-L", &label, "kill-server"])
            .output();
        panic!("D413 launcher capture did not finish within 20 seconds");
    }
    worker.join().unwrap();
    let output = result.unwrap().unwrap();
    assert!(
        output.status.success(),
        "D413 launch must succeed: {output:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(20));
    let pid_output = Command::new(binary())
        .args(["-L", &label, "display-message", "-p", "#{pid}"])
        .output()
        .unwrap();
    assert!(pid_output.status.success(), "{pid_output:?}");
    let pid: u32 = String::from_utf8_lossy(&pid_output.stdout)
        .trim()
        .parse()
        .unwrap();
    // SAFETY: query-only handle for the daemon whose PID the owned endpoint returned.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    assert!(!raw.is_null());
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut bound = 0;
    let queried =
        unsafe { IsProcessInJob(process.as_raw_handle(), std::ptr::null_mut(), &mut bound) };
    let cleanup = Command::new(binary())
        .args(["-L", &label, "kill-server"])
        .output()
        .unwrap();
    assert!(cleanup.status.success(), "{cleanup:?}");
    assert_ne!(queried, 0);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if bound != 0 {
        assert_eq!(
            stderr.trim(),
            "rmux: warning: daemon is job-bound; an enclosing job may terminate it"
        );
        assert_eq!(stderr.lines().count(), 1);
    } else {
        assert!(
            stderr.is_empty(),
            "detached daemon must not claim job-bound: {stderr}"
        );
    }
    println!(
        "D413 actual_daemon_job_bound={} launcher_elapsed_ms={}",
        bound != 0,
        started.elapsed().as_millis()
    );
}

#[test]
fn windows_job_bound_readiness_declares_actual_membership() {
    use rmux_os::daemon::StartupReadyEvent;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    let ready = StartupReadyEvent::new().unwrap();
    let mut marker = ready.name().to_os_string();
    marker.push("-job-bound");
    let wide: Vec<_> = marker.encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: create/open the readiness companion event, with no inherited handles.
    let raw = unsafe { CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr()) };
    assert!(!raw.is_null());
    let marker = unsafe { OwnedHandle::from_raw_handle(raw) };
    let label = format!("d413-ready-{}", std::process::id());
    let socket = rmux_client::socket_path_for_label(&label).unwrap();
    let mut daemon = Command::new(binary())
        .arg(rmux_client::INTERNAL_DAEMON_FLAG)
        .arg(&socket)
        .arg("--startup-ready-event")
        .arg(ready.name())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let signaled = ready.wait(Duration::from_secs(20)).unwrap();
    let mut bound = 0;
    let queried =
        unsafe { IsProcessInJob(daemon.as_raw_handle(), std::ptr::null_mut(), &mut bound) };
    let observed = unsafe { WaitForSingleObject(marker.as_raw_handle(), 0) };
    let cleanup = Command::new(binary())
        .args(["-L", &label, "kill-server"])
        .output()
        .unwrap();
    assert!(cleanup.status.success(), "{cleanup:?}");
    assert!(daemon.wait().unwrap().success());
    assert!(signaled, "daemon must publish readiness");
    assert_ne!(queried, 0);
    assert_eq!(
        observed,
        if bound != 0 {
            WAIT_OBJECT_0
        } else {
            WAIT_TIMEOUT
        }
    );
    println!("D413 readiness_job_bound={}", bound != 0);
}
