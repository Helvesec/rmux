//! Exercises the host's actual job policy without changing any job limits.
use super::*;
use std::process::Command;
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Threading::{
    OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE, SYNCHRONIZATION_SYNCHRONIZE,
};

const TEST: &str = "daemon::windows_launch::handle_tests::windows_job_launch_inherits_no_handles";
const ROLE: &str = "RMUX_D413_HANDLE_ROLE";

#[test]
fn windows_job_launch_inherits_no_handles() {
    match std::env::var(ROLE).as_deref() {
        Ok("launcher") => {
            let name =
                super::super::wide_event_name(&std::env::var_os("RMUX_D413_SENTINEL").unwrap())
                    .unwrap();
            // SAFETY: open an owned, deliberately inheritable non-stdio event handle.
            let raw = unsafe { OpenEventW(EVENT_MODIFY_STATE, 1, name.as_ptr()) };
            assert!(!raw.is_null());
            let _sentinel = unsafe { OwnedHandle::from_raw_handle(raw) };
            // This is a dedicated single-test subprocess, with no other environment users.
            unsafe {
                std::env::set_var(ROLE, "daemon");
                std::env::set_var("RMUX_D413_HANDLE", (raw as usize).to_string());
            }
            let child = launch_hidden_daemon(
                std::env::current_exe().unwrap().as_os_str(),
                [TEST, "--exact", "--nocapture", "--test-threads=1"],
            )
            .unwrap();
            println!("BOUND={}", child.is_job_bound());
            std::process::exit(0);
        }
        Ok("daemon") => {
            let raw: usize = std::env::var("RMUX_D413_HANDLE").unwrap().parse().unwrap();
            // SAFETY: SetEvent on an invalid handle fails; the parent observes its event.
            unsafe { SetEvent(raw as HANDLE) };
            super::super::signal_startup_ready_event(&std::env::var_os("RMUX_D413_READY").unwrap())
                .unwrap();
            let name = super::super::wide_event_name(&std::env::var_os("RMUX_D413_STOP").unwrap())
                .unwrap();
            let stop = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
            assert!(!stop.is_null());
            let stop = unsafe { OwnedHandle::from_raw_handle(stop) };
            // Bound fixture lifetime even if a parent assertion fails.
            unsafe { WaitForSingleObject(stop.as_raw_handle(), 10_000) };
            std::process::exit(0);
        }
        _ => {}
    }
    let ready = super::super::StartupReadyEvent::new().unwrap();
    let sentinel = super::super::StartupReadyEvent::new().unwrap();
    let stop = super::super::StartupReadyEvent::new().unwrap();
    let started = Instant::now();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(ROLE, "launcher")
        .env("RMUX_D413_SENTINEL", sentinel.name())
        .env("RMUX_D413_READY", ready.name())
        .env("RMUX_D413_STOP", stop.name())
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    let is_ready = ready.wait(Duration::from_secs(2)).unwrap();
    super::super::signal_startup_ready_event(stop.name()).unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "launcher pipes remained inherited: {elapsed:?}"
    );
    assert!(is_ready);
    assert!(
        !sentinel.wait(Duration::ZERO).unwrap(),
        "extra event handle was inherited"
    );
    let bound = ready.is_job_bound().unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("BOUND={bound}")));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), usize::from(bound), "{stderr}");
    if bound {
        assert!(stderr.contains("job-bound"));
    }
}
