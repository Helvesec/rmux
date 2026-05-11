//! Same-user named mutex used by the SDK Windows daemon startup race.
//!
//! The Windows hidden-daemon launch path needs a per-endpoint serialization
//! point that lives across processes. A named mutex with the same self-only
//! ACL the named pipe uses is the documented Win32 primitive for that. This
//! module exposes a safe wrapper so the SDK bootstrap layer (which forbids
//! `unsafe`) can consume the primitive without owning the FFI surface itself.
//!
//! The wrapper preserves three guarantees the higher layer relies on:
//!
//! * The mutex's security descriptor matches the running user's SID. A peer
//!   started under a different identity cannot acquire or open the mutex.
//! * `Created` is reported only when the calling process wins creation of the
//!   current kernel object. Win32 destroys a named mutex after the last handle is
//!   closed, so a later non-contending caller may legitimately create a fresh
//!   object and also see `Created`.
//! * Releasing the mutex is tied to the guard's `Drop`, so a failure in the
//!   caller's launch path cannot leak ownership across calls.
//!
//! Thread affinity contract: Win32 mutexes are owned per-thread. The thread
//! that calls [`acquire_named_mutex`] is the only one that can release the
//! mutex via `ReleaseMutex`. If the resulting [`NamedMutexGuard`] is dropped
//! on a different thread, `ReleaseMutex` fails silently with
//! `ERROR_NOT_OWNER` and the kernel mutex stays held until the original
//! owning thread terminates (which marks it abandoned for the next waiter).
//! Callers that need the mutex held across `await` points must arrange for
//! the guard to drop on its acquiring thread; the SDK bootstrap layer does
//! this with a dedicated holder thread.

#![cfg(windows)]

use std::ffi::OsStr;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::ptr::null_mut;
use std::time::Duration;

use rmux_os::identity::{IdentityResolver, UserIdentity};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, HANDLE,
    WAIT_ABANDONED, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::System::Threading::{
    CreateMutexExW, ReleaseMutex, WaitForSingleObject, CREATE_MUTEX_INITIAL_OWNER, MUTEX_ALL_ACCESS,
};

/// Maximum length of a Windows kernel object name.
///
/// Documented in `CreateMutexExW`'s `lpName` parameter description; matches
/// `MAX_PATH` because the kernel uses the same buffer.
pub const MAX_NAMED_MUTEX_LEN: usize = 260;

/// Outcome of a successful named-mutex acquire.
#[derive(Debug)]
pub enum NamedMutexAcquire {
    /// The mutex did not previously exist; this caller created it and won
    /// initial ownership atomically with the create call.
    Created(NamedMutexGuard),
    /// The mutex already existed; this caller waited and acquired ownership.
    Opened(NamedMutexGuard),
    /// The previous owner released the mutex by dying. Ownership transferred
    /// to this caller, which should treat the protected resource as suspect.
    Abandoned(NamedMutexGuard),
}

impl NamedMutexAcquire {
    /// Returns whether this caller created the current kernel mutex object.
    ///
    /// This is not a durable "first process ever" flag: Windows removes named
    /// mutex objects when their last handle closes.
    #[must_use]
    pub const fn is_creator(&self) -> bool {
        matches!(self, Self::Created(_))
    }

    /// Borrows the held guard.
    #[must_use]
    pub fn guard(&self) -> &NamedMutexGuard {
        match self {
            Self::Created(g) | Self::Opened(g) | Self::Abandoned(g) => g,
        }
    }

    /// Consumes the outcome and returns the underlying guard.
    #[must_use]
    pub fn into_guard(self) -> NamedMutexGuard {
        match self {
            Self::Created(g) | Self::Opened(g) | Self::Abandoned(g) => g,
        }
    }
}

/// Owned named-mutex handle. Releases on drop.
///
/// The guard is `Send` so the SDK bootstrap layer can move it into a
/// dedicated holder thread; however, the underlying Win32 mutex remains
/// owned by the *thread* that called [`acquire_named_mutex`]. Dropping the
/// guard from any other thread leaves the kernel mutex held until that
/// thread terminates. See the module-level thread-affinity contract.
#[derive(Debug)]
pub struct NamedMutexGuard {
    handle: HANDLE,
}

// SAFETY: HANDLE is a kernel handle and the Win32 mutex APIs are documented
// as thread-safe. The guard is the sole owner of `handle`. Crossing thread
// boundaries with the guard is allowed at the type level so callers can
// schedule the release on the originally acquiring thread; see the
// thread-affinity contract for the operational consequences if they don't.
unsafe impl Send for NamedMutexGuard {}

