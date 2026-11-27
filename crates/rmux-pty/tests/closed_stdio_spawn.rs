#![cfg(unix)]

//! A caller that closed one of its standard descriptors can still launch a
//! PTY child whose three standard streams are the slave.
//!
//! The PTY allocation then reuses the closed number for the master. The
//! child's copy of that number is replaced by its stdio slave before the
//! pre-exec setup runs, so the setup must never close it by number.

use std::io;
use std::os::fd::RawFd;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use rmux_pty::{ChildCommand, Signal};

#[path = "../../../tests/support/rerun_exact.rs"]
mod test_rerun;

const TEST: &str = "spawn_with_closed_standard_descriptor_wires_all_three_to_the_slave";
const MODE: &str = "RMUX_TEST_CLOSED_STDIO";
/// Printed through the child's stdout, which must be the PTY slave.
const VERDICT: &str = "closed-stdio-child-ok";
const CHILD_SCRIPT: &str =
    "[ -t 0 ] && [ -t 1 ] && [ -t 2 ] && printf 'closed-stdio-%s\\n' child-ok";
/// Above every descriptor the driver allocates while its stdio is closed.
const SAVED_FD_FLOOR: RawFd = 64;

/// Each case closes these descriptors before allocating the PTY. Closing all
/// three also leaves the slave on a standard number.
const CASES: [&str; 4] = ["0", "1", "2", "0,1,2"];

/// The child's exit status and PTY output, or why none was measured.
type Outcome = Result<(ExitStatus, String), String>;

fn duplicate_above_floor(fd: RawFd) -> RawFd {
    // SAFETY: F_DUPFD_CLOEXEC creates a new descriptor owned by this driver.
    let saved = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, SAVED_FD_FLOOR) };
    assert!(
        saved >= SAVED_FD_FLOOR,
        "save fd {fd}: {}",
        io::Error::last_os_error()
    );
    saved
}

fn spawn_and_wait() -> Outcome {
    let mut spawned = match ChildCommand::new("/bin/sh")
        .args(["-c", CHILD_SCRIPT])
        .spawn()
    {
        Ok(spawned) => spawned,
        Err(error) => return Err(format!("{error:?}")),
    };
    let io = match spawned.master().try_clone_io() {
        Ok(io) => io,
        Err(error) => return Err(format!("clone master: {error:?}")),
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut output = Vec::new();
    loop {
        let mut buffer = [0; 4096];
        match io.try_read(&mut buffer) {
            Ok(size) => output.extend_from_slice(&buffer[..size]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) || error.raw_os_error() == Some(libc::EIO) => {}
            Err(error) => return Err(format!("read master: {error}")),
        }
        let text = || String::from_utf8_lossy(&output).into_owned();
        match spawned.child_mut().try_wait() {
            Ok(Some(status)) => return Ok((status, text())),
            Ok(None) => {}
            Err(error) => return Err(format!("wait: {error:?}")),
        }
        if Instant::now() >= deadline {
            let _ = spawned.child().kill(Signal::KILL);
            let _ = spawned.child_mut().wait();
            return Err(format!("timed out: {:?}", text()));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn driver(case: &str) {
    let closed: Vec<RawFd> = case.split(',').map(|fd| fd.parse().unwrap()).collect();
    let saved: Vec<(RawFd, RawFd)> = closed
        .iter()
        .map(|&fd| (fd, duplicate_above_floor(fd)))
        .collect();
    // Nothing may print or panic until the descriptors are restored: libtest
    // reports through them.
    for &fd in &closed {
        // SAFETY: this isolated driver process owns its standard descriptors.
        unsafe { libc::close(fd) };
    }
    let outcome = spawn_and_wait();
    for &(fd, copy) in &saved {
        // SAFETY: `copy` is the driver's saved duplicate; dup2 reinstalls it.
        let restored = unsafe { libc::dup2(copy, fd) };
        // SAFETY: the saved duplicate is no longer needed.
        unsafe { libc::close(copy) };
        assert_eq!(restored, fd, "restore fd {fd}");
    }
    match outcome {
        Ok((status, output)) if status.success() && output.contains(VERDICT) => {}
        other => panic!("closed fds {case}: {other:?}"),
    }
}

#[test]
fn spawn_with_closed_standard_descriptor_wires_all_three_to_the_slave() {
    if let Ok(case) = std::env::var(MODE) {
        driver(&case);
        return;
    }
    // Closed descriptors never affect sibling tests: each case runs alone.
    let mut failures = Vec::new();
    for case in CASES {
        let mut command = test_rerun::exact_command(&std::env::current_exe().unwrap(), TEST);
        command.env(MODE, case);
        let output = command.output().unwrap();
        if let Some(failure) = test_rerun::exact_run_failure(TEST, &output) {
            failures.push(format!("case {case}: {failure}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
