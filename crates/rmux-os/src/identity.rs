//! Process identity helpers.

use std::io;

#[cfg(windows)]
use std::ptr::null_mut;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
#[cfg(windows)]
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
#[cfg(windows)]
use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Platform user identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UserIdentity {
    /// Unix user id.
    Uid(u32),
    /// Windows security identifier string.
    Sid(Box<str>),
}

/// Resolves process identity details.
#[derive(Debug, Default, Clone, Copy)]
pub struct IdentityResolver;

impl IdentityResolver {
    /// Returns the identity of the current process user.
    pub fn current() -> io::Result<UserIdentity> {
        current_user_identity()
    }
}

/// Returns the real user id for the current process.
#[cfg(unix)]
#[must_use]
pub fn real_user_id() -> u32 {
    rustix::process::getuid().as_raw()
}

#[cfg(unix)]
fn current_user_identity() -> io::Result<UserIdentity> {
    Ok(UserIdentity::Uid(real_user_id()))
}

#[cfg(windows)]
fn current_user_identity() -> io::Result<UserIdentity> {
    let token = current_process_token()?;
    let sid = token_user_sid_string(token.get())?;
    Ok(UserIdentity::Sid(sid.into_boxed_str()))
}

#[cfg(windows)]
struct OwnedHandle(HANDLE);

#[cfg(windows)]
impl OwnedHandle {
    fn get(&self) -> HANDLE {
        self.0
    }
}

#[cfg(windows)]
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                // SAFETY: `self.0` is a handle returned by a successful Win32 call.
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
fn current_process_token() -> io::Result<OwnedHandle> {
    let mut token = null_mut();
    let ok = unsafe {
        // SAFETY: The current process pseudo-handle is always valid and `token`
        // is a writable out-parameter for OpenProcessToken.
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle(token))
}

#[cfg(windows)]
fn token_user_sid_string(token: HANDLE) -> io::Result<String> {
    let mut needed = 0;
    unsafe {
        // SAFETY: This first call intentionally passes a null buffer to request
        // the required byte count.
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut buffer = vec![0_u8; usize::try_from(needed).map_err(|_| io::ErrorKind::InvalidData)?];
    let ok = unsafe {
        // SAFETY: `buffer` is writable for `needed` bytes reported above.
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let token_user = unsafe {
        // SAFETY: A successful TokenUser query initializes a TOKEN_USER header
        // at the beginning of the provided buffer.
        &*(buffer.as_ptr().cast::<TOKEN_USER>())
    };
    sid_to_string(token_user.User.Sid)
}

#[cfg(windows)]
fn sid_to_string(sid: *mut core::ffi::c_void) -> io::Result<String> {
    let mut sid_string = null_mut();
    let ok = unsafe {
        // SAFETY: `sid` comes from a TOKEN_USER structure returned by Windows;
        // `sid_string` is an out-parameter freed with LocalFree on success.
        ConvertSidToStringSidW(sid, &mut sid_string)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let value = wide_ptr_to_string(sid_string.cast_const());
    unsafe {
        // SAFETY: `sid_string` was allocated by ConvertSidToStringSidW.
        LocalFree(sid_string.cast());
    }
    value
}

#[cfg(windows)]
fn wide_ptr_to_string(ptr: *const u16) -> io::Result<String> {
    if ptr.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows returned a null SID string",
        ));
    }
    let mut len = 0;
    unsafe {
        // SAFETY: Windows returns a nul-terminated UTF-16 string on success.
        while *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16(std::slice::from_raw_parts(ptr, len)).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid UTF-16 SID string: {error}"),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{IdentityResolver, UserIdentity};

    #[test]
    fn current_identity_is_available() {
        let identity = IdentityResolver::current().expect("current user identity");
        match identity {
            UserIdentity::Uid(uid) => assert!(uid < u32::MAX),
            UserIdentity::Sid(sid) => assert!(sid.starts_with("S-")),
        }
    }
}
