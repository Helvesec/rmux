//! Linux `close_range` fallback cases for the hidden-daemon descriptor harness.

use std::io;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::process::Command;

use super::super::fd_tests::{configure_for_case, run_case, run_stage, FdHarness};
use super::{close_range_error, inherited_fd_scan_limit, mark_inherited_fds_with};

const CLOSE_RANGE_FALLBACK: FdHarness = FdHarness {
    test: "daemon::linux::fd_tests::close_range_fallback_harness",
    configure: configure_close_range_case,
};

#[test]
fn close_range_fallback_harness() {
    run_stage(&CLOSE_RANGE_FALLBACK);
}

fn configure_close_range_case(command: &mut Command, case: &str, kept: RawFd) {
    if matches!(case, "enosys" | "einval") {
        let scan_limit =
            inherited_fd_scan_limit().map_err(|error| error.raw_os_error().unwrap_or(libc::EIO));
        let unsupported: fn(u32, u32) -> io::Result<bool> = if case == "enosys" {
            |_, _| close_range_error(io::Error::from_raw_os_error(libc::ENOSYS))
        } else {
            |_, _| close_range_error(io::Error::from_raw_os_error(libc::EINVAL))
        };
        // SAFETY: the buffers and fallback bound are prepared before fork;
        // the injected syscall result and child path allocate nothing.
        unsafe {
            command.pre_exec(move || {
                mark_inherited_fds_with(&[kept], scan_limit, unsupported)?;
                rustix::process::setsid().map_err(io::Error::from)?;
                Ok(())
            });
        }
        return;
    }
    configure_for_case(command, case, kept);
}

#[test]
fn enosys_fallback_closes_high_fd_at_exec() {
    run_case(&CLOSE_RANGE_FALLBACK, "enosys");
}

#[test]
fn einval_fallback_closes_high_fd_at_exec() {
    run_case(&CLOSE_RANGE_FALLBACK, "einval");
}

#[test]
fn close_range_neighbors_preserve_error_policy() {
    for errno in [libc::EPERM, libc::EBADF, libc::EINTR] {
        assert_eq!(
            close_range_error(io::Error::from_raw_os_error(errno))
                .unwrap_err()
                .raw_os_error(),
            Some(errno)
        );
    }
    for errno in [libc::ENOSYS, libc::EINVAL] {
        assert!(!close_range_error(io::Error::from_raw_os_error(errno)).unwrap());
    }
}
