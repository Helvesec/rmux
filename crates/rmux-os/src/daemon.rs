//! Hidden-daemon process launch policy.
//!
//! This module is the single OS boundary for launching the detached RMUX
//! daemon. CLI and SDK call sites should use these helpers instead of copying
//! platform flags or Unix session setup locally.

#[cfg(unix)]
mod startup_owner;
#[cfg(unix)]
pub use startup_owner::take_startup_owner;

mod startup_ready;
pub use startup_ready::StartupReadyNotification;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux::{inherited_fd_scan_limit, mark_inherited_fds_close_on_exec_except};
#[cfg(unix)]
mod inherited_fds;
#[cfg(all(unix, not(target_os = "linux")))]
use inherited_fds::inherited_fd_scan_limit;
#[cfg(unix)]
use inherited_fds::mark_inherited_fds_close_on_exec_fallback_except;
#[cfg(target_os = "linux")]
pub use linux::signal_startup_ready_fd;

#[cfg(windows)]
mod windows_launch;
#[cfg(windows)]
pub use windows_launch::{launch_hidden_daemon, WindowsDaemonChild};
#[cfg(windows)]
mod windows_ready;
#[cfg(windows)]
pub use windows_ready::{signal_startup_ready_event, StartupReadyEvent};

use std::io;
use std::process::{Child, Command};

#[cfg(windows)]
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::os::fd::RawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use std::process;
#[cfg(windows)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(windows)]
use std::sync::{Mutex, MutexGuard, OnceLock};
#[cfg(windows)]
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// Hosted Windows test runners own a non-breakaway job for the full test run.
// This test-only override is not compiled into release builds.
#[cfg(all(windows, debug_assertions))]
const INTERNAL_CALLER_JOB_TEST_OPT_IN_ENV: &str = "RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB";

#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, GetHandleInformation, GetLastError, SetHandleInformation, ERROR_ACCESS_DENIED,
    ERROR_ALREADY_EXISTS, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, HANDLE,
    HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
#[cfg(windows)]
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CreateEventW, OpenEventW, SetEvent, WaitForSingleObject, CREATE_BREAKAWAY_FROM_JOB,
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS, EVENT_MODIFY_STATE,
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

