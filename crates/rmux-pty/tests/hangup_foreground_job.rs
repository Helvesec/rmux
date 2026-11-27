#![cfg(target_os = "linux")]

//! Linux teardown notifies the current foreground job before its shell can
//! reclaim the terminal, and still notifies the launch group. The shell-like
//! fixture catches HUP and waits for the job, so its exit cannot supply the
//! foreground job's signal on behalf of the method under test.

use std::io::{self, Read};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use rmux_pty::{ChildCommand, PtyIo};

const TEST: &str = "hangup_signals_the_terminal_foreground_job_and_the_shell";
const MODE: &str = "RMUX_TEST_HANGUP_FOREGROUND_JOB";
const READY: &str = "foreground-job-ready";
const REPORT: &str = "leader-sighup=";

static LEADER_SIGHUP_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn note_leader_sighup(_: libc::c_int) {
    // SAFETY: the nonblocking writer stays open for this fixture process's
    // lifetime. write is async-signal-safe; preserve the interrupted errno.
    unsafe {
        let errno = *libc::__errno_location();
        libc::write(
            LEADER_SIGHUP_FD.load(Ordering::SeqCst),
            b"H".as_ptr().cast(),
            1,
        );
        *libc::__errno_location() = errno;
    }
}

fn wait_for_job(job: libc::pid_t, options: libc::c_int) -> libc::c_int {
    let mut status = 0;
    // SAFETY: job is this fixture's unreaped child, and status is writable.
    while unsafe { libc::waitpid(job, &mut status, options) } == -1 {
        assert_eq!(
            io::Error::last_os_error().kind(),
            io::ErrorKind::Interrupted
        );
    }
    status
}

/// The PTY child: a session leader that runs one foreground job.
fn job_control_leader() {
    let (mut notification, writer) = UnixStream::pair().unwrap();
    notification
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writer.set_nonblocking(true).unwrap();
    // A process-directed signal may run on libtest's other thread. Keep its
    // writer alive until process exit, including after this test returns.
    LEADER_SIGHUP_FD.store(writer.into_raw_fd(), Ordering::SeqCst);
    // SAFETY: plain libc process-control calls on this re-executed process and
    // its own fork. The forked job only calls async-signal-safe functions.
    unsafe {
        let mut action = std::mem::zeroed::<libc::sigaction>();
        action.sa_sigaction = note_leader_sighup as *const () as usize;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(
            libc::sigaction(libc::SIGHUP, &action, std::ptr::null_mut()),
            0
        );

        let job = libc::fork();
        assert!(job >= 0, "fork: {}", io::Error::last_os_error());
        if job == 0 {
            if libc::setpgid(0, 0) != 0
                || libc::signal(libc::SIGHUP, libc::SIG_DFL) == libc::SIG_ERR
                || libc::raise(libc::SIGSTOP) != 0
            {
                libc::_exit(1);
            }
            loop {
                libc::pause();
            }
        }
        // A group alone does not prove the job has reset its inherited HUP
        // handler. Its stop acknowledges both steps before we advertise READY.
        let status = wait_for_job(job, libc::WUNTRACED);
        assert!(libc::WIFSTOPPED(status));
        assert_eq!(libc::WSTOPSIG(status), libc::SIGSTOP);
        assert_eq!(libc::tcsetpgrp(0, job), 0, "{}", io::Error::last_os_error());
        assert_eq!(libc::kill(job, libc::SIGCONT), 0);
        println!("{READY}");

        // hangup sends two separate signals. The job can exit before the
        // leader's signal is even sent, so wait for that handler's receipt
        // independently instead of sampling a flag immediately after waitpid.
        // Keep the job unreaped until then so timeout cleanup still owns its ID.
        let leader_sighup = notification.read_exact(&mut [0]).is_ok();
        let status = wait_for_job(job, 0);
        let signal = if libc::WIFSIGNALED(status) {
            libc::WTERMSIG(status)
        } else {
            0
        };
        println!("job-signal={signal} {REPORT}{leader_sighup}");
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
fn hangup_signals_the_terminal_foreground_job_and_the_shell() {
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

    spawned.child().hangup().unwrap();
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
        "the foreground job did not get SIGHUP: {output:?}"
    );
    assert!(
        output.contains(&format!("job-signal={} {REPORT}true", libc::SIGHUP)),
        "{output:?}"
    );
    assert!(status.success(), "leader {status:?}: {output:?}");
}

/// Without a job-control shell the child itself is the foreground group, and
/// `hangup` still reaches it.
#[test]
fn hangup_signals_a_child_that_kept_the_foreground() {
    let mut spawned = ChildCommand::new("/bin/sleep").arg("30").spawn().unwrap();
    let io = spawned.master().try_clone_io().unwrap();
    let leader = libc::pid_t::try_from(spawned.child().pid().as_u32()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while terminal_foreground_group(&io) != leader && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(terminal_foreground_group(&io), leader);

    spawned.child().hangup().unwrap();
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
        panic!("sleep survived hangup");
    };
    assert_eq!(status.signal(), Some(libc::SIGHUP), "{status:?}");
}
