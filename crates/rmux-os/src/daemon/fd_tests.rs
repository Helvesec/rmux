use super::*;
use crate::test_rerun::rerun_exact;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::Stdio;

const STAGE: &str = "RMUX_DAEMON_FD_TEST_STAGE";
const FD_TABLE: &str = "RMUX_DAEMON_FD_TABLE";

/// A re-executed test entry and the configuration its launcher applies.
/// Platform-specific cases live with their platform module and reuse this
/// harness through their own entry.
pub(super) struct FdHarness {
    pub(super) test: &'static str,
    pub(super) configure: fn(&mut Command, &str, RawFd),
}

/// Descriptor numbers placed above the former 16_384 fallback scan cap when
/// the launcher's table reaches them. Darwin accepts a raised RLIMIT_NOFILE
/// but bounds the usable table by kern.maxfilesperproc, which is below 20_001
/// on the macos-15 arm64 runner, so the numbers follow the measured table.
struct FdLayout {
    high: RawFd,
    kept: RawFd,
    late: RawFd,
}

impl FdLayout {
    fn for_table(case: &str, table: RawFd) -> Self {
        let high = 20_000.min(table - 8);
        assert!(high > 1_024, "descriptor table of {table} has no high fd");
        Self {
            high,
            kept: if case == "kept-high" { high + 1 } else { 30 },
            late: 25_000.min(table - 1),
        }
    }
}

const HIDDEN_DAEMON: FdHarness = FdHarness {
    test: "daemon::fd_tests::hidden_daemon_closes_high_inherited_fds",
    configure: configure_for_case,
};

#[test]
fn hidden_daemon_closes_high_inherited_fds() {
    run_stage(&HIDDEN_DAEMON);
}

pub(super) fn run_stage(harness: &FdHarness) {
    match std::env::var(STAGE).as_deref() {
        Ok("daemon") => {
            let case = std::env::var("RMUX_DAEMON_FD_CASE").unwrap_or_default();
            let table = std::env::var(FD_TABLE).unwrap().parse().unwrap();
            let layout = FdLayout::for_table(&case, table);
            for fd in [0, 1, 2, layout.kept] {
                assert!(
                    unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0,
                    "kept fd {fd}"
                );
            }
            for fd in [
                layout.high,
                std::env::var("RMUX_DISCARDED_FD").unwrap().parse().unwrap(),
            ] {
                assert_eq!(
                    unsafe { libc::fcntl(fd, libc::F_GETFD) },
                    -1,
                    "inherited fd {fd} survived exec"
                );
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
            }
            if case == "late" {
                assert_eq!(unsafe { libc::fcntl(layout.late, libc::F_GETFD) }, -1);
            }
            let byte = [1_u8];
            assert_eq!(
                unsafe { libc::write(layout.kept, byte.as_ptr().cast(), 1) },
                1
            );
            let mut release = [0_u8];
            assert_eq!(unsafe { libc::read(0, release.as_mut_ptr().cast(), 1) }, 0);
        }
        Ok("launcher") => launcher(harness),
        _ => rerun_exact(harness.test, |command| {
            command.env(STAGE, "launcher");
        }),
    }
}

fn pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [-1; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

fn launcher(harness: &FdHarness) {
    let case = std::env::var("RMUX_DAEMON_FD_CASE").unwrap_or_default();
    // Limit changes and raw descriptor numbers are confined to this subprocess.
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) },
        0
    );
    let mut limit = unsafe { limit.assume_init() };
    limit.rlim_cur = limit.rlim_max.min(32_768);
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    // The accepted soft limit may exceed the usable table on Darwin.
    let table = unsafe { libc::getdtablesize() };
    let layout = FdLayout::for_table(&case, table);
    let (reader, writer) = pipe();
    let high = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_DUPFD, layout.high) };
    assert_eq!(high, layout.high, "F_DUPFD in a table of {table}");
    let high = unsafe { OwnedFd::from_raw_fd(high) };
    let (ready_reader, ready_writer) = pipe();
    assert_eq!(
        unsafe { libc::dup2(ready_writer.as_raw_fd(), layout.kept) },
        layout.kept
    );
    let kept = unsafe { OwnedFd::from_raw_fd(layout.kept) };
    assert_eq!(
        unsafe { libc::fcntl(layout.kept, libc::F_SETFD, libc::FD_CLOEXEC) },
        0
    );
    let low = writer.as_raw_fd();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", harness.test, "--nocapture"])
        .env(STAGE, "daemon")
        .env("RMUX_DISCARDED_FD", low.to_string())
        .env(FD_TABLE, table.to_string())
        .stdin(Stdio::piped());
    if case == "lowered" {
        limit.rlim_cur = 64;
        limit.rlim_max = 64;
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    }
    (harness.configure)(&mut command, &case, layout.kept);
    // A descriptor opened after configuration must also be covered.
    let late = if case == "late" {
        assert_eq!(
            unsafe { libc::dup2(high.as_raw_fd(), layout.late) },
            layout.late
        );
        Some(unsafe { OwnedFd::from_raw_fd(layout.late) })
    } else {
        None
    };

    let mut child = spawn_hidden_daemon_command(&mut command).unwrap();
    drop((writer, high, late, ready_writer, kept));
    let mut poll = libc::pollfd {
        fd: reader.as_raw_fd(),
        events: libc::POLLIN | libc::POLLHUP,
        revents: 0,
    };
    let eof_ready = unsafe { libc::poll(&mut poll, 1, 2_000) };
    let mut byte = [0_u8];
    let eof = if eof_ready == 1 {
        unsafe { libc::read(reader.as_raw_fd(), byte.as_mut_ptr().cast(), 1) }
    } else {
        -2
    };
    drop(child.stdin.take());
    let status = child.wait().unwrap();
    assert_eq!(
        eof, 0,
        "unintended inherited pipe must reach EOF while daemon is alive"
    );
    assert!(status.success(), "daemon descriptor assertions failed");
    assert_eq!(
        unsafe { libc::read(ready_reader.as_raw_fd(), byte.as_mut_ptr().cast(), 1) },
        1
    );
    assert_eq!(byte, [1]);
}