impl Drop for NamedMutexGuard {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                // SAFETY: `handle` was returned by a successful CreateMutexExW
                // and remains live until CloseHandle below. ReleaseMutex only
                // succeeds on the original acquiring thread (per the Win32
                // contract); on the wrong thread it returns 0 with
                // `ERROR_NOT_OWNER` which is the documented "no-op release"
                // outcome described in the module docs. Either way we close
                // the local handle so this process never leaks the kernel
                // reference.
                let _ = ReleaseMutex(self.handle);
                CloseHandle(self.handle);
            }
        }
    }
}

/// Errors produced by [`acquire_named_mutex`].
#[derive(Debug)]
pub enum NamedMutexError {
    /// The supplied mutex name was empty, oversized, or contained a NUL.
    InvalidName {
        /// Reason the mutex name was rejected.
        reason: String,
    },
    /// Building the same-user security descriptor failed.
    SecurityDescriptor(io::Error),
    /// `CreateMutexExW` failed.
    Create(io::Error),
    /// `WaitForSingleObject` returned a hard failure.
    Wait(io::Error),
    /// The wait elapsed before ownership was acquired.
    TimedOut,
    /// `CreateMutexExW` succeeded for a different identity but the running
    /// user could not open the existing object.
    AccessDenied(io::Error),
}

impl std::fmt::Display for NamedMutexError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName { reason } => {
                write!(formatter, "named-mutex name rejected: {reason}")
            }
            Self::SecurityDescriptor(error) => {
                write!(formatter, "named-mutex security descriptor failed: {error}")
            }
            Self::Create(error) => write!(formatter, "CreateMutexExW failed: {error}"),
            Self::Wait(error) => write!(formatter, "WaitForSingleObject failed: {error}"),
            Self::TimedOut => write!(formatter, "named-mutex wait timed out"),
            Self::AccessDenied(error) => {
                write!(
                    formatter,
                    "named-mutex access denied for current user: {error}"
                )
            }
        }
    }
}

impl std::error::Error for NamedMutexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SecurityDescriptor(error)
            | Self::Create(error)
            | Self::Wait(error)
            | Self::AccessDenied(error) => Some(error),
            _ => None,
        }
    }
}

/// Acquires `name` as a per-user named mutex, waiting up to `timeout`.
///
/// The mutex is created (or opened) with a discretionary ACL that allows only
/// the running user's SID. A peer running under a different identity will
/// fail with [`NamedMutexError::AccessDenied`] and never observe ownership.
pub fn acquire_named_mutex(
    name: &OsStr,
    timeout: Duration,
) -> Result<NamedMutexAcquire, NamedMutexError> {
    validate_name(name)?;
    let wide_name: Vec<u16> = name.encode_wide().chain(std::iter::once(0)).collect();
    let mut attrs = SameUserAttrs::new().map_err(NamedMutexError::SecurityDescriptor)?;

    let handle = unsafe {
        // SAFETY: `attrs` exposes a live SECURITY_ATTRIBUTES that points at a
        // self-owned security descriptor for the duration of the call;
        // `wide_name` is null-terminated UTF-16.
        CreateMutexExW(
            attrs.as_mut_ptr().cast(),
            wide_name.as_ptr(),
            CREATE_MUTEX_INITIAL_OWNER,
            MUTEX_ALL_ACCESS,
        )
    };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        return Err(
            if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                NamedMutexError::AccessDenied(error)
            } else {
                NamedMutexError::Create(error)
            },
        );
    }

    let already_existed = unsafe {
        // SAFETY: GetLastError is always safe to call from any thread.
        GetLastError()
    } == ERROR_ALREADY_EXISTS;

    drop(attrs);

    if !already_existed {
        // CreateMutexExW with CREATE_MUTEX_INITIAL_OWNER granted ownership at
        // creation, so we are already inside the critical section.
        return Ok(NamedMutexAcquire::Created(NamedMutexGuard { handle }));
    }

    // The mutex pre-existed. CREATE_MUTEX_INITIAL_OWNER only takes effect
    // when CreateMutexExW actually creates the object, so we must wait.
    let timeout_ms = duration_to_millis_clamped(timeout);
    let wait = unsafe {
        // SAFETY: `handle` is a valid mutex handle this thread owns; the call
        // signature matches Win32 semantics.
        WaitForSingleObject(handle, timeout_ms)
    };

    match wait {
        WAIT_OBJECT_0 => Ok(NamedMutexAcquire::Opened(NamedMutexGuard { handle })),
        WAIT_ABANDONED => Ok(NamedMutexAcquire::Abandoned(NamedMutexGuard { handle })),
        WAIT_TIMEOUT => {
            unsafe {
                // SAFETY: We never acquired ownership; only the handle ref needs cleanup.
                CloseHandle(handle);
            }
            Err(NamedMutexError::TimedOut)
        }
        WAIT_FAILED => {
            let error = io::Error::last_os_error();
            unsafe {
                // SAFETY: handle is still valid; we close before propagating the failure.
                CloseHandle(handle);
            }
            Err(NamedMutexError::Wait(error))
        }
        other => {
            unsafe {
                // SAFETY: handle is still valid; we close before propagating the failure.
                CloseHandle(handle);
            }
            Err(NamedMutexError::Wait(io::Error::other(format!(
                "WaitForSingleObject returned unexpected code 0x{other:08x}"
            ))))
        }
    }
}

