//! Self-only Windows filesystem objects used by managed endpoint discovery.

#![cfg(windows)]

use std::io;
use std::mem::{size_of, zeroed};
use std::path::Path;
use std::ptr::null_mut;

use rmux_os::identity::{IdentityResolver, UserIdentity};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS, GENERIC_READ, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW, GetSecurityInfo,
    SDDL_REVISION, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    EqualSid, GetAce, GetSecurityDescriptorControl, GetSidSubAuthority, GetSidSubAuthorityCount,
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SE_DACL_PROTECTED,
    SYSTEM_MANDATORY_LABEL_ACE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
};
use windows_sys::Win32::System::SystemServices::{
    ACCESS_ALLOWED_ACE_TYPE, SYSTEM_MANDATORY_LABEL_ACE_TYPE, SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
};

/// Security attributes with an owner-only DACL and exact mandatory label.
pub(crate) struct SameUserSecurityAttributes {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl SameUserSecurityAttributes {
    pub(crate) fn new(integrity: &str) -> io::Result<Self> {
        let sid = current_sid_string()?;
        let mandatory_sid = integrity_sddl_sid(integrity)?;
        let sddl = wide_null(&format!(
            "O:{sid}G:{sid}D:P(A;;GA;;;{sid})S:(ML;;NW;;;{mandatory_sid})"
        ));
        let mut descriptor = null_mut();
        let ok = unsafe {
            // SAFETY: `sddl` is NUL-terminated and descriptor is an out pointer
            // released by Drop.
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

    pub(crate) fn as_ptr(&self) -> *const SECURITY_ATTRIBUTES {
        &self.attributes
    }
}

impl Drop for SameUserSecurityAttributes {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            unsafe {
                // SAFETY: descriptor was allocated by the SDDL conversion API.
                LocalFree(self.descriptor.cast());
            }
        }
    }
}

pub(crate) fn ensure_private_directory(path: &Path, integrity: &str) -> io::Result<()> {
    let wide = wide_path(path)?;
    let security = SameUserSecurityAttributes::new(integrity)?;
    let created = unsafe {
        // SAFETY: path is NUL-terminated and attributes remain live for the call.
        CreateDirectoryW(wide.as_ptr(), security.as_ptr())
    };
    if created == 0 {
        let error = unsafe { GetLastError() };
        if error != ERROR_ALREADY_EXISTS {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
    }

    let handle = unsafe {
        // SAFETY: path is NUL-terminated. OPEN_REPARSE_POINT ensures validation
        // applies to this directory entry rather than a substituted target.
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let result = validate_private_handle(handle, integrity, true);
    unsafe {
        // SAFETY: handle was returned by CreateFileW above.
        CloseHandle(handle);
    }
    result
}

pub(crate) fn validate_private_handle(
    handle: HANDLE,
    integrity: &str,
    expect_directory: bool,
) -> io::Result<()> {
    validate_file_kind(handle, expect_directory)?;

    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut label_acl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    let status = unsafe {
        // SAFETY: handle is live and all output pointers remain backed by
        // descriptor until this function returns.
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            &mut label_acl,
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let _descriptor = LocalSecurityDescriptor(descriptor);

    let current_sid = CurrentSid::new()?;
    if owner.is_null() || unsafe { EqualSid(owner, current_sid.as_ptr()) } == 0 {
        return Err(permission_denied(
            "managed endpoint state owner is not the current user",
        ));
    }
    validate_owner_only_dacl(descriptor, dacl, current_sid.as_ptr())?;
    validate_mandatory_label(label_acl, integrity)?;
    Ok(())
}

fn validate_file_kind(handle: HANDLE, expect_directory: bool) -> io::Result<()> {
    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe {
        // SAFETY: the structure is plain old data and immediately initialized
        // by GetFileInformationByHandle before any field is read.
        zeroed()
    };
    let ok = unsafe { GetFileInformationByHandle(handle, &mut information) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(permission_denied(
            "managed endpoint state cannot be a reparse point",
        ));
    }
    let is_directory = information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    if is_directory != expect_directory {
        return Err(permission_denied(
            "managed endpoint state has an unexpected file type",
        ));
    }
    Ok(())
}

fn validate_owner_only_dacl(
    descriptor: PSECURITY_DESCRIPTOR,
    dacl: *const ACL,
    current_sid: PSID,
) -> io::Result<()> {
    if dacl.is_null() {
        return Err(permission_denied(
            "managed endpoint state has no protected DACL",
        ));
    }
    let mut control = 0_u16;
    let mut revision = 0_u32;
    let ok = unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if control & SE_DACL_PROTECTED == 0 {
        return Err(permission_denied(
            "managed endpoint state DACL inherits permissions",
        ));
    }
    let ace_count = unsafe { (*dacl).AceCount };
    if ace_count != 1 {
        return Err(permission_denied(
            "managed endpoint state DACL is not owner-only",
        ));
    }
    let mut ace = null_mut();
    let ok = unsafe { GetAce(dacl, 0, &mut ace) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let header = unsafe { &*ace.cast::<ACE_HEADER>() };
    if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE
        || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
    {
        return Err(permission_denied(
            "managed endpoint state DACL contains an unexpected ACE",
        ));
    }
    let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
    let sid = std::ptr::addr_of!(allowed.SidStart).cast_mut().cast();
    if allowed.Mask != FILE_ALL_ACCESS || unsafe { EqualSid(sid, current_sid) } == 0 {
        return Err(permission_denied(
            "managed endpoint state DACL is not exact owner-only full access",
        ));
    }
    Ok(())
}

fn validate_mandatory_label(label_acl: *const ACL, integrity: &str) -> io::Result<()> {
    if label_acl.is_null() {
        return Err(permission_denied(
            "managed endpoint state has no mandatory label",
        ));
    }
    let expected = integrity_rid(integrity)?;
    let ace_count = unsafe { (*label_acl).AceCount };
    if ace_count != 1 {
        return Err(permission_denied(
            "managed endpoint state has an ambiguous mandatory label",
        ));
    }
    let mut ace = null_mut();
    let ok = unsafe { GetAce(label_acl, 0, &mut ace) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let header = unsafe { &*ace.cast::<ACE_HEADER>() };
    if u32::from(header.AceType) != SYSTEM_MANDATORY_LABEL_ACE_TYPE
        || usize::from(header.AceSize) < size_of::<SYSTEM_MANDATORY_LABEL_ACE>()
    {
        return Err(permission_denied(
            "managed endpoint state mandatory label is malformed",
        ));
    }
    let mandatory = unsafe { &*ace.cast::<SYSTEM_MANDATORY_LABEL_ACE>() };
    let sid = std::ptr::addr_of!(mandatory.SidStart).cast_mut().cast();
    if mandatory.Mask != SYSTEM_MANDATORY_LABEL_NO_WRITE_UP || sid_integrity_rid(sid)? != expected {
        return Err(permission_denied(
            "managed endpoint state has the wrong mandatory integrity policy",
        ));
    }
    Ok(())
}

fn sid_integrity_rid(sid: PSID) -> io::Result<u32> {
    let count = unsafe { GetSidSubAuthorityCount(sid) };
    if count.is_null() || unsafe { *count } == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows returned an invalid mandatory-label SID",
        ));
    }
    let index = u32::from(unsafe { *count } - 1);
    let rid = unsafe { GetSidSubAuthority(sid, index) };
    if rid.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { *rid })
}

fn current_sid_string() -> io::Result<Box<str>> {
    match IdentityResolver::current()? {
        UserIdentity::Sid(sid) => Ok(sid),
        UserIdentity::Uid(_) => Err(io::Error::other(
            "Windows identity resolver returned a Unix uid",
        )),
    }
}

struct CurrentSid(PSID);

impl CurrentSid {
    fn new() -> io::Result<Self> {
        let wide = wide_null(&current_sid_string()?);
        let mut sid = null_mut();
        let ok = unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(sid))
    }

    fn as_ptr(&self) -> PSID {
        self.0
    }
}

impl Drop for CurrentSid {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                // SAFETY: SID was allocated by ConvertStringSidToSidW.
                LocalFree(self.0.cast());
            }
        }
    }
}

struct LocalSecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                // SAFETY: descriptor was allocated by GetSecurityInfo.
                LocalFree(self.0.cast());
            }
        }
    }
}

fn integrity_sddl_sid(integrity: &str) -> io::Result<&'static str> {
    match integrity {
        "untrusted" => Ok("UN"),
        "low" => Ok("LW"),
        "medium" => Ok("ME"),
        "high" => Ok("HI"),
        "system" => Ok("SI"),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported Windows integrity label {other:?}"),
        )),
    }
}

fn integrity_rid(integrity: &str) -> io::Result<u32> {
    match integrity {
        "untrusted" => Ok(0x0000),
        "low" => Ok(0x1000),
        "medium" => Ok(0x2000),
        "high" => Ok(0x3000),
        "system" => Ok(0x4000),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported Windows integrity label {other:?}"),
        )),
    }
}

pub(crate) fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;

    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if wide.is_empty() || wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed endpoint state path is empty or contains NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn permission_denied(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
#[path = "windows_endpoint_security_tests.rs"]
mod tests;
