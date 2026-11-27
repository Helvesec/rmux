#![cfg(unix)]

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use rmux_pty::{ChildCommand, Signal};

#[path = "../../../tests/support/rerun_exact.rs"]
mod test_rerun;

const TEST: &str = "post_exec_keeps_only_standard_descriptors";
const MODE: &str = "RMUX_TEST_POST_EXEC_FDS";
/// Preferred sentinel, kept wherever the descriptor table can hold it.
const HIGH_FD: i32 = 16_385;
/// Darwin's default kern.maxfilesperproc (OPEN_MAX): XNU keeps it on a Mac
/// with less than 8 GiB, such as the 7 GB macos-15 arm64 runner.
const BOUNDED_TABLE: libc::rlim_t = 10_240;
const LOWERED_LIMIT: libc::rlim_t = 1_024;
/// The probe's report when only the standard descriptors survived exec.
const CLEAN_VERDICT: &str = "standard_open=true unexpected=[]";

fn descriptor_flags(fd: i32) -> i32 {
    // SAFETY: F_GETFD only queries the supplied descriptor number.
    unsafe { libc::fcntl(fd, libc::F_GETFD) }
}

fn probe() -> ! {
    // No extra descriptor is allowed after exec. In particular the probe
    // opens no directory or report file that would require an exception.
    let unexpected: Vec<_> = (3..=HIGH_FD + 64)
        .filter(|&fd| descriptor_flags(fd) >= 0)
        .collect();
    let standard_open = (0..=2).all(|fd| descriptor_flags(fd) >= 0);
    println!("standard_open={standard_open} unexpected={unexpected:?}");
    std::process::exit(if standard_open && unexpected.is_empty() {
        0
    } else {
        23
    });
}

fn inheritable_duplicate(file: &File, minimum: i32) -> OwnedFd {
    // SAFETY: F_DUPFD creates a new owned descriptor without CLOEXEC.
    let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, minimum) };
    assert!(fd >= minimum, "duplicate: {}", io::Error::last_os_error());
    // SAFETY: the successful F_DUPFD returned a new owned descriptor.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

fn limit() -> libc::rlimit {
    let mut limit = std::mem::MaybeUninit::uninit();
    // SAFETY: getrlimit initializes the provided storage on success.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) },
        0
    );
    // SAFETY: getrlimit succeeded.
    unsafe { limit.assume_init() }
}

fn set_limit(limit: libc::rlimit) {
    // SAFETY: the value is initialized; only this isolated test process changes.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
}

fn wait_with_output(mut child: rmux_pty::SpawnedPty) -> (ExitStatus, String) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut output = Vec::new();
    let io = child.master().try_clone_io().unwrap();
    loop {
        let mut buffer = [0; 4096];
        match io.try_read(&mut buffer) {
            Ok(size) => output.extend_from_slice(&buffer[..size]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {}
            Err(error) => panic!("read probe: {error}"),
        }
        if let Some(status) = child.child_mut().try_wait().unwrap() {
            return (status, String::from_utf8_lossy(&output).into_owned());
        }
        if Instant::now() >= deadline {
            child.child().kill(Signal::KILL).unwrap();
            child.child_mut().wait().unwrap();
            panic!("post-exec probe timed out: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn driver(bounded: bool) {
    let executable = std::env::current_exe().unwrap();
    let mut raised = limit();
    if bounded {
        raised.rlim_max = raised.rlim_max.min(BOUNDED_TABLE);
    }
    raised.rlim_cur = raised
        .rlim_cur
        .max((HIGH_FD + 128) as libc::rlim_t)
        .min(raised.rlim_max);
    set_limit(raised);
    // Darwin accepts a soft limit above kern.maxfilesperproc but caps the
    // usable table there, so the sentinel follows the measured table.
    // SAFETY: getdtablesize only reads this process's descriptor limit.
    let table = unsafe { libc::getdtablesize() };
    let high_fd = HIGH_FD.min(table - 8);
    assert!(
        high_fd > LOWERED_LIMIT as i32,
        "descriptor table of {table} has no fd above the lowered limit"
    );
    let file = File::open("/dev/null").unwrap();
    let low = inheritable_duplicate(&file, 37);
    let high = inheritable_duplicate(&file, high_fd);
    assert_eq!(high.as_raw_fd(), high_fd, "F_DUPFD in a table of {table}");
    assert_eq!(descriptor_flags(low.as_raw_fd()), 0);
    assert_eq!(descriptor_flags(high.as_raw_fd()), 0);

    // A deliberately leaky ordinary exec must be rejected by the same probe.
    let canary = Command::new(&executable)
        .args(["--exact", TEST, "--nocapture"])
        .env(MODE, "probe")
        .output()
        .unwrap();
    assert_eq!(
        canary.status.code(),
        Some(23),
        "canary must detect inherited FDs"
    );
    println!("canary: {}", String::from_utf8_lossy(&canary.stdout));

    // Lowering the soft limit does not close descriptors already above it.
    // Exercise both ordinary spawning and this valid inherited-FD case.
    for lowered_limit in [false, true] {
        if lowered_limit {
            set_limit(libc::rlimit {
                rlim_cur: LOWERED_LIMIT,
                rlim_max: raised.rlim_max,
            });
        }
        let spawned = ChildCommand::new(&executable)
            .args(["--exact", TEST, "--nocapture"])
            .env(MODE, "probe")
            .spawn()
            .unwrap();
        let (status, output) = wait_with_output(spawned);
        // The verdict line proves that the probe itself ran: an exec that
        // matched no test would exit 0 as well.
        assert!(
            status.success() && output.contains(CLEAN_VERDICT),
            "lowered_limit={lowered_limit}: {status}: {output}"
        );
        assert_eq!(descriptor_flags(low.as_raw_fd()), 0, "parent FD changed");
        assert_eq!(
            descriptor_flags(high.as_raw_fd()),
            0,
            "parent sentinel changed"
        );
        // The exec error pipe must remain usable until exec, even though it
        // must not survive a successful exec.
        let error = ChildCommand::new("/rmux-nonexistent-post-exec-program")
            .spawn()
            .expect_err("missing executable must report an exec error");
        assert!(
            matches!(error, rmux_pty::PtyError::Spawn(ref error) if error.kind() == io::ErrorKind::NotFound)
        );
    }
}

#[test]
fn post_exec_keeps_only_standard_descriptors() {
    match std::env::var(MODE).as_deref() {
        Ok("probe") => probe(),
        Ok("driver") => driver(false),
        Ok("bounded-driver") => driver(true),
        _ => {
            // Inheritable sentinels and rlimits never affect sibling tests.
            // The bounded driver gives every host the runner's smaller table.
            for mode in ["driver", "bounded-driver"] {
                test_rerun::rerun_exact(TEST, |command| {
                    command.env(MODE, mode);
                });
            }
        }
    }
}
