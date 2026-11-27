use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use rmux_client::Connection;
#[cfg(unix)]
use rmux_proto::{ErrorResponse, OptionScopeSelector, Response};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
};

#[cfg(unix)]
use super::unexpected_response;
use super::{connect_with_startserver_outcome, ExitFailure, StartServerConnection, StartupOptions};
use crate::empty_server_lifecycle::shutdown_started_empty_server_at;

pub(super) fn run_shell_startup(
    socket_path: &Path,
    startup: StartupOptions,
    shell_command: &str,
    login_shell: bool,
) -> Result<i32, ExitFailure> {
    let endpoint = startup.endpoint.clone();
    let StartServerConnection {
        mut connection,
        provenance,
    } = connect_with_startserver_outcome(socket_path, startup)?;
    let shell = resolve_shell_startup_path(&mut connection)?;
    drop(connection);
    // tmux answers MSG_SHELL and drops the client before the shell runs, so
    // an empty server started by `-c` honours `exit-empty` at that point.
    shutdown_started_empty_server_at(&endpoint.socket_path(), provenance)
        .map_err(|error| ExitFailure::new(1, error.to_string()))?;

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

/// tmux answers MSG_SHELL with the server's global `default-shell`, which
/// the server resolves after its configuration, and replaces an unusable
/// shell by `_PATH_BSHELL`.
#[cfg(unix)]
fn resolve_shell_startup_path(connection: &mut Connection) -> Result<PathBuf, ExitFailure> {
    let response = connection
        .show_options(
            OptionScopeSelector::SessionGlobal,
            Some("default-shell".to_owned()),
            true,
            false,
            false,
        )
        .map_err(ExitFailure::from_client)?;
    match response {
        Response::ShowOptions(response) => Ok(shell_from_default_shell_value(
            response.command_output().stdout(),
        )),
        Response::Error(ErrorResponse { error }) => Err(ExitFailure::new(1, error.to_string())),
        response => Err(unexpected_response("show-options", &response)),
    }
}

#[cfg(unix)]
fn shell_from_default_shell_value(value: &[u8]) -> PathBuf {
    let value = value.strip_suffix(b"\n").unwrap_or(value);
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(value)))
        .filter(|path| usable_shell_path(path))
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

/// `cmd.exe /C` is the only command form this launcher builds on Windows,
/// so the server's `default-shell`, which may name another shell, is not
/// substituted here.
#[cfg(windows)]
fn resolve_shell_startup_path(_connection: &mut Connection) -> Result<PathBuf, ExitFailure> {
    Ok(std::env::var_os("COMSPEC")
        .map(PathBuf::from)
        .filter(|path| usable_shell_path(path))
        .unwrap_or_else(|| PathBuf::from("cmd.exe")))
}

pub(super) fn usable_shell_path(path: &Path) -> bool {
    usable_shell_path_with_current_executable(path, std::env::current_exe().ok().as_deref())
}

pub(super) fn usable_shell_path_with_current_executable(
    path: &Path,
    current: Option<&Path>,
) -> bool {
    if !path.is_absolute() {
        return false;
    }

    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() || !is_executable_file(&metadata) {
        return false;
    }

    !current.is_some_and(|current| same_file_identity_for_paths(path, current))
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
