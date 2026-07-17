//! Hidden-daemon process launch policy.
//!
//! This module is the single OS boundary for launching the detached RMUX
//! daemon. CLI and SDK call sites should use these helpers instead of copying
//! platform flags or Unix session setup locally.

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

/// Writes a single readiness notification to an inherited Linux eventfd.
#[cfg(target_os = "linux")]
pub fn signal_startup_ready_fd(ready_fd: RawFd) -> io::Result<()> {
    if ready_fd < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "startup readiness fd must be non-negative",
        ));
    }

    let bytes = 1_u64.to_ne_bytes();
    // SAFETY: `ready_fd` is supplied by the parent rmux client as an inherited
    // eventfd. Borrowing it does not transfer ownership, and `write` only
    // touches the kernel object referenced by this descriptor.
    let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(ready_fd) };
    let written = rustix::io::write(fd, &bytes).map_err(io::Error::from)?;
    if written == bytes.len() {
        return Ok(());
    }

    Err(io::Error::new(
        io::ErrorKind::WriteZero,
        "short eventfd readiness write",
    ))
}

/// A named Win32 event used to notify a launcher that a hidden daemon bound its pipe.
#[cfg(windows)]
#[derive(Debug)]
pub struct StartupReadyEvent {
    handle: HANDLE,
    name: OsString,
}

#[cfg(windows)]
impl StartupReadyEvent {
    /// Creates a new unsignaled manual-reset event with a unique local name.
    pub fn new() -> io::Result<Self> {
        for _ in 0..64 {
            let name = unique_startup_ready_event_name();
            let wide = wide_event_name(&name)?;
            // SAFETY: the name is a NUL-terminated UTF-16 buffer and the
            // default security descriptor is suitable for a same-user child
            // process launched immediately after event creation.
            let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }

            // A collision is exceptionally unlikely but easy to handle. The
            // event name is passed to the daemon; if we created a preexisting
            // object an unrelated peer could release our wait too early.
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                // SAFETY: `handle` was returned by `CreateEventW` above and
                // is no longer needed after detecting the name collision.
                let _ = unsafe { CloseHandle(handle) };
                continue;
            }

            return Ok(Self { handle, name });
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique rmux startup readiness event",
        ))
    }

    /// Returns the event name to pass to the hidden daemon.
    #[must_use]
    pub fn name(&self) -> &OsStr {
        self.name.as_os_str()
    }

    /// Waits until the daemon signals readiness, returning `false` on timeout.
    pub fn wait(&self, timeout: Duration) -> io::Result<bool> {
        let timeout_ms = duration_to_wait_millis(timeout);
        // SAFETY: `self.handle` is owned by this object and remains valid for
        // the duration of the wait.
        match unsafe { WaitForSingleObject(self.handle, timeout_ms) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            other => Err(io::Error::other(format!(
                "unexpected startup readiness wait result {other}"
            ))),
        }
    }
}

#[cfg(windows)]
impl Drop for StartupReadyEvent {
    fn drop(&mut self) {
        // SAFETY: `handle` is owned by this object and closed exactly once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// Signals a named Win32 startup readiness event created by the launcher.
#[cfg(windows)]
pub fn signal_startup_ready_event(name: &OsStr) -> io::Result<()> {
    let wide = wide_event_name(name)?;
    // SAFETY: the name is a NUL-terminated UTF-16 buffer. The handle, if
    // returned, is closed before this function exits.
    let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, wide.as_ptr()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: `handle` is an event handle opened with EVENT_MODIFY_STATE.
    let result = unsafe { SetEvent(handle) };
    let error = if result == 0 {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    // SAFETY: `handle` was returned by `OpenEventW` above.
    let _ = unsafe { CloseHandle(handle) };
    match error {
        Some(error) => Err(error),
        None => Ok(()),
    }
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
    // SAFETY: The closure runs after fork and before exec in the daemon child.
    // It only marks inherited descriptors close-on-exec and calls `setsid`;
    // both operations stay inside libc/rustix OS boundaries and avoid touching
    // parent-owned Rust state.
    unsafe {
        command.pre_exec(move || {
            mark_inherited_fds_close_on_exec_except(&preserved_fds)?;
            rustix::process::setsid().map_err(io::Error::from)?;
            Ok(())
        });
    }
}

#[cfg(target_os = "linux")]
fn mark_inherited_fds_close_on_exec_except(preserved_fds: &[RawFd]) -> io::Result<()> {
    if mark_inherited_fd_ranges_close_on_exec(preserved_fds)? {
        return Ok(());
    }

    mark_inherited_fds_close_on_exec_fallback_except(preserved_fds)
}

#[cfg(target_os = "linux")]
fn mark_inherited_fd_ranges_close_on_exec(preserved_fds: &[RawFd]) -> io::Result<bool> {
    let mut start = 3_u32;

    for &fd in preserved_fds {
        if fd < 3 {
            continue;
        }
        let Ok(fd) = u32::try_from(fd) else {
            continue;
        };
        if start < fd && !close_range_cloexec(start, fd - 1)? {
            return Ok(false);
        }
        clear_close_on_exec(fd as libc::c_int)?;
        start = fd.saturating_add(1);
    }

    if !close_range_cloexec(start, u32::MAX)? {
        return Ok(false);
    }
    Ok(true)
}

#[cfg(target_os = "linux")]
fn close_range_cloexec(first: u32, last: u32) -> io::Result<bool> {
    if first > last {
        return Ok(true);
    }

    // SAFETY: `close_range` with `CLOSE_RANGE_CLOEXEC` does not close file
    // descriptors or dereference Rust memory; it only asks the kernel to mark
    // inherited descriptors in the supplied range close-on-exec in the child
    // process.
    let result = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            first,
            last,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if result == 0 {
        return Ok(true);
    }

    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOSYS | libc::EINVAL) => Ok(false),
        _ => Err(error),
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn mark_inherited_fds_close_on_exec_except(preserved_fds: &[RawFd]) -> io::Result<()> {
    mark_inherited_fds_close_on_exec_fallback_except(preserved_fds)
}

#[cfg(unix)]
const FALLBACK_FD_SCAN_LIMIT: libc::c_int = 16_384;

#[cfg(unix)]
fn mark_inherited_fds_close_on_exec_fallback_except(preserved_fds: &[RawFd]) -> io::Result<()> {
    let max_fd = inherited_fd_scan_limit();

    for fd in 3..max_fd {
        if preserved_fds.contains(&fd) {
            clear_close_on_exec(fd)?;
            continue;
        }

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
fn clear_close_on_exec(fd: libc::c_int) -> io::Result<()> {
    let flags = fcntl_getfd_retry(fd)?;
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    let result = fcntl_setfd_retry(fd, flags & !libc::FD_CLOEXEC)?;
    if result == -1 {
        return Err(io::Error::last_os_error());
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
}