/// Configures a hidden daemon command while preserving selected inherited file
/// descriptors across the final daemon `exec`.
#[cfg(unix)]
pub fn configure_hidden_daemon_command_preserving_fds(
    command: &mut Command,
    allow_job_breakaway: bool,
    preserved_fds: &[RawFd],
) {
    configure_hidden_daemon_command_impl_preserving(command, allow_job_breakaway, preserved_fds);
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

/// Spawns a hidden daemon that must escape the launcher's Windows job object.
///
/// Callers must first configure the command with job breakaway enabled. Unlike
/// the legacy fallback policy, this helper performs exactly one spawn attempt.
/// On Windows, breakaway-related failures explain why RMUX refuses to retry
/// inside the caller's job object.
pub fn spawn_hidden_daemon_command_requiring_job_breakaway(
    command: &mut Command,
) -> io::Result<Child> {
    spawn_hidden_daemon_command_requiring_job_breakaway_impl(command)
}

/// Returns whether a hidden-daemon spawn error should be retried without the
/// Windows job breakaway flag.
///
/// This compatibility helper now always returns `false`: an RMUX daemon must
/// outlive its short-lived launcher, so starting it inside the launcher's job
/// object is never a safe fallback.
#[deprecated(note = "hidden RMUX daemons must require Windows job breakaway")]
#[must_use]
pub const fn should_retry_hidden_daemon_without_breakaway(_error: &io::Error) -> bool {
    false
}

#[cfg(unix)]
fn configure_hidden_daemon_command_impl(command: &mut Command, _allow_job_breakaway: bool) {
    configure_hidden_daemon_command_impl_preserving(command, _allow_job_breakaway, &[]);
}

#[cfg(unix)]
fn configure_hidden_daemon_command_impl_preserving(
    command: &mut Command,
    _allow_job_breakaway: bool,
    preserved_fds: &[RawFd],
) {
    let mut preserved_fds = preserved_fds.to_vec();
    preserved_fds.sort_unstable();
    preserved_fds.dedup();
    let scan_limit =
        inherited_fd_scan_limit().map_err(|error| error.raw_os_error().unwrap_or(libc::EIO));
    // SAFETY: The closure runs after fork and before exec in the daemon child.
    // It only marks inherited descriptors close-on-exec and calls `setsid`;
    // both operations stay inside libc/rustix OS boundaries and avoid touching
    // parent-owned Rust state.
    unsafe {
        command.pre_exec(move || {
            mark_inherited_fds_close_on_exec_except(&preserved_fds, scan_limit)?;
            rustix::process::setsid().map_err(io::Error::from)?;
            Ok(())
        });
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn mark_inherited_fds_close_on_exec_except(
    preserved_fds: &[RawFd],
    scan_limit: Result<RawFd, i32>,
) -> io::Result<()> {
    mark_inherited_fds_close_on_exec_fallback_except(preserved_fds, scan_limit)
}

#[cfg(windows)]
fn unique_startup_ready_event_name() -> OsString {
    static NEXT_EVENT_ID: AtomicU64 = AtomicU64::new(0);

    let sequence = NEXT_EVENT_ID.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    OsString::from(format!(
        r"Local\rmux-startup-ready-{}-{sequence}-{timestamp:x}",
        process::id()
    ))
}

#[cfg(windows)]
fn wide_event_name(name: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = name.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "startup readiness event name must not contain NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

#[cfg(windows)]
fn duration_to_wait_millis(timeout: Duration) -> u32 {
    timeout
        .as_millis()
        .min(u128::from(u32::MAX - 1))
        .try_into()
        .unwrap_or(u32::MAX - 1)
}

#[cfg(windows)]
fn configure_hidden_daemon_command_impl(command: &mut Command, allow_job_breakaway: bool) {
    let allow_job_breakaway = allow_job_breakaway && !internal_caller_job_test_opt_in_enabled();
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
fn spawn_hidden_daemon_command_requiring_job_breakaway_impl(
    command: &mut Command,
) -> io::Result<Child> {
    if internal_caller_job_test_opt_in_enabled() {
        return spawn_hidden_daemon_command_impl(command);
    }

    let _guard = StandardHandleInheritanceGuard::new()?;
    command.spawn().map_err(hidden_daemon_breakaway_spawn_error)
}

#[cfg(not(windows))]
fn spawn_hidden_daemon_command_requiring_job_breakaway_impl(
    command: &mut Command,
) -> io::Result<Child> {
    command.spawn()
}

#[cfg(windows)]
fn hidden_daemon_breakaway_spawn_error(error: io::Error) -> io::Error {
    if !matches!(
        error.raw_os_error(),
        Some(code)
            if code == ERROR_ACCESS_DENIED as i32 || code == ERROR_INVALID_PARAMETER as i32
    ) {
        return error;
    }

    io::Error::new(
        error.kind(),
        format!(
            "Windows refused to launch an independent RMUX daemon: {error}. \
             RMUX will not retry inside the caller's job object because that \
             would terminate the daemon with the launcher; allow child-process \
             breakaway in the enclosing job or start RMUX outside that job"
        ),
    )
}

#[cfg(all(windows, debug_assertions))]
fn internal_caller_job_test_opt_in_enabled() -> bool {
    std::env::var_os(INTERNAL_CALLER_JOB_TEST_OPT_IN_ENV).is_some_and(|value| value == "1")
}

#[cfg(all(windows, not(debug_assertions)))]
const fn internal_caller_job_test_opt_in_enabled() -> bool {
    false
}

/// Returns the Win32 creation flags used for hidden daemon children.
#[cfg(windows)]
#[must_use]
pub const fn hidden_daemon_creation_flags(allow_job_breakaway: bool) -> u32 {
    // Keep the daemon detached from the launcher console, but do not create a
    // new process group: ConPTY's Ctrl-C delivery relies on the host and hosted
    // console processes staying in the same process group.
    let base = DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT;
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
    use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;

    #[test]
    fn hidden_daemon_flags_detach_console_and_preserve_unicode_env() {
        let flags = hidden_daemon_creation_flags(true);

        assert_ne!(flags & DETACHED_PROCESS, 0);
        assert_ne!(flags & CREATE_NO_WINDOW, 0);
        assert_eq!(flags & CREATE_NEW_PROCESS_GROUP, 0);
        assert_ne!(flags & CREATE_UNICODE_ENVIRONMENT, 0);
        assert_ne!(flags & CREATE_BREAKAWAY_FROM_JOB, 0);

        let fallback_flags = hidden_daemon_creation_flags(false);
        assert_ne!(fallback_flags & DETACHED_PROCESS, 0);
        assert_ne!(fallback_flags & CREATE_NO_WINDOW, 0);
        assert_eq!(fallback_flags & CREATE_BREAKAWAY_FROM_JOB, 0);
    }

    #[test]
    #[allow(deprecated)]
    fn hidden_daemon_retry_without_breakaway_is_always_refused() {
        assert!(!should_retry_hidden_daemon_without_breakaway(
            &io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32)
        ));
        assert!(!should_retry_hidden_daemon_without_breakaway(
            &io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as i32)
        ));
        assert!(!should_retry_hidden_daemon_without_breakaway(
            &io::Error::from_raw_os_error(2)
        ));
    }

    #[test]
    fn breakaway_spawn_failure_explains_the_safe_recovery() {
        let error = hidden_daemon_breakaway_spawn_error(io::Error::from_raw_os_error(
            ERROR_ACCESS_DENIED as i32,
        ));
        let message = error.to_string();

        assert!(message.contains("Windows refused to launch an independent RMUX daemon"));
        assert!(message.contains("will not retry inside the caller's job object"));
        assert!(message.contains("allow child-process breakaway"));
    }

    #[test]
    fn unrelated_spawn_failure_keeps_its_native_error() {
        let error = hidden_daemon_breakaway_spawn_error(io::Error::from_raw_os_error(2));

        assert_eq!(error.raw_os_error(), Some(2));
    }

    #[test]
    fn startup_ready_event_round_trips_signal_by_name() {
        let ready = StartupReadyEvent::new().expect("create startup readiness event");

        assert!(
            !ready
                .wait(Duration::from_millis(1))
                .expect("initial readiness wait succeeds"),
            "fresh startup readiness event must start unsignaled"
        );
        signal_startup_ready_event(ready.name()).expect("signal readiness event");
        assert!(
            ready
                .wait(Duration::from_millis(100))
                .expect("signaled readiness wait succeeds"),
            "startup readiness event should become signaled"
        );
    }
    #[test]
    fn readiness_notification_uses_the_same_named_event_transport() {
        let ready = StartupReadyEvent::new().unwrap();
        let notification = StartupReadyNotification::from_event_name(ready.name().to_owned());
        let cloned = notification.clone();
        assert!(!ready.wait(Duration::from_millis(1)).unwrap());
        notification.signal().unwrap();
        assert!(ready.wait(Duration::from_millis(100)).unwrap());
        cloned.signal().unwrap();
        let invalid = StartupReadyNotification::from_event_name(OsString::from("bad\0name"));
        assert_eq!(
            invalid.signal().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[cfg(all(test, unix))]
mod fd_tests;
