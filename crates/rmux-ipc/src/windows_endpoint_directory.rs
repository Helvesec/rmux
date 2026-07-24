//! Windows known-folder selection for managed endpoint discovery state.

#![cfg(windows)]

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr::null_mut;

use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_LocalAppDataLow, SHGetKnownFolderPath,
};

pub(crate) fn state_base(integrity: &str) -> io::Result<PathBuf> {
    let folder = match integrity {
        "low" => &FOLDERID_LocalAppDataLow,
        "medium" | "high" | "system" => &FOLDERID_LocalAppData,
        "untrusted" => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "managed endpoint discovery is unavailable at untrusted Windows integrity",
            ));
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported Windows endpoint integrity label",
            ));
        }
    };
    known_folder_path(folder)
}

fn known_folder_path(folder: &windows_sys::core::GUID) -> io::Result<PathBuf> {
    let mut path = null_mut();
    let status = unsafe {
        // SAFETY: `folder` is a known-folder GUID, the current user token is
        // selected with a null handle, and `path` receives COM-owned memory.
        SHGetKnownFolderPath(folder, 0, null_mut(), &mut path)
    };
    if status < 0 {
        return Err(io::Error::other(format!(
            "SHGetKnownFolderPath failed with HRESULT 0x{:08X}",
            status as u32
        )));
    }
    let path = KnownFolderPath::new(path)?;
    let mut length = 0_usize;
    while length < 32_768 {
        let unit = unsafe {
            // SAFETY: SHGetKnownFolderPath returns a NUL-terminated Windows
            // path. The documented maximum is below this defensive bound.
            *path.0.add(length)
        };
        if unit == 0 {
            let value = unsafe {
                // SAFETY: the preceding scan found the terminator, so these
                // `length` UTF-16 units are initialized and remain live.
                std::slice::from_raw_parts(path.0, length)
            };
            let result = PathBuf::from(OsString::from_wide(value));
            return result
                .is_absolute()
                .then_some(result)
                .ok_or_else(|| io::Error::other("Windows known folder is not absolute"));
        }
        length += 1;
    }
    Err(io::Error::other("Windows known folder is not terminated"))
}

struct KnownFolderPath(windows_sys::core::PWSTR);

impl KnownFolderPath {
    fn new(path: windows_sys::core::PWSTR) -> io::Result<Self> {
        (!path.is_null())
            .then_some(Self(path))
            .ok_or_else(|| io::Error::other("SHGetKnownFolderPath returned an empty path"))
    }
}

impl Drop for KnownFolderPath {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: SHGetKnownFolderPath allocated this non-null pointer with
            // the COM task allocator and this guard owns it exactly once.
            CoTaskMemFree(self.0.cast());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn untrusted_integrity_is_not_advertised_as_writable() {
        let error = super::state_base("untrusted")
            .expect_err("untrusted integrity has no writable per-user known folder");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }
}
