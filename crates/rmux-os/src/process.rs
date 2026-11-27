//! Process inspection helpers.

use std::collections::HashMap;
#[cfg(any(unix, windows))]
use std::ffi::OsString;
use std::io;
#[cfg(unix)]
use std::os::fd::{AsRawFd, BorrowedFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

#[cfg(target_os = "macos")]
mod macos_impl;
#[cfg(windows)]
mod windows_process;
#[cfg(target_os = "linux")]
use linux_impl::{
    command_name_impl, current_path_impl, environment_impl, executable_path_impl, fd_path_impl,
    is_live_impl, raw_environment_impl,
};
#[cfg(all(test, target_os = "macos"))]
use macos_impl::environment_from_macos_procargs;
#[cfg(target_os = "macos")]
use macos_impl::{
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
        environment_from_nul_entries, linux_cwd_path_string, raw_environment_from_nul_entries,
        window_name_from_process_name,
    };

    pub(super) fn current_path_impl(pid: u32) -> io::Result<Option<String>> {
        match std::fs::read_link(format!("/proc/{pid}/cwd")) {
            Ok(path) => Ok(Some(linux_cwd_path_string(path))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Reads the name from `/proc/<pid>/cmdline` only, as tmux 3.7b does.
    ///
    /// `cmdline` is empty while `execve` installs the new image, and `comm`
    /// is rewritten in that same window: a concurrent `comm` read can return
    /// a torn name such as `slee` while `bash` execs `sleep` (measured on
    /// Linux 7.0). Report no name instead; callers then fall back to the
    /// pane's own command, as tmux does.
    pub(super) fn command_name_impl(pid: u32) -> io::Result<Option<String>> {
        Ok(command_name_from_linux_cmdline(pid))
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
        window_name_from_process_name(std::str::from_utf8(first).ok()?)
    }

    #[cfg(test)]
    mod tests;

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

#[cfg(any(unix, test))]
fn environment_from_nul_entries(environ: &[u8]) -> Option<HashMap<String, String>> {
    let mut values = HashMap::new();
    for entry in environ.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let Ok(entry) = std::str::from_utf8(entry) else {
            continue;
        };
        let Some((name, value)) = entry.split_once('=') else {
            continue;
        };
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

/// tmux 3.7b `parse_window_name` applied to the name `osdep_get_name`
/// reports for a pane's foreground process: argv[0] on Linux, the executable
/// file name on macOS.
///
/// A process title set by `exec -a` or setproctitle (`my prog x`,
/// `nginx: master process nginx`) keeps only its first word, and only a name
/// that starts with `/` is reduced to its basename: `./a.out` stays whole.
///
/// Two deliberate divergences: a name that parses to nothing (`-`, `\x01`)
/// reports no name, so callers fall back to the pane's command instead of
/// showing an empty window name, and trailing non-ASCII characters are kept
/// where tmux's byte-wise `isalnum`/`ispunct` trim turns `café` into `caf`.
#[cfg(unix)]
fn window_name_from_process_name(process_name: &str) -> Option<String> {
    let mut name = process_name.strip_prefix('"').unwrap_or(process_name);
    if let Some(quote) = name.find('"') {
        name = &name[..quote];
    }
    name = name.strip_prefix("exec ").unwrap_or(name);
    name = name.trim_start_matches([' ', '-']);
    if let Some(space) = name.find(' ') {
        name = &name[..space];
    }
    name = name.trim_end_matches(|character: char| character.is_ascii_control());
    if name.starts_with('/') {
        // POSIX basename: trailing slashes go, and `/` stays `/`.
        let without_trailing = name.trim_end_matches('/');
        name = match without_trailing.rsplit_once('/') {
            Some((_, base)) => base,
            None => "/",
        };
    }
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(windows)]
fn executable_name(path: &str) -> Option<String> {
    let name = std::path::Path::new(path).file_name()?.to_string_lossy();
    let trimmed = name.trim_start_matches('-');
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
