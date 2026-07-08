//! Process inspection helpers.

use std::collections::HashMap;
#[cfg(all(unix, target_os = "macos"))]
use std::ffi::CStr;
#[cfg(any(unix, windows))]
use std::ffi::OsString;
use std::io;
#[cfg(unix)]
use std::os::fd::{AsRawFd, BorrowedFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

#[cfg(windows)]
#[path = "process_windows.rs"]
mod windows_process;
#[cfg(target_os = "linux")]
use linux_impl::{
    command_name_impl, current_path_impl, environment_impl, executable_path_impl, fd_path_impl,
    is_live_impl, raw_environment_impl,
};
#[cfg(windows)]
pub use windows_process::ProcessJob;

/// Inspect process metadata for the current platform.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessInspector;

impl ProcessInspector {
    /// Returns the parent process id for `pid`, when available.
    pub fn parent_pid(&self, pid: u32) -> io::Result<Option<u32>> {
        parent_pid_impl(pid)
    }

    /// Returns the current working directory for `pid`, when available.
    pub fn current_path(&self, pid: u32) -> io::Result<Option<String>> {
        current_path_impl(pid)
    }

    /// Returns the executable command name for `pid`, when available.
    pub fn command_name(&self, pid: u32) -> io::Result<Option<String>> {
        command_name_impl(pid)
    }

    /// Returns the executable path for `pid`, when available.
    pub fn executable_path(&self, pid: u32) -> io::Result<Option<String>> {
        executable_path_impl(pid)
    }

    /// Returns the path for a process file descriptor, when available.
    pub fn fd_path(&self, pid: u32, fd: i32) -> io::Result<Option<PathBuf>> {
        if fd < 0 {
            return Ok(None);
        }
        fd_path_impl(pid, fd)
    }

    /// Returns whether `pid` points to a live process, when knowable.
    pub fn is_live(&self, pid: u32) -> io::Result<Option<bool>> {
        is_live_impl(pid)
    }

    /// Returns a process environment snapshot, when available.
    pub fn environment(&self, pid: u32) -> io::Result<Option<HashMap<String, String>>> {
        environment_impl(pid)
    }

    /// Returns a process environment snapshot without UTF-8 conversion, when available.
    #[cfg(any(unix, windows))]
    pub fn raw_environment(&self, pid: u32) -> io::Result<Option<Vec<(OsString, OsString)>>> {
        raw_environment_impl(pid)
    }

    /// Returns executable names for live descendants of `pid`, when available.
    #[cfg(windows)]
    pub fn descendant_command_names(&self, pid: u32) -> io::Result<Vec<String>> {
        descendant_command_names_impl(pid)
    }
}

/// Returns the parent process id for `pid`, when available.
#[must_use]
pub fn parent_pid(pid: u32) -> Option<u32> {
    ProcessInspector.parent_pid(pid).ok().flatten()
}

/// Returns the current working directory for `pid`, when the platform exposes it.
#[must_use]
pub fn current_path(pid: u32) -> Option<String> {
    ProcessInspector.current_path(pid).ok().flatten()
}

/// Returns the executable command name for `pid`, when available.
#[must_use]
pub fn command_name(pid: u32) -> Option<String> {
    ProcessInspector.command_name(pid).ok().flatten()
}

/// Returns the executable path for `pid`, when available.
#[must_use]
pub fn executable_path(pid: u32) -> Option<String> {
    ProcessInspector.executable_path(pid).ok().flatten()
}

/// Returns the path for a process file descriptor, when the platform exposes it.
#[must_use]
pub fn fd_path(pid: u32, fd: i32) -> Option<PathBuf> {
    ProcessInspector.fd_path(pid, fd).ok().flatten()
}

/// Returns whether `pid` points to a process that still looks usable.
#[must_use]
pub fn is_live(pid: u32) -> bool {
    ProcessInspector
        .is_live(pid)
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// Returns a process environment snapshot, when the platform exposes it.
#[must_use]
pub fn environment(pid: u32) -> Option<HashMap<String, String>> {
    ProcessInspector.environment(pid).ok().flatten()
}

/// Returns a process environment snapshot without UTF-8 conversion, when available.
#[cfg(any(unix, windows))]
#[must_use]
pub fn raw_environment(pid: u32) -> Option<Vec<(OsString, OsString)>> {
    ProcessInspector.raw_environment(pid).ok().flatten()
}

/// Returns executable names for live descendants of `pid`, when available.
#[cfg(windows)]
pub fn descendant_command_names(pid: u32) -> Vec<String> {
    ProcessInspector
        .descendant_command_names(pid)
        .unwrap_or_default()
}

/// Unix-only process helpers.
#[cfg(unix)]
pub mod unix {
    use super::*;

    /// Returns the foreground process id for a terminal file descriptor.
    #[must_use]
    pub fn foreground_pid(fd: BorrowedFd<'_>) -> Option<u32> {
        let pgrp = unsafe {
            // SAFETY: `fd` is a borrowed file descriptor supplied by the
            // caller. `tcgetpgrp` does not take ownership of it.
            libc::tcgetpgrp(fd.as_raw_fd())
        };
        if pgrp <= 0 {
            return None;
        }
        u32::try_from(pgrp).ok()
    }
}

#[cfg(not(windows))]
fn parent_pid_impl(_pid: u32) -> io::Result<Option<u32>> {
    Ok(None)
}

#[allow(dead_code)]
fn linux_cwd_path_string(path: PathBuf) -> String {
    const DELETED_SUFFIX: &str = " (deleted)";

    let value = path.to_string_lossy();
    value
        .strip_suffix(DELETED_SUFFIX)
        .unwrap_or(&value)
        .to_owned()
}

#[cfg(target_os = "linux")]
mod linux_impl {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::io;
    use std::path::PathBuf;

    use super::{
        environment_from_nul_entries, executable_name, linux_cwd_path_string,
        raw_environment_from_nul_entries,
    };

    pub(super) fn current_path_impl(pid: u32) -> io::Result<Option<String>> {
        match std::fs::read_link(format!("/proc/{pid}/cwd")) {
            Ok(path) => Ok(Some(linux_cwd_path_string(path))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(super) fn command_name_impl(pid: u32) -> io::Result<Option<String>> {
        Ok(command_name_from_linux_cmdline(pid).or_else(|| command_name_from_linux_comm(pid)))
    }

    pub(super) fn executable_path_impl(pid: u32) -> io::Result<Option<String>> {
        match std::fs::read_link(format!("/proc/{pid}/exe")) {
            Ok(path) => Ok(Some(path.to_string_lossy().into_owned())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn command_name_from_linux_cmdline(pid: u32) -> Option<String> {
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let first = cmdline
            .split(|byte| *byte == 0)
            .find(|segment| !segment.is_empty())?;
        executable_name(std::str::from_utf8(first).ok()?)
    }

    fn command_name_from_linux_comm(pid: u32) -> Option<String> {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        executable_name(comm.trim())
    }

    pub(super) fn fd_path_impl(pid: u32, fd: i32) -> io::Result<Option<PathBuf>> {
        match std::fs::read_link(format!("/proc/{pid}/fd/{fd}")) {
            Ok(path) => Ok(Some(path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(super) fn is_live_impl(pid: u32) -> io::Result<Option<bool>> {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return Ok(Some(false));
        };
        let Some((_, tail)) = stat.rsplit_once(") ") else {
            return Ok(None);
        };
        Ok(Some(!matches!(tail.chars().next(), Some('Z' | 'X'))))
    }

    pub(super) fn environment_impl(pid: u32) -> io::Result<Option<HashMap<String, String>>> {
        let environ = match std::fs::read(format!("/proc/{pid}/environ")) {
            Ok(environ) => environ,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(environment_from_nul_entries(&environ))
    }

    pub(super) fn raw_environment_impl(pid: u32) -> io::Result<Option<Vec<(OsString, OsString)>>> {
        let environ = match std::fs::read(format!("/proc/{pid}/environ")) {
            Ok(environ) => environ,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(Some(raw_environment_from_nul_entries(&environ)))
    }
}

#[cfg(target_os = "macos")]
fn current_path_impl(pid: u32) -> io::Result<Option<String>> {
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>();
    let Some(pid) = pid.try_into().ok() else {
        return Ok(None);
    };
    let Some(size_i32) = size.try_into().ok() else {
        return Ok(None);
    };
    let read = unsafe {
        // SAFETY: `info` points to writable memory sized for the requested flavor.
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size_i32,
        )
    };
    if usize::try_from(read)
        .map(|read| read < size)
        .unwrap_or(true)
    {
        return Ok(None);
    }

    let info = unsafe {
        // SAFETY: `proc_pidinfo` reported that it initialized the full structure.
        info.assume_init()
    };
    Ok(string_from_c_chars(info.pvi_cdir.vip_path.as_ptr().cast()))
}

#[cfg(target_os = "macos")]
fn command_name_impl(pid: u32) -> io::Result<Option<String>> {
    Ok(command_name_from_macos_pidpath(pid).or_else(|| command_name_from_macos_proc_name(pid)))
}

#[cfg(target_os = "macos")]
fn executable_path_impl(pid: u32) -> io::Result<Option<String>> {
    Ok(macos_pidpath(pid))
}

#[cfg(target_os = "macos")]
fn command_name_from_macos_pidpath(pid: u32) -> Option<String> {
    executable_name(&macos_pidpath(pid)?)
}

#[cfg(target_os = "macos")]
fn macos_pidpath(pid: u32) -> Option<String> {
    let mut buffer = [0 as libc::c_char; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let written = unsafe {
        // SAFETY: `buffer` is writable for the size passed to `proc_pidpath`.
        libc::proc_pidpath(
            pid.try_into().ok()?,
            buffer.as_mut_ptr().cast(),
            buffer.len().try_into().ok()?,
        )
    };
    if written <= 0 {
        return None;
    }
    string_from_c_chars(buffer.as_ptr())
}

#[cfg(target_os = "macos")]
fn command_name_from_macos_proc_name(pid: u32) -> Option<String> {
    let mut buffer = [0 as libc::c_char; 1024];
    let written = unsafe {
        // SAFETY: `buffer` is writable for the size passed to `proc_name`.
        libc::proc_name(
            pid.try_into().ok()?,
            buffer.as_mut_ptr().cast(),
            buffer.len().try_into().ok()?,
        )
    };
    if written <= 0 {
        return None;
    }
    string_from_c_chars(buffer.as_ptr()).and_then(|name| executable_name(&name))
}

#[cfg(target_os = "macos")]
fn fd_path_impl(pid: u32, fd: i32) -> io::Result<Option<PathBuf>> {
    let mut info = std::mem::MaybeUninit::<MacosVnodeFdInfoWithPath>::zeroed();
    let size = std::mem::size_of::<MacosVnodeFdInfoWithPath>();
    let Some(pid) = pid.try_into().ok() else {
        return Ok(None);
    };
    let Some(size_i32) = size.try_into().ok() else {
        return Ok(None);
    };
    let read = unsafe {
        // SAFETY: `info` points to writable memory sized for the requested flavor.
        libc::proc_pidfdinfo(
            pid,
            fd,
            MACOS_PROC_PIDFDVNODEPATHINFO,
            info.as_mut_ptr().cast(),
            size_i32,
        )
    };
    if usize::try_from(read)
        .map(|read| read < size)
        .unwrap_or(true)
    {
        return Ok(None);
    }

    let info = unsafe {
        // SAFETY: `proc_pidfdinfo` reported that it initialized the full structure.
        info.assume_init()
    };
    Ok(string_from_c_chars(info.pvip.vip_path.as_ptr().cast()).map(PathBuf::from))
}

#[cfg(target_os = "macos")]
fn is_live_impl(pid: u32) -> io::Result<Option<bool>> {
    let Some(pid) = libc::c_int::try_from(pid).ok() else {
        return Ok(None);
    };
    let Some(size) = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok() else {
        return Ok(None);
    };
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let read = unsafe {
        // SAFETY: `info` points to writable memory sized for the requested flavor.
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read < size {
        return Ok(Some(false));
    }

    let info = unsafe {
        // SAFETY: `proc_pidinfo` reported that it initialized the full structure.
        info.assume_init()
    };
    Ok(Some(info.pbi_status != libc::SZOMB))
}

#[cfg(target_os = "macos")]
fn environment_impl(pid: u32) -> io::Result<Option<HashMap<String, String>>> {
    Ok(macos_procargs(pid).and_then(|buffer| environment_from_macos_procargs(&buffer)))
}

#[cfg(target_os = "macos")]
fn raw_environment_impl(pid: u32) -> io::Result<Option<Vec<(OsString, OsString)>>> {
    Ok(macos_procargs(pid).and_then(|buffer| raw_environment_from_macos_procargs(&buffer)))
}

#[cfg(windows)]
fn parent_pid_impl(pid: u32) -> io::Result<Option<u32>> {
    windows_process::parent_pid(pid)
}

#[cfg(windows)]
fn current_path_impl(pid: u32) -> io::Result<Option<String>> {
    windows_process::current_path(pid)
}

#[cfg(windows)]
fn command_name_impl(pid: u32) -> io::Result<Option<String>> {
    windows_process::command_name(pid)
}

#[cfg(windows)]
fn executable_path_impl(pid: u32) -> io::Result<Option<String>> {
    windows_process::executable_path(pid)
}

#[cfg(windows)]
fn fd_path_impl(pid: u32, fd: i32) -> io::Result<Option<PathBuf>> {
    windows_process::fd_path(pid, fd)
}

#[cfg(windows)]
fn is_live_impl(pid: u32) -> io::Result<Option<bool>> {
    windows_process::is_live(pid)
}

#[cfg(windows)]
fn environment_impl(pid: u32) -> io::Result<Option<HashMap<String, String>>> {
    windows_process::environment(pid)
}

#[cfg(windows)]
fn raw_environment_impl(pid: u32) -> io::Result<Option<Vec<(OsString, OsString)>>> {
    Ok(windows_process::environment(pid)?.map(|environment| {
        environment
            .into_iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }))
}

#[cfg(windows)]
fn descendant_command_names_impl(pid: u32) -> io::Result<Vec<String>> {
    windows_process::descendant_command_names(pid)
}

#[cfg(target_os = "macos")]
const MACOS_PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;

#[cfg(target_os = "macos")]
#[repr(C)]
struct MacosProcFileInfo {
    fi_openflags: u32,
    fi_status: u32,
    fi_offset: libc::off_t,
    fi_type: i32,
    fi_guardflags: u32,
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct MacosVnodeFdInfoWithPath {
    pfi: MacosProcFileInfo,
    pvip: libc::vnode_info_path,
}

#[cfg(target_os = "macos")]
fn string_from_c_chars(chars: *const libc::c_char) -> Option<String> {
    let value = unsafe {
        // SAFETY: macOS libproc path/name buffers are nul-terminated on success.
        CStr::from_ptr(chars)
    }
    .to_string_lossy()
    .into_owned();
    (!value.is_empty()).then_some(value)
}

#[cfg(target_os = "macos")]
fn macos_procargs(pid: u32) -> Option<Vec<u8>> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mib_len = u32::try_from(mib.len()).ok()?;
    let mut size = 0;
    let result = unsafe {
        // SAFETY: The first sysctl call asks only for the required buffer size.
        libc::sysctl(
            mib.as_mut_ptr(),
            mib_len,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size == 0 {
        return None;
    }

    let mut buffer = vec![0; size];
    let result = unsafe {
        // SAFETY: `buffer` is writable for `size` bytes reported by sysctl.
        libc::sysctl(
            mib.as_mut_ptr(),
            mib_len,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size == 0 {
        return None;
    }
    buffer.truncate(size);
    Some(buffer)
}

#[cfg(target_os = "macos")]
fn environment_from_macos_procargs(buffer: &[u8]) -> Option<HashMap<String, String>> {
    let offset = macos_environment_offset(buffer)?;
    environment_from_nul_entries(&buffer[offset..])
}

#[cfg(target_os = "macos")]
fn raw_environment_from_macos_procargs(buffer: &[u8]) -> Option<Vec<(OsString, OsString)>> {
    let offset = macos_environment_offset(buffer)?;
    Some(raw_environment_from_nul_entries(&buffer[offset..]))
}

#[cfg(target_os = "macos")]
fn macos_environment_offset(buffer: &[u8]) -> Option<usize> {
    let argc_size = std::mem::size_of::<libc::c_int>();
    if buffer.len() < argc_size {
        return None;
    }
    let mut argc_bytes = [0; std::mem::size_of::<libc::c_int>()];
    argc_bytes.copy_from_slice(&buffer[..argc_size]);
    let argc = libc::c_int::from_ne_bytes(argc_bytes);
    if argc < 0 {
        return None;
    }

    let mut offset = skip_nul_terminated(buffer, argc_size)?;
    offset = skip_nul_padding(buffer, offset);
    for _ in 0..argc {
        offset = skip_nul_terminated(buffer, offset)?;
    }
    Some(skip_nul_padding(buffer, offset))
}

#[cfg(target_os = "macos")]
fn skip_nul_terminated(buffer: &[u8], offset: usize) -> Option<usize> {
    let relative_end = buffer.get(offset..)?.iter().position(|byte| *byte == 0)?;
    Some(offset + relative_end + 1)
}

#[cfg(target_os = "macos")]
fn skip_nul_padding(buffer: &[u8], mut offset: usize) -> usize {
    while buffer.get(offset).is_some_and(|byte| *byte == 0) {
        offset += 1;
    }
    offset
}

#[cfg(any(unix, test))]
fn environment_from_nul_entries(environ: &[u8]) -> Option<HashMap<String, String>> {
    let mut values = HashMap::new();
    for entry in environ.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let entry = std::str::from_utf8(entry).ok()?;
        let (name, value) = entry.split_once('=')?;
        values.insert(name.to_owned(), value.to_owned());
    }
    Some(values)
}

#[cfg(unix)]
fn raw_environment_from_nul_entries(environ: &[u8]) -> Vec<(OsString, OsString)> {
    let mut values = Vec::new();
    for entry in environ.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let (name, value) = entry.split_at(separator);
        if name.is_empty() || name.first().is_some_and(|byte| *byte == b'=') {
            continue;
        }
        values.push((
            OsString::from_vec(name.to_vec()),
            OsString::from_vec(value[1..].to_vec()),
        ));
    }
    values
}

fn executable_name(path: &str) -> Option<String> {
    let name = Path::new(path).file_name()?.to_string_lossy();
    let trimmed = name.trim_start_matches('-');
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
