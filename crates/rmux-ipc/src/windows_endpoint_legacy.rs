//! Compatibility guard for the pre-v6 Windows endpoint namespace.

#![cfg(windows)]

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_SEM_TIMEOUT,
};
use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

use crate::endpoint::{LocalEndpoint, PIPE_PREFIX};
use crate::windows_endpoint_record::LEGACY_COMPONENT_MAX_LEN;
use crate::windows_endpoint_security::wide_path;

const MAX_LEGACY_PIPE_PATH_UNITS: usize = 256;

pub(crate) fn supported_component(sid: &str, integrity: &str, component: &OsStr) -> Option<String> {
    let component = component.to_string_lossy();
    let endpoint = endpoint(sid, integrity, &component);
    if component.len() > LEGACY_COMPONENT_MAX_LEN
        || endpoint.as_pipe_name().encode_wide().count() > MAX_LEGACY_PIPE_PATH_UNITS
    {
        return None;
    }
    Some(component.into_owned())
}

pub(crate) fn endpoint(sid: &str, integrity: &str, component: &str) -> LocalEndpoint {
    LocalEndpoint::from_path(PathBuf::from(format!(
        "{PIPE_PREFIX}rmux-{sid}-il-{integrity}-{component}"
    )))
}

pub(crate) fn namespace_exists(endpoint: &LocalEndpoint) -> io::Result<bool> {
    let wide = wide_path(endpoint.as_path())?;
    let available = unsafe {
        // SAFETY: `wide` is a NUL-terminated path to a local named pipe.
        WaitNamedPipeW(wide.as_ptr(), 0)
    };
    if available != 0 {
        return Ok(true);
    }

    let error = io::Error::last_os_error();
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_FILE_NOT_FOUND) => Ok(false),
        Some(ERROR_ACCESS_DENIED | ERROR_PIPE_BUSY | ERROR_SEM_TIMEOUT) => Ok(true),
        _ => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_endpoint_keeps_the_published_static_shape() {
        let endpoint = endpoint("S-1-5-21", "medium", "named");
        assert_eq!(
            endpoint.as_path(),
            std::path::Path::new(r"\\.\pipe\rmux-S-1-5-21-il-medium-named")
        );
    }
}
