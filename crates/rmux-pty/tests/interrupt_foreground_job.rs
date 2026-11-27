#![cfg(unix)]

//! `PtyChild::interrupt` reaches the job a job-control shell put in the
//! foreground of the PTY, as Ctrl-C on that terminal would, and leaves the
//! shell itself alone.
//!
//! The PTY child stands in for such a shell: it moves a forked job into its own
//! process group, hands it the terminal with `tcsetpgrp`, then waits for it
//! while recording any SIGINT it receives itself.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rmux_pty::{ChildCommand, PtyIo};

const TEST: &str = "interrupt_signals_the_terminal_foreground_job_not_the_shell";
const MODE: &str = "RMUX_TEST_INTERRUPT_FOREGROUND_JOB";
const READY: &str = "foreground-job-ready";
const REPORT: &str = "leader-sigint=";

static LEADER_SIGINT: AtomicBool = AtomicBool::new(false);

extern "C" fn note_leader_sigint(_: libc::c_int) {
    LEADER_SIGINT.store(true, Ordering::SeqCst);
}

/// The PTY child: a session leader that runs one foreground job.
fn job_control_leader() {
    // SAFETY: plain libc process-control calls on this re-executed process and
    // its own fork. The forked job only calls async-signal-safe functions.
    unsafe {
        let mut action = std::mem::zeroed::<libc::sigaction>();
        action.sa_sigaction = note_leader_sigint as *const () as usize;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(
            libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut()),
            0
        );

        let job = libc::fork();
        assert!(job >= 0, "fork: {}", io::Error::last_os_error());
        if job == 0 {
            libc::setpgid(0, 0);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            loop {
                libc::pause();
            }
        }
        // Both sides set the group, as a shell does, so neither races the other.
        let _ = libc::setpgid(job, job);
        assert_eq!(libc::tcsetpgrp(0, job), 0, "{}", io::Error::last_os_error());
        println!("{READY}");

        let mut status = 0;
        while libc::waitpid(job, &mut status, 0) == -1 {
            assert_eq!(
                io::Error::last_os_error().kind(),
                io::ErrorKind::Interrupted
            );
        }
        let signal = if libc::WIFSIGNALED(status) {
            libc::WTERMSIG(status)
        } else {
            0
        };
        println!(
            "job-signal={signal} {REPORT}{}",
            LEADER_SIGINT.load(Ordering::SeqCst)
        );
    }
}

/// Reads PTY output into `output` until it contains `marker` or `timeout`.
fn read_until(io: &PtyIo, output: &mut Vec<u8>, marker: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if String::from_utf8_lossy(output).contains(marker) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        let mut buffer = [0; 4096];
        match io.try_read(&mut buffer) {
            Ok(size) => output.extend_from_slice(&buffer[..size]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) || error.raw_os_error() == Some(libc::EIO) =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("read master: {error}"),
        }
    }
}

fn terminal_foreground_group(io: &PtyIo) -> libc::pid_t {
    // SAFETY: tcgetpgrp only reads the terminal state of the open master.
    unsafe { libc::tcgetpgrp(io.as_raw_fd()) }
}

#[test]
fn interrupt_signals_the_terminal_foreground_job_not_the_shell() {
    if std::env::var_os(MODE).is_some() {
        job_control_leader();
        return;
    }
    let mut spawned = ChildCommand::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env(MODE, "1")
        .spawn()
        .unwrap();
    let io = spawned.master().try_clone_io().unwrap();
    let mut output = Vec::new();
    let text = |output: &[u8]| String::from_utf8_lossy(output).into_owned();
    assert!(
        read_until(&io, &mut output, READY, Duration::from_secs(10)),
        "job never took the terminal: {:?}",
        text(&output)
    );
    let leader = libc::pid_t::try_from(spawned.child().pid().as_u32()).unwrap();
    let job_group = terminal_foreground_group(&io);
    assert!(
        job_group > 0 && job_group != leader,
        "foreground {job_group}"
    );

    spawned.child().interrupt().unwrap();
    let reported = read_until(&io, &mut output, REPORT, Duration::from_secs(5));
    if !reported {
        // SAFETY: `job_group` is the group of the job this test's child forked
        // and is still waiting for; nothing else belongs to it.
        unsafe { libc::kill(-job_group, libc::SIGKILL) };
    }
    // Keep reading while the leader exits: Darwin holds an exiting session
    // leader until its terminal output has been drained.
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        let _ = read_until(&io, &mut output, "test result:", Duration::from_millis(20));
        if let Some(status) = spawned.child_mut().try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "leader kept running: {:?}",
            text(&output)
        );
    };
    let output = text(&output);
    assert!(
        reported,
        "the foreground job did not get SIGINT: {output:?}"
    );
    assert!(
        output.contains(&format!("job-signal={} {REPORT}false", libc::SIGINT)),
        "{output:?}"
    );
    assert!(status.success(), "leader {status:?}: {output:?}");
}

/// Without a job-control shell the child itself is the foreground group, and
/// `interrupt` still reaches it.
#[test]
fn interrupt_signals_a_child_that_kept_the_foreground() {
    let mut spawned = ChildCommand::new("/bin/sleep").arg("30").spawn().unwrap();
    let io = spawned.master().try_clone_io().unwrap();
    let leader = libc::pid_t::try_from(spawned.child().pid().as_u32()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while terminal_foreground_group(&io) != leader && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(terminal_foreground_group(&io), leader);

    spawned.child().interrupt().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = spawned.child_mut().try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let Some(status) = status else {
        let _ = spawned.child().terminate_forcefully();
        let _ = spawned.child_mut().wait();
        panic!("sleep survived interrupt");
    };
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?}");
}
