//! Hidden-daemon process launch policy.
//!
//! This module is the single OS boundary for launching the detached RMUX
//! daemon. CLI and SDK call sites should use these helpers instead of copying
//! platform flags or Unix session setup locally.

use std::io;
use std::process::{Child, Command};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use std::sync::{Mutex, MutexGuard, OnceLock};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    GetHandleInformation, SetHandleInformation, ERROR_ACCESS_DENIED, ERROR_INVALID_HANDLE,
    ERROR_INVALID_PARAMETER, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
    CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS,
};

/// Configures `command` so the spawned RMUX daemon is not tied to the client
/// process' controlling terminal, console, or job object when the platform
/// supports that separation.
///
/// On Windows, `allow_job_breakaway` controls whether
/// `CREATE_BREAKAWAY_FROM_JOB` is included. On Unix it is ignored because a
/// fresh session is created in the child just before `exec`.
pub fn configure_hidden_daemon_command(command: &mut Command, allow_job_breakaway: bool) {
    configure_hidden_daemon_command_impl(command, allow_job_breakaway);
}

/// Spawns a previously configured hidden-daemon command.
///
/// On Windows, captured stdout/stderr handles owned by the short-lived launcher
/// can otherwise leak into the detached daemon and keep parent-side
/// `wait_with_output` calls open until the daemon exits. This helper is the
/// single place that applies the handle inheritance guard before spawning.
pub fn spawn_hidden_daemon_command(command: &mut Command) -> io::Result<Child> {
    spawn_hidden_daemon_command_impl(command)
}

/// Returns whether a hidden-daemon spawn error should be retried without the
/// Windows job breakaway flag.
#[must_use]
pub fn should_retry_hidden_daemon_without_breakaway(error: &io::Error) -> bool {
    should_retry_hidden_daemon_without_breakaway_impl(error)
}

#[cfg(unix)]
fn configure_hidden_daemon_command_impl(command: &mut Command, _allow_job_breakaway: bool) {
    // SAFETY: The closure runs after fork and before exec in the daemon child.
    // It only marks inherited descriptors close-on-exec and calls `setsid`;
    // both operations stay inside libc/rustix OS boundaries and avoid touching
    // parent-owned Rust state.
    unsafe {
        command.pre_exec(|| {
            mark_inherited_fds_close_on_exec()?;
            rustix::process::setsid().map_err(io::Error::from)?;
            Ok(())
        });
    }
}

#[cfg(target_os = "linux")]
fn mark_inherited_fds_close_on_exec() -> io::Result<()> {
    // SAFETY: `close_range` with `CLOSE_RANGE_CLOEXEC` does not close file
    // descriptors or dereference Rust memory; it only asks the kernel to mark
    // inherited descriptors >= 3 close-on-exec in the child process.
    let result = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3_u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if result == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOSYS | libc::EINVAL) => mark_inherited_fds_close_on_exec_fallback(),
        _ => Err(error),
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn mark_inherited_fds_close_on_exec() -> io::Result<()> {
    mark_inherited_fds_close_on_exec_fallback()
}

#[cfg(unix)]
const FALLBACK_FD_SCAN_LIMIT: libc::c_int = 16_384;

#[cfg(unix)]
fn mark_inherited_fds_close_on_exec_fallback() -> io::Result<()> {
    let max_fd = inherited_fd_scan_limit();

    for fd in 3..max_fd {
        // SAFETY: `fcntl(F_GETFD)` observes descriptor flags for an integer fd.
        // Invalid descriptors are reported as EBADF and handled below.
        let flags = fcntl_getfd_retry(fd)?;
        if flags == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EBADF) {
                continue;
            }
            return Err(error);
        }

        // SAFETY: `fd` was accepted by `F_GETFD`; `F_SETFD` only updates that
        // descriptor's close-on-exec flag and reports races via EBADF.
        let result = fcntl_setfd_retry(fd, flags | libc::FD_CLOEXEC)?;
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EBADF) {
                continue;
            }
            return Err(error);
        }
    }

    Ok(())
}

#[cfg(unix)]
fn inherited_fd_scan_limit() -> libc::c_int {
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: `limit` points to writable storage for `getrlimit`.
    let result = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) };
    if result != 0 {
        return 1024;
    }
    // SAFETY: `getrlimit` succeeded and initialized `limit`.
    let limit = unsafe { limit.assume_init() };
    let soft = limit.rlim_cur.min(libc::c_int::MAX as libc::rlim_t);
    libc::c_int::try_from(soft)
        .unwrap_or(libc::c_int::MAX)
        .clamp(3, FALLBACK_FD_SCAN_LIMIT)
}

#[cfg(unix)]
fn fcntl_getfd_retry(fd: libc::c_int) -> io::Result<libc::c_int> {
    loop {
        // SAFETY: `fcntl(F_GETFD)` observes descriptor flags for an integer fd.
        let result = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if result != -1 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return Ok(result);
        }
    }
}

