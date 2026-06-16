//! Process memory pressure helpers.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
const DEFAULT_DAEMON_ARENA_MAX: i32 = 4;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
const DAEMON_ARENA_MAX_ENV: &str = "RMUX_DAEMON_ARENA_MAX";

/// Applies daemon allocator tuning before the long-lived server starts.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn configure_daemon_allocator() {
    let arena_max = std::env::var(DAEMON_ARENA_MAX_ENV)
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_DAEMON_ARENA_MAX);
    // SAFETY: `mallopt` changes process-local glibc allocator policy. Calling
    // it before the daemon runtime starts avoids extra arenas without touching
    // any Rust allocation directly.
    let _ = unsafe { libc::mallopt(libc::M_ARENA_MAX, arena_max) };
}

/// Applies daemon allocator tuning before the long-lived server starts.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn configure_daemon_allocator() {}

/// Returns unused heap pages to the operating system when the platform exposes
/// a safe process-local trim primitive through libc.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn trim_process_heap() {
    // SAFETY: `malloc_trim(0)` is process-local, takes no pointers from Rust,
    // and only asks the active libc allocator to release unused free-list
    // pages. It does not invalidate live allocations.
    let _ = unsafe { libc::malloc_trim(0) };
}

/// Returns unused heap pages to the operating system when supported.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn trim_process_heap() {}
