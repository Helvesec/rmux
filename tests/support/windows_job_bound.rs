//! D413 startup assertions shared by the fifteen affected attach/send-keys cases.
use std::error::Error;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

pub fn run_rmux_job_bound<const N: usize>(
    binary: &Path,
    label: &str,
    expect_launch: bool,
    args: [&str; N],
) -> Result<(), Box<dyn Error>> {
    let mut command = Command::new(binary);
    command
        .args(["-L", label])
        .args(args)
        .env_remove("RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB");
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = tx.send(command.output());
    });
    let output = rx.recv_timeout(Duration::from_secs(30))??;
    worker.join().expect("capturing launcher output");
    assert!(
        output.status.success(),
        "D392/D413 startup failed: {output:?}"
    );
    let pid_output = Command::new(binary)
        .args(["-L", label, "display-message", "-p", "#{pid}"])
        .output()?;
    assert!(pid_output.status.success(), "{pid_output:?}");
    assert!(
        pid_output.stderr.is_empty(),
        "an existing daemon must not warn again: {pid_output:?}"
    );
    let pid: u32 = String::from_utf8_lossy(&pid_output.stdout).trim().parse()?;
    // SAFETY: query-only handle to the daemon returned by our unique endpoint.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    assert!(!handle.is_null(), "{}", std::io::Error::last_os_error());
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut bound = 0;
    assert_ne!(
        unsafe { IsProcessInJob(handle.as_raw_handle(), std::ptr::null_mut(), &mut bound) },
        0
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    if bound != 0 && expect_launch {
        assert_eq!(
            stderr.trim(),
            "rmux: warning: daemon is job-bound; an enclosing job may terminate it"
        );
        assert_eq!(stderr.lines().count(), 1);
    } else {
        assert!(stderr.is_empty(), "{stderr}");
    }
    // The existing test continues to exercise its original console assertions.
    // Independence beyond the job lifetime is promised only when bound == 0.
    println!(
        "D413 startup job_bound={} expect_launch={expect_launch} capture_complete=true",
        bound != 0
    );
    Ok(())
}