#[cfg(unix)]
fn fcntl_setfd_retry(fd: libc::c_int, flags: libc::c_int) -> io::Result<libc::c_int> {
    loop {
        // SAFETY: `fcntl(F_SETFD)` mutates only fd flags for the supplied fd.
        let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags) };
        if result != -1 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return Ok(result);
        }
    }
}

#[cfg(windows)]
fn configure_hidden_daemon_command_impl(command: &mut Command, allow_job_breakaway: bool) {
    command.creation_flags(hidden_daemon_creation_flags(allow_job_breakaway));
}

#[cfg(not(any(unix, windows)))]
fn configure_hidden_daemon_command_impl(_command: &mut Command, _allow_job_breakaway: bool) {}

#[cfg(windows)]
fn spawn_hidden_daemon_command_impl(command: &mut Command) -> io::Result<Child> {
    let _guard = StandardHandleInheritanceGuard::new()?;
    command.spawn()
}

#[cfg(not(windows))]
fn spawn_hidden_daemon_command_impl(command: &mut Command) -> io::Result<Child> {
    command.spawn()
}

#[cfg(windows)]
fn should_retry_hidden_daemon_without_breakaway_impl(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code)
            if code == ERROR_ACCESS_DENIED as i32 || code == ERROR_INVALID_PARAMETER as i32
    )
}

#[cfg(not(windows))]
fn should_retry_hidden_daemon_without_breakaway_impl(_error: &io::Error) -> bool {
    false
}

/// Returns the Win32 creation flags used for hidden daemon children.
#[cfg(windows)]
#[must_use]
pub const fn hidden_daemon_creation_flags(allow_job_breakaway: bool) -> u32 {
    let base =
        DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT;
    if allow_job_breakaway {
        base | CREATE_BREAKAWAY_FROM_JOB
    } else {
        base
    }
}

#[cfg(windows)]
struct StandardHandleInheritanceGuard {
    _lock: MutexGuard<'static, ()>,
    handles: Vec<(HANDLE, u32)>,
}

#[cfg(windows)]
impl StandardHandleInheritanceGuard {
    fn new() -> io::Result<Self> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();

        let lock = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("hidden daemon std-handle inheritance mutex must not be poisoned");
        let mut guard = Self {
            _lock: lock,
            handles: Vec::new(),
        };

        for std_handle in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: `std_handle` is one of the three documented standard
            // handle constants, and the returned pseudo-handle is validated
            // before use.
            let handle = unsafe { GetStdHandle(std_handle) };
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                continue;
            }
            let mut flags = 0_u32;
            // SAFETY: `handle` was returned by `GetStdHandle` and filtered for
            // null/INVALID_HANDLE_VALUE above; `flags` is a valid out pointer.
            let ok = unsafe { GetHandleInformation(handle, &mut flags) };
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_INVALID_HANDLE as i32) {
                    continue;
                }
                return Err(error);
            }
            if flags & HANDLE_FLAG_INHERIT != 0 {
                // SAFETY: `handle` is a valid standard handle at this point;
                // only the inherit bit is modified and restored by the guard.
                let ok = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                guard.handles.push((handle, flags));
            }
        }

        Ok(guard)
    }
}

#[cfg(windows)]
impl Drop for StandardHandleInheritanceGuard {
    fn drop(&mut self) {
        for (handle, flags) in self.handles.drain(..) {
            let inherit_flag = flags & HANDLE_FLAG_INHERIT;
            // SAFETY: handles in this list were successfully updated by
            // `new`; restoring the inherit bit is best-effort during drop.
            let _ = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, inherit_flag) };
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn hidden_daemon_flags_detach_console_and_preserve_unicode_env() {
        let flags = hidden_daemon_creation_flags(true);

        assert_ne!(flags & DETACHED_PROCESS, 0);
        assert_ne!(flags & CREATE_NO_WINDOW, 0);
        assert_ne!(flags & CREATE_NEW_PROCESS_GROUP, 0);
        assert_ne!(flags & CREATE_UNICODE_ENVIRONMENT, 0);
        assert_ne!(flags & CREATE_BREAKAWAY_FROM_JOB, 0);

        let fallback_flags = hidden_daemon_creation_flags(false);
        assert_ne!(fallback_flags & DETACHED_PROCESS, 0);
        assert_ne!(fallback_flags & CREATE_NO_WINDOW, 0);
        assert_eq!(fallback_flags & CREATE_BREAKAWAY_FROM_JOB, 0);
    }

    #[test]
    fn hidden_daemon_retry_is_limited_to_breakaway_failures() {
        assert!(should_retry_hidden_daemon_without_breakaway(
            &io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32)
        ));
        assert!(should_retry_hidden_daemon_without_breakaway(
            &io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as i32)
        ));
        assert!(!should_retry_hidden_daemon_without_breakaway(
            &io::Error::from_raw_os_error(2)
        ));
    }
}