pub(super) fn configure_for_case(command: &mut Command, case: &str, kept: RawFd) {
    let _ = case;
    configure_hidden_daemon_command_preserving_fds(command, false, &[kept]);
}

pub(super) fn run_case(harness: &FdHarness, case: &str) {
    rerun_exact(harness.test, |command| {
        command
            .env(STAGE, "launcher")
            .env("RMUX_DAEMON_FD_CASE", case);
    });
}

#[test]
fn high_fd_above_lowered_soft_and_hard_limits_is_closed() {
    run_case(&HIDDEN_DAEMON, "lowered");
}

#[test]
fn high_readiness_fd_is_explicitly_preserved() {
    run_case(&HIDDEN_DAEMON, "kept-high");
}

#[test]
fn descriptor_opened_after_configuration_is_closed() {
    run_case(&HIDDEN_DAEMON, "late");
}

/// Runs `case` alone in a re-executed copy of this test binary.
///
/// Darwin's libSystem kills a fork child with SIGKILL ("crashed on child side
/// of fork pre-exec") when another parent thread is inside the first libnotify
/// initialisation at fork time, as `getpw*_r` is in `identity::tests`. A test
/// that forks from the shared harness can therefore lose its child before the
/// `pre_exec` error is reported. The re-exec itself carries no `pre_exec`, so
/// std spawns it without fork handlers.
fn in_single_test_process(test: &str, case: impl FnOnce()) {
    if std::env::var(STAGE).as_deref() == Ok("single-test") {
        case();
        return;
    }
    rerun_exact(test, |command| {
        command.env(STAGE, "single-test");
    });
}

#[test]
fn invalid_preserved_fd_fails_spawn() {
    in_single_test_process("daemon::fd_tests::invalid_preserved_fd_fails_spawn", || {
        let mut command = Command::new("/usr/bin/true");
        configure_hidden_daemon_command_preserving_fds(&mut command, false, &[i32::MAX]);
        assert_eq!(
            command.status().unwrap_err().raw_os_error(),
            Some(libc::EBADF)
        );
    });
}

#[test]
fn fallback_preparation_error_fails_spawn() {
    in_single_test_process(
        "daemon::fd_tests::fallback_preparation_error_fails_spawn",
        || {
            let mut command = Command::new("/usr/bin/true");
            // SAFETY: only a raw OS error is returned in the child, with no allocation.
            unsafe {
                command.pre_exec(|| {
                    mark_inherited_fds_close_on_exec_fallback_except(&[], Err(libc::EACCES))
                });
            }
            assert_eq!(
                command.status().unwrap_err().raw_os_error(),
                Some(libc::EACCES)
            );
        },
    );
}

/// A harness whose test name no longer exists, as after a rename.
const RENAMED: FdHarness = FdHarness {
    test: "daemon::fd_tests::renamed_harness",
    configure: configure_for_case,
};

#[test]
#[should_panic(expected = "did not run exactly one passing test")]
fn stage_rerun_of_a_renamed_test_fails() {
    run_stage(&RENAMED);
}

#[test]
#[should_panic(expected = "did not run exactly one passing test")]
fn case_rerun_of_a_renamed_test_fails() {
    run_case(&RENAMED, "late");
}

#[test]
#[should_panic(expected = "did not run exactly one passing test")]
fn single_test_rerun_of_a_renamed_test_fails() {
    in_single_test_process("daemon::fd_tests::renamed_single_test", || {
        unreachable!("a renamed test never runs")
    });
}
