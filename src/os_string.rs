//! Cross-platform helpers for top-level argument parsing.

use std::ffi::OsStr;

/// Returns an OS string as bytes for ASCII-only option parsing.
#[must_use]
pub(crate) fn os_str_bytes(value: &OsStr) -> Vec<u8> {
    os_str_bytes_impl(value)
}

#[cfg(unix)]
fn os_str_bytes_impl(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;

    value.as_bytes().to_vec()
}

#[cfg(windows)]
fn os_str_bytes_impl(value: &OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}
