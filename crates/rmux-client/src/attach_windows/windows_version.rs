use std::mem::size_of;
use std::sync::OnceLock;

use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOEXW;

pub(super) const SCOPED_VT_INPUT_MIN_BUILD: u32 = 26_100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct WindowsVersion {
    pub(super) major: u32,
    pub(super) minor: u32,
    pub(super) build: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowsVersionProbe {
    Detected(WindowsVersion),
    Unavailable,
}

static WINDOWS_VERSION: OnceLock<WindowsVersionProbe> = OnceLock::new();

pub(super) fn current_windows_version() -> Option<WindowsVersion> {
    match *WINDOWS_VERSION.get_or_init(probe_windows_version) {
        WindowsVersionProbe::Detected(version) => Some(version),
        WindowsVersionProbe::Unavailable => None,
    }
}

pub(super) const fn supports_scoped_vt_input(version: WindowsVersion) -> bool {
    version.major > 10 || (version.major == 10 && version.build >= SCOPED_VT_INPUT_MIN_BUILD)
}

fn probe_windows_version() -> WindowsVersionProbe {
    let mut info = OSVERSIONINFOEXW {
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOEXW>() as u32,
        ..OSVERSIONINFOEXW::default()
    };
    let status = unsafe {
        // SAFETY: `info` is initialized with the size required by
        // RtlGetVersion and remains writable for the duration of the call.
        RtlGetVersion(&mut info)
    };
    if status < 0 {
        return WindowsVersionProbe::Unavailable;
    }
    WindowsVersionProbe::Detected(WindowsVersion {
        major: info.dwMajorVersion,
        minor: info.dwMinorVersion,
        build: info.dwBuildNumber,
    })
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlGetVersion(version_information: *mut OSVERSIONINFOEXW) -> i32;
}

#[cfg(test)]
mod tests {
    use super::{supports_scoped_vt_input, WindowsVersion, SCOPED_VT_INPUT_MIN_BUILD};

    #[test]
    fn scoped_vt_input_is_limited_to_new_windows_11_builds() {
        assert!(!supports_scoped_vt_input(WindowsVersion {
            major: 10,
            minor: 0,
            build: 19_045,
        }));
        assert!(!supports_scoped_vt_input(WindowsVersion {
            major: 10,
            minor: 0,
            build: SCOPED_VT_INPUT_MIN_BUILD - 1,
        }));
        assert!(supports_scoped_vt_input(WindowsVersion {
            major: 10,
            minor: 0,
            build: SCOPED_VT_INPUT_MIN_BUILD,
        }));
        assert!(supports_scoped_vt_input(WindowsVersion {
            major: 11,
            minor: 0,
            build: 1,
        }));
    }
}
