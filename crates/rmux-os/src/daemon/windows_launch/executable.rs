//! Keep daemon executable selection consistent with Windows `Command`.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use windows_sys::Win32::System::SystemInformation::{GetSystemDirectoryW, GetWindowsDirectoryW};

pub(super) fn resolve(program: &OsStr) -> io::Result<Vec<u16>> {
    let units: Vec<_> = program.encode_wide().collect();
    if matches!(units.last(), Some(47 | 92)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "daemon path has no file name",
        ));
    }
    if units.iter().any(|unit| matches!(unit, 47 | 92 | 58)) {
        // Explicit paths bypass PATH. Like Command, prefer an appended .exe
        // when no .exe suffix is present, then try the original spelling.
        let path = Path::new(program);
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        {
            let mut with_exe = program.to_owned();
            with_exe.push(".exe");
            if std::fs::symlink_metadata(&with_exe).is_ok() {
                return super::executable_name(&with_exe);
            }
        }
        return super::executable_name(program);
    }
    let mut name = program.to_owned();
    if !units.contains(&u16::from(b'.')) {
        name.push(".exe");
    }
    let mut directories = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            directories.push(parent.to_owned());
        }
    }
    directories.extend(system_directory(GetSystemDirectoryW));
    directories.extend(system_directory(GetWindowsDirectoryW));
    if let Some(paths) = std::env::var_os("PATH") {
        directories
            .extend(std::env::split_paths(&paths).filter(|path| !path.as_os_str().is_empty()));
    }
    for directory in directories {
        let candidate = directory.join(&name);
        if std::fs::symlink_metadata(&candidate).is_ok() {
            return super::executable_name(candidate.as_os_str());
        }
    }
    Err(io::Error::from_raw_os_error(2))
}

fn system_directory(query: unsafe extern "system" fn(*mut u16, u32) -> u32) -> Option<PathBuf> {
    let mut buffer = vec![0_u16; 260];
    loop {
        // SAFETY: buffer is writable for its advertised size. These Win32
        // queries return the required size when it is insufficient.
        let length = unsafe { query(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if length == 0 {
            return None;
        }
        if length < buffer.len() {
            return Some(PathBuf::from(OsString::from_wide(&buffer[..length])));
        }
        buffer.resize(length + 1, 0);
    }
}
