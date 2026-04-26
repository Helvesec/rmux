//! Process inspection helpers.

use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::ffi::CStr;
use std::io;
#[cfg(unix)]
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use rustix::termios::tcgetpgrp;

/// Inspect process metadata for the current platform.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessInspector;

impl ProcessInspector {
    /// Returns the current working directory for `pid`, when available.
    pub fn current_path(&self, pid: u32) -> io::Result<Option<String>> {
        current_path_impl(pid)
    }

    /// Returns the executable command name for `pid`, when available.
    pub fn command_name(&self, pid: u32) -> io::Result<Option<String>> {
        command_name_impl(pid)
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

/// Unix-only process helpers.
#[cfg(unix)]
pub mod unix {
    use super::*;

    /// Returns the foreground process id for a terminal file descriptor.
    #[must_use]
    pub fn foreground_pid(fd: BorrowedFd<'_>) -> Option<u32> {
        let pgrp = tcgetpgrp(fd).ok()?;
        u32::try_from(pgrp.as_raw_nonzero().get()).ok()
    }
}

#[cfg(target_os = "linux")]
fn current_path_impl(pid: u32) -> io::Result<Option<String>> {
    match std::fs::read_link(format!("/proc/{pid}/cwd")) {
        Ok(path) => Ok(Some(path.to_string_lossy().into_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn command_name_impl(pid: u32) -> io::Result<Option<String>> {
    Ok(command_name_from_linux_cmdline(pid).or_else(|| command_name_from_linux_comm(pid)))
}

#[cfg(target_os = "linux")]
fn command_name_from_linux_cmdline(pid: u32) -> Option<String> {
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let first = cmdline
        .split(|byte| *byte == 0)
        .find(|segment| !segment.is_empty())?;
    executable_name(std::str::from_utf8(first).ok()?)
}

#[cfg(target_os = "linux")]
fn command_name_from_linux_comm(pid: u32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    executable_name(comm.trim())
}

#[cfg(target_os = "linux")]
fn fd_path_impl(pid: u32, fd: i32) -> io::Result<Option<PathBuf>> {
    match std::fs::read_link(format!("/proc/{pid}/fd/{fd}")) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn is_live_impl(pid: u32) -> io::Result<Option<bool>> {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return Ok(Some(false));
    };
    let Some((_, tail)) = stat.rsplit_once(") ") else {
        return Ok(None);
    };
    Ok(Some(!matches!(tail.chars().next(), Some('Z' | 'X'))))
}

#[cfg(target_os = "linux")]
fn environment_impl(pid: u32) -> io::Result<Option<HashMap<String, String>>> {
    let environ = match std::fs::read(format!("/proc/{pid}/environ")) {
        Ok(environ) => environ,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(environment_from_nul_entries(&environ))
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
    if usize::try_from(read).map_or(true, |read| read < size) {
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
fn command_name_from_macos_pidpath(pid: u32) -> Option<String> {
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
    executable_name(&string_from_c_chars(buffer.as_ptr())?)
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
    if usize::try_from(read).map_or(true, |read| read < size) {
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

#[cfg(windows)]
fn current_path_impl(_pid: u32) -> io::Result<Option<String>> {
    Ok(None)
}

#[cfg(windows)]
fn command_name_impl(pid: u32) -> io::Result<Option<String>> {
    windows_command_name(pid)
}

#[cfg(windows)]
fn fd_path_impl(_pid: u32, _fd: i32) -> io::Result<Option<PathBuf>> {
    Ok(None)
}

#[cfg(windows)]
fn is_live_impl(pid: u32) -> io::Result<Option<bool>> {
    windows_is_live(pid)
}

#[cfg(windows)]
fn environment_impl(_pid: u32) -> io::Result<Option<HashMap<String, String>>> {
    Ok(None)
}

#[cfg(windows)]
fn windows_is_live(pid: u32) -> io::Result<Option<bool>> {
    use windows_sys::Win32::Foundation::STILL_ACTIVE;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe {
        // SAFETY: OpenProcess validates the pid and returns either a handle or null.
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
    };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(87) => Ok(Some(false)),
            _ => Err(error),
        };
    }
    let _guard = WindowsHandle(handle);

    let mut exit_code = 0;
    let ok = unsafe {
        // SAFETY: `handle` is a live process handle and `exit_code` is writable.
        GetExitCodeProcess(handle, &mut exit_code)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Some(exit_code == STILL_ACTIVE as u32))
}

#[cfg(windows)]
fn windows_command_name(pid: u32) -> io::Result<Option<String>> {
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe {
        // SAFETY: OpenProcess validates the pid and returns either a handle or null.
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
    };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(87) => Ok(None),
            _ => Err(error),
        };
    }
    let _guard = WindowsHandle(handle);

    let mut buffer = vec![0_u16; 32_768];
    let mut len = u32::try_from(buffer.len()).map_err(|_| io::ErrorKind::InvalidData)?;
    let ok = unsafe {
        // SAFETY: `buffer` is writable for `len` UTF-16 code units.
        QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut len)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(usize::try_from(len).map_err(|_| io::ErrorKind::InvalidData)?);
    let path = String::from_utf16(&buffer).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid process image path: {error}"),
        )
    })?;
    Ok(executable_name(&path))
}

#[cfg(windows)]
struct WindowsHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for WindowsHandle {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: `self.0` is a handle returned by a successful Win32 call.
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
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
    offset = skip_nul_padding(buffer, offset);
    environment_from_nul_entries(&buffer[offset..])
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

fn executable_name(path: &str) -> Option<String> {
    let name = Path::new(path).file_name()?.to_string_lossy();
    let trimmed = name.trim_start_matches('-');
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fd_path_rejects_negative_descriptors() {
        assert_eq!(fd_path(std::process::id(), -1), None);
    }

    #[test]
    fn current_process_is_live() {
        assert_eq!(
            ProcessInspector
                .is_live(std::process::id())
                .expect("liveness query"),
            Some(true)
        );
        assert!(is_live(std::process::id()));
    }

    #[cfg(unix)]
    #[test]
    fn current_process_path_is_available() {
        let path = current_path(std::process::id()).expect("current process cwd should be visible");
        assert!(!path.is_empty());
    }

    #[test]
    fn current_process_command_name_is_available() {
        let name =
            command_name(std::process::id()).expect("current process command should be visible");
        assert!(!name.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn current_process_environment_is_available() {
        let environment =
            environment(std::process::id()).expect("current process environment should be visible");
        assert!(!environment.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn windows_reports_unavailable_environment_as_ok_none() {
        assert_eq!(
            ProcessInspector
                .environment(std::process::id())
                .expect("environment query should not fail"),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_reports_unavailable_fd_path_as_ok_none() {
        assert_eq!(
            ProcessInspector
                .fd_path(std::process::id(), 0)
                .expect("fd path query should not fail"),
            None
        );
    }

    #[test]
    fn parses_nul_separated_environment() {
        let environment = environment_from_nul_entries(b"A=1\0B=two\0\0").expect("environment");

        assert_eq!(environment.get("A").map(String::as_str), Some("1"));
        assert_eq!(environment.get("B").map(String::as_str), Some("two"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_macos_procargs_environment() {
        let mut buffer = Vec::new();
        let argc: libc::c_int = 2;
        buffer.extend_from_slice(&argc.to_ne_bytes());
        buffer.extend_from_slice(b"/bin/zsh\0");
        buffer.extend_from_slice(b"\0\0");
        buffer.extend_from_slice(b"zsh\0-l\0");
        buffer.extend_from_slice(b"RMUX_PANE=%1\0LANG=en_US.UTF-8\0\0");

        let environment = environment_from_macos_procargs(&buffer).expect("environment");

        assert_eq!(environment.get("RMUX_PANE").map(String::as_str), Some("%1"));
        assert_eq!(
            environment.get("LANG").map(String::as_str),
            Some("en_US.UTF-8")
        );
    }
}