fn validate_name(name: &OsStr) -> Result<(), NamedMutexError> {
    if name.is_empty() {
        return Err(NamedMutexError::InvalidName {
            reason: "mutex name was empty".into(),
        });
    }
    if name.len() > MAX_NAMED_MUTEX_LEN {
        return Err(NamedMutexError::InvalidName {
            reason: format!(
                "mutex name length {} exceeds {MAX_NAMED_MUTEX_LEN} bytes",
                name.len()
            ),
        });
    }
    if name.encode_wide().any(|unit| unit == 0) {
        return Err(NamedMutexError::InvalidName {
            reason: "mutex name contained a NUL code unit".into(),
        });
    }
    Ok(())
}

fn duration_to_millis_clamped(timeout: Duration) -> u32 {
    let millis = timeout.as_millis();
    // Cap below `INFINITE` (`u32::MAX`) so a caller's overflow cannot block forever.
    if millis >= u128::from(u32::MAX) {
        u32::MAX - 1
    } else {
        u32::try_from(millis).unwrap_or(0)
    }
}

struct SameUserAttrs {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl SameUserAttrs {
    fn new() -> io::Result<Self> {
        let sid = match IdentityResolver::current()? {
            UserIdentity::Sid(sid) => sid,
            UserIdentity::Uid(_) => {
                return Err(io::Error::other(
                    "windows identity resolver returned a unix uid",
                ));
            }
        };
        let sddl = wide_null(&format!("O:{sid}G:{sid}D:P(A;;0x1F0001;;;{sid})"));
        let mut descriptor = null_mut();

        let ok = unsafe {
            // SAFETY: `sddl` is a null-terminated UTF-16 string; `descriptor`
            // is an out pointer freed by Drop with LocalFree.
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION,
                &mut descriptor,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.cast(),
                bInheritHandle: 0,
            },
        })
    }

    fn as_mut_ptr(&mut self) -> *mut core::ffi::c_void {
        (&mut self.attributes as *mut SECURITY_ATTRIBUTES).cast()
    }
}

impl Drop for SameUserAttrs {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            unsafe {
                // SAFETY: `descriptor` was allocated by
                // ConvertStringSecurityDescriptorToSecurityDescriptorW.
                LocalFree(self.descriptor.cast());
            }
        }
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_local_mutex_name(label: &str) -> std::ffi::OsString {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::ffi::OsString::from(format!(
            "Local\\rmux-test-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn empty_name_is_rejected() {
        let error = acquire_named_mutex(std::ffi::OsStr::new(""), Duration::from_millis(0))
            .expect_err("empty name must be rejected");
        assert!(matches!(error, NamedMutexError::InvalidName { .. }));
    }

    #[test]
    fn oversize_name_is_rejected() {
        let oversize = "A".repeat(MAX_NAMED_MUTEX_LEN + 1);
        let error = acquire_named_mutex(std::ffi::OsStr::new(&oversize), Duration::from_millis(0))
            .expect_err("oversize name must be rejected");
        assert!(matches!(error, NamedMutexError::InvalidName { .. }));
    }

    #[test]
    fn first_caller_wins_creation_and_release_allows_later_acquire() {
        // Win32 mutexes are recursive: a single thread that already owns the
        // mutex can re-acquire it without blocking. To exercise the actual
        // cross-thread serialization contract we drive the second caller from
        // a separate OS thread.
        let name = unique_local_mutex_name("creation");
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel::<()>();
        let owner_name = name.clone();
        let owner_thread = std::thread::spawn(move || {
            let owner = acquire_named_mutex(&owner_name, Duration::from_millis(0))
                .expect("first caller acquires");
            assert!(owner.is_creator(), "first caller must report Created");
            acquired_tx.send(()).expect("send acquired");
            release_rx.recv().expect("await release signal");
            drop(owner);
        });

        acquired_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("owner thread acquires before contender starts");

        let busy_error = acquire_named_mutex(&name, Duration::from_millis(50))
            .expect_err("second caller must time out while owner holds");
        assert!(matches!(busy_error, NamedMutexError::TimedOut));

        release_tx.send(()).expect("signal owner to release");
        owner_thread.join().expect("owner thread joins");

        let _later = acquire_named_mutex(&name, Duration::from_millis(500))
            .expect("later caller acquires after release");
    }
}
