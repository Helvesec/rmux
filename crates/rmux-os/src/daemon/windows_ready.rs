//! Named startup events and the Windows job-bound readiness marker.

use super::*;

/// A named Win32 event used to notify a launcher that a hidden daemon bound its pipe.
#[cfg(windows)]
#[derive(Debug)]
pub struct StartupReadyEvent {
    handle: HANDLE,
    job_bound: HANDLE,
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

            let marker_name = job_bound_event_name(&name);
            let marker_wide = wide_event_name(&marker_name)?;
            // SAFETY: same ownership/security as the readiness event above.
            let job_bound = unsafe { CreateEventW(std::ptr::null(), 1, 0, marker_wide.as_ptr()) };
            if job_bound.is_null() {
                let error = io::Error::last_os_error();
                unsafe { CloseHandle(handle) };
                return Err(error);
            }
            // SAFETY: GetLastError reads thread-local state; both handles are
            // owned here and closed exactly once on this refusal path.
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                unsafe {
                    CloseHandle(job_bound);
                    CloseHandle(handle);
                }
                continue;
            }
            return Ok(Self {
                handle,
                job_bound,
                name,
            });
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

    /// Reads the daemon's `job-bound` readiness marker after a successful wait.
    ///
    /// The daemon sets this companion event before publishing readiness when it
    /// belongs to any job, including an ancestor left after partial breakaway.
    /// An older daemon without this marker leaves it unset.
    pub fn is_job_bound(&self) -> io::Result<bool> {
        // SAFETY: this object owns the live marker event.
        match unsafe { WaitForSingleObject(self.job_bound, 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            other => Err(io::Error::other(format!(
                "unexpected job-bound wait result {other}"
            ))),
        }
    }
}

#[cfg(windows)]
impl Drop for StartupReadyEvent {
    fn drop(&mut self) {
        // SAFETY: `handle` is owned by this object and closed exactly once.
        let _ = unsafe { CloseHandle(self.handle) };
        let _ = unsafe { CloseHandle(self.job_bound) };
    }
}

/// Signals a named Win32 startup readiness event created by the launcher.
#[cfg(windows)]
pub fn signal_startup_ready_event(name: &OsStr) -> io::Result<()> {
    publish_job_bound_marker(name)?;
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

fn job_bound_event_name(name: &OsStr) -> OsString {
    let mut name = name.to_owned();
    name.push("-job-bound");
    name
}

fn publish_job_bound_marker(name: &OsStr) -> io::Result<()> {
    let wide = wide_event_name(&job_bound_event_name(name))?;
    // SAFETY: name is valid UTF-16; no inherited handle is needed for readiness.
    let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, wide.as_ptr()) };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        // A pre-marker launcher supplied only the original readiness event.
        if error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND as i32)
        {
            return Ok(());
        }
        return Err(error);
    }
    // An unavailable query must not advertise an independent daemon.
    let bound = super::windows_launch::current_process_is_job_bound().unwrap_or(true);
    // SAFETY: `handle` is the event opened above and stays open until closed below.
    let result = if bound && unsafe { SetEvent(handle) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    };
    // SAFETY: this function owns the handle returned by OpenEventW.
    unsafe { CloseHandle(handle) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_job_readiness_marker_is_unset_until_publication() {
        let ready = StartupReadyEvent::new().unwrap();
        assert!(!ready.wait(Duration::ZERO).unwrap());
        assert!(!ready.is_job_bound().unwrap());
        signal_startup_ready_event(ready.name()).unwrap();
        assert!(ready.wait(Duration::ZERO).unwrap());
        assert_eq!(
            ready.is_job_bound().unwrap(),
            super::super::windows_launch::current_process_is_job_bound().unwrap()
        );
    }

    #[test]
    fn windows_job_readiness_accepts_legacy_event_without_marker() {
        let name = unique_startup_ready_event_name();
        let wide = wide_event_name(&name).unwrap();
        // SAFETY: create and own just the pre-marker readiness event.
        let raw = unsafe { CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr()) };
        assert!(!raw.is_null());
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        let event = unsafe { OwnedHandle::from_raw_handle(raw) };
        signal_startup_ready_event(&name).unwrap();
        // SAFETY: `event` owns a live event handle for the whole wait.
        assert_eq!(
            unsafe { WaitForSingleObject(event.as_raw_handle(), 0) },
            WAIT_OBJECT_0
        );
    }

    #[test]
    fn windows_job_readiness_errors_do_not_claim_success() {
        assert_eq!(
            signal_startup_ready_event(OsStr::new("bad\0event"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let missing = unique_startup_ready_event_name();
        assert_eq!(
            signal_startup_ready_event(&missing)
                .unwrap_err()
                .raw_os_error(),
            Some(2)
        );
    }
}
