use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::Command as ProcessCommand;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
};

use super::{connect_with_startserver, ExitFailure, StartupOptions};

pub(super) fn run_shell_startup(
    socket_path: &Path,
    startup: StartupOptions,
    shell_command: &str,
    login_shell: bool,
) -> Result<i32, ExitFailure> {
    let connection = connect_with_startserver(socket_path, startup)?;
    drop(connection);

    let shell = resolve_shell_startup_path();
    let argv0 = shell_argv0(&shell, login_shell);
    let mut command = ProcessCommand::new(&shell);
    configure_shell_command(&mut command, &argv0, shell_command);
    let status = command.env("SHELL", &shell).status().map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "failed to execute shell-command startup using '{}': {error}",
                shell.display()
            ),
        )
    })?;

    Ok(exit_code_from_status(status))
}

fn resolve_shell_startup_path() -> std::path::PathBuf {
    resolve_shell_startup_path_impl()
}

#[cfg(unix)]
fn resolve_shell_startup_path_impl() -> std::path::PathBuf {
    std::env::var_os("SHELL")
        .map(std::path::PathBuf::from)
        .filter(|path| usable_shell_path(path))
        .unwrap_or_else(|| std::path::PathBuf::from("/bin/sh"))
}

#[cfg(windows)]
fn resolve_shell_startup_path_impl() -> std::path::PathBuf {
    std::env::var_os("COMSPEC")
        .map(std::path::PathBuf::from)
        .filter(|path| usable_shell_path(path))
        .unwrap_or_else(|| std::path::PathBuf::from("cmd.exe"))
}

pub(super) fn usable_shell_path(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }

    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() || !is_executable_file(&metadata) {
        return false;
    }

    !std::env::current_exe()
        .ok()
        .is_some_and(|current| same_file_identity_for_paths(path, &current))
}

pub(super) fn same_file_identity_for_paths(left: &Path, right: &Path) -> bool {
    same_file_identity_for_paths_impl(left, right)
}

#[cfg(unix)]
fn same_file_identity_for_paths_impl(left: &Path, right: &Path) -> bool {
    let (Ok(left), Ok(right)) = (std::fs::metadata(left), std::fs::metadata(right)) else {
        return false;
    };
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(windows)]
fn same_file_identity_for_paths_impl(left: &Path, right: &Path) -> bool {
    let (Ok(left), Ok(right)) = (file_identity(left), file_identity(right)) else {
        return false;
    };
    left == right
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WindowsFileIdentity {
    volume_serial: u32,
    file_index_high: u32,
    file_index_low: u32,
}

#[cfg(windows)]
fn file_identity(path: &Path) -> std::io::Result<WindowsFileIdentity> {
    let file = std::fs::File::open(path)?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let ok = unsafe {
        // SAFETY: the file handle is valid for the duration of the call and
        // `info` is a writable out-parameter initialized by Windows on success.
        GetFileInformationByHandle(file.as_raw_handle(), &mut info)
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(WindowsFileIdentity {
        volume_serial: info.dwVolumeSerialNumber,
        file_index_high: info.nFileIndexHigh,
        file_index_low: info.nFileIndexLow,
    })
}

#[cfg(unix)]
fn is_executable_file(metadata: &std::fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(windows)]
fn is_executable_file(_metadata: &std::fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn configure_shell_command(command: &mut ProcessCommand, argv0: &OsString, shell_command: &str) {
    command.arg0(argv0).arg("-c").arg(shell_command);
}

#[cfg(windows)]
fn configure_shell_command(command: &mut ProcessCommand, _argv0: &OsString, shell_command: &str) {
    command.arg("/C");
    command.raw_arg(rmux_os::command::cmd_c_verbatim_tail(shell_command));
}

fn shell_argv0(shell: &Path, login_shell: bool) -> OsString {
    let name = shell
        .file_name()
        .unwrap_or(shell.as_os_str())
        .to_os_string();
    if !login_shell {
        return name;
    }

    let mut login_name = OsString::from("-");
    login_name.push(name);
    login_name
}

fn exit_code_from_status(status: std::process::ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| exit_signal(&status).map(|signal| 128 + signal))
        .unwrap_or(1)
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    status.signal()
}

#[cfg(windows)]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}
