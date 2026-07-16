use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rmux_os::process_tree::ProcessTreeChild;
use rustix::process::{waitid, Pid, WaitId, WaitIdOptions};

use super::wait_for_shell_child;

#[test]
fn shell_child_wait_cleans_up_a_stopped_foreground_tree() {
    let pid_file = unique_descendant_pid_file();
    let mut command = Command::new("sh");
    command
        .args([
            "-c",
            "sleep 60 & printf '%s' \"$!\" > \"$RMUX_DESCENDANT_PID\"; wait",
        ])
        .env("RMUX_DESCENDANT_PID", &pid_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = ProcessTreeChild::spawn(&mut command).expect("spawn shell process tree");

    let descendant_pid = wait_for_descendant_pid(&pid_file);
    let leader_pid = child.child_mut().id();
    let mut cleanup = DescendantCleanup {
        pid: Some(descendant_pid),
        pid_file,
    };
    child
        .forward_signal(libc::SIGTSTP)
        .expect("stop shell process tree");
    let stop_deadline = Instant::now() + Duration::from_secs(2);
    while !child_is_stopped(leader_pid).expect("inspect stopped child") {
        assert!(
            Instant::now() < stop_deadline,
            "shell process-group leader did not stop"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let controller = child.controller();
    let started = Instant::now();
    let wait_thread = std::thread::spawn(move || {
        let result = wait_for_shell_child(&mut child, || Some(libc::SIGTERM));
        (result, child)
    });
    let wait_deadline = started + Duration::from_secs(2);
    while !wait_thread.is_finished() && Instant::now() < wait_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let timed_out = !wait_thread.is_finished();
    if timed_out {
        controller
            .terminate()
            .expect("watchdog should terminate a wedged stopped shell tree");
    }
    let (wait_result, mut child) = wait_thread.join().expect("shell wait thread panicked");

    assert!(
        !timed_out,
        "attach did not promptly observe the stopped child"
    );
    wait_result.expect("a stopped foreground command should return control to attach");
    let exit_deadline = Instant::now() + Duration::from_secs(2);
    while process_exists(descendant_pid) && Instant::now() < exit_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !process_exists(descendant_pid),
        "stopped descendant {descendant_pid} survived foreground-command cleanup"
    );
    cleanup.disarm();
    assert!(
        child.has_exited().expect("query reaped child status"),
        "stopped shell process-group leader was not reaped"
    );
    drop(cleanup);
}

fn child_is_stopped(pid: u32) -> std::io::Result<bool> {
    let pid = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| std::io::Error::other("child process id is invalid"))?;
    Ok(waitid(
        WaitId::Pid(pid),
        WaitIdOptions::STOPPED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )?
    .is_some_and(|status| status.stopped()))
}

struct DescendantCleanup {
    pid: Option<i32>,
    pid_file: PathBuf,
}

impl DescendantCleanup {
    fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for DescendantCleanup {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            unsafe {
                // SAFETY: While the test is still failing, it owns the live
                // descendant PID and uses a force signal only for cleanup.
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_file(&self.pid_file);
    }
}

fn unique_descendant_pid_file() -> PathBuf {
    std::env::temp_dir().join(format!(
        "rmux-attach-stopped-descendant-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos()
    ))
}

fn wait_for_descendant_pid(pid_file: &PathBuf) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(contents) = std::fs::read_to_string(pid_file) {
            if let Ok(pid) = contents.parse() {
                return pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "shell did not publish its descendant pid"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn process_exists(pid: i32) -> bool {
    let result = unsafe {
        // SAFETY: Signal zero performs an existence check without changing the
        // target process.
        libc::kill(pid, 0)
    };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}
