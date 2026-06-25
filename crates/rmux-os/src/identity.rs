//! Process identity helpers.

#[cfg(unix)]
use std::ffi::{CStr, CString};
use std::io;

#[cfg(windows)]
use std::ptr::null_mut;
#[cfg(windows)]
use std::sync::OnceLock;

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

/// Unix user details resolved from the platform account database.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnixUser {
    /// Numeric user id.
    pub uid: u32,
    /// Login name.
    pub name: String,
}

/// Resolves process identity details.
#[derive(Debug, Default, Clone, Copy)]
pub struct IdentityResolver;

impl IdentityResolver {
    /// Returns the identity of the current process user.
    pub fn current() -> io::Result<UserIdentity> {
        current_user_identity()
    }

    /// Resolves a Unix user by login name through the platform account database.
    #[cfg(unix)]
    pub fn unix_user_by_name(name: &str) -> io::Result<Option<UnixUser>> {
        unix_user_by_name(name)
    }

    /// Resolves a Unix user by uid through the platform account database.
    #[cfg(unix)]
    pub fn unix_user_by_uid(uid: u32) -> io::Result<Option<UnixUser>> {
        unix_user_by_uid(uid)
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

#[cfg(unix)]
fn unix_user_by_name(name: &str) -> io::Result<Option<UnixUser>> {
    let name = CString::new(name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unix user names cannot contain NUL bytes",
        )
    })?;
    let mut buffer = passwd_lookup_buffer();

    loop {
        let mut passwd = empty_passwd();
        let mut result = std::ptr::null_mut();
        let status = unsafe {
            // SAFETY: `name` is nul-terminated, `passwd` and `result` are valid
            // out-parameters, and `buffer` is writable for its full length.
            libc::getpwnam_r(
                name.as_ptr(),
                &mut passwd,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        match passwd_lookup_result(status, result, &passwd)? {
            PasswdLookup::Found(user) => return Ok(Some(user)),
            PasswdLookup::Missing => return Ok(None),
            PasswdLookup::Retry => grow_passwd_lookup_buffer(&mut buffer)?,
        }
    }
}

#[cfg(unix)]
fn unix_user_by_uid(uid: u32) -> io::Result<Option<UnixUser>> {
    let mut buffer = passwd_lookup_buffer();

    loop {
        let mut passwd = empty_passwd();
        let mut result = std::ptr::null_mut();
        let status = unsafe {
            // SAFETY: `passwd` and `result` are valid out-parameters, and
            // `buffer` is writable for its full length.
            libc::getpwuid_r(
                uid,
                &mut passwd,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        match passwd_lookup_result(status, result, &passwd)? {
            PasswdLookup::Found(user) => return Ok(Some(user)),
            PasswdLookup::Missing => return Ok(None),
            PasswdLookup::Retry => grow_passwd_lookup_buffer(&mut buffer)?,
        }
    }
}

#[cfg(unix)]
enum PasswdLookup {
    Found(UnixUser),
    Missing,
    Retry,
}

#[cfg(unix)]
fn passwd_lookup_result(
    status: libc::c_int,
    result: *mut libc::passwd,
    passwd: &libc::passwd,
) -> io::Result<PasswdLookup> {
    if status == libc::ENOENT && result.is_null() {
        return Ok(PasswdLookup::Missing);
    }
    if status == libc::ERANGE {
        return Ok(PasswdLookup::Retry);
    }
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    if result.is_null() {
        return Ok(PasswdLookup::Missing);
    }
    passwd_to_unix_user(passwd).map(PasswdLookup::Found)
}

#[cfg(unix)]
fn passwd_to_unix_user(passwd: &libc::passwd) -> io::Result<UnixUser> {
    if passwd.pw_name.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unix account record has a null name",
        ));
    }
    let name = unsafe {
        // SAFETY: POSIX passwd records expose `pw_name` as a nul-terminated C
        // string for successful lookups.
        CStr::from_ptr(passwd.pw_name)
    }
    .to_str()
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
    .to_owned();

    Ok(UnixUser {
        uid: passwd.pw_uid,
        name,
    })
}

#[cfg(unix)]
fn empty_passwd() -> libc::passwd {
    unsafe {
        // SAFETY: `libc::passwd` is a plain C record filled by getpw*_r before
        // any field is read.
        std::mem::zeroed()
    }
}

#[cfg(unix)]
fn passwd_lookup_buffer() -> Vec<u8> {
    let default = 16 * 1024;
    let max = 1024 * 1024;
    let size = unsafe {
        // SAFETY: sysconf reads process-global configuration without aliases or
        // writable pointers.
        libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX)
    };
    let size = if size > 0 {
        usize::try_from(size).unwrap_or(default).clamp(default, max)
    } else {
        default
    };
    vec![0; size]
}

#[cfg(unix)]
fn grow_passwd_lookup_buffer(buffer: &mut Vec<u8>) -> io::Result<()> {
    let next = buffer
        .len()
        .checked_mul(2)
        .filter(|size| *size <= 1024 * 1024)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Unix account record exceeded the maximum lookup buffer",
            )
        })?;
    buffer.resize(next, 0);
    Ok(())
}

#[cfg(windows)]
fn current_user_identity() -> io::Result<UserIdentity> {
    static CURRENT_USER_IDENTITY: OnceLock<UserIdentity> = OnceLock::new();

    if let Some(identity) = CURRENT_USER_IDENTITY.get() {
        return Ok(identity.clone());
    }

    let token = current_process_token()?;
    let sid = token_user_sid_string(token.get())?;
    let identity = UserIdentity::Sid(sid.into_boxed_str());
    let _ = CURRENT_USER_IDENTITY.set(identity.clone());
    Ok(identity)
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

    #[test]
    fn current_identity_is_stable() {
        let first = IdentityResolver::current().expect("first current user identity");
        let second = IdentityResolver::current().expect("second current user identity");

        assert_eq!(first, second);
    }

    #[cfg(unix)]
    #[test]
    fn current_unix_user_resolves_by_uid_and_name() {
        let UserIdentity::Uid(uid) = IdentityResolver::current().expect("current identity") else {
            panic!("Unix current identity should be a uid");
        };
        let user = IdentityResolver::unix_user_by_uid(uid)
            .expect("uid lookup")
            .expect("current uid should resolve");

        assert_eq!(user.uid, uid);
        assert!(!user.name.is_empty());
        assert_eq!(
            IdentityResolver::unix_user_by_name(&user.name)
                .expect("name lookup")
                .map(|entry| entry.uid),
            Some(uid)
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_user_name_rejects_nul_bytes() {
        let error = IdentityResolver::unix_user_by_name("bad\0name")
            .expect_err("nul byte should be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    #[test]
    fn missing_unix_user_returns_none() {
        let name = format!("rmux_missing_user_{}", std::process::id());
        let user = IdentityResolver::unix_user_by_name(&name).expect("missing user lookup");
        assert_eq!(user, None);
    }
}
