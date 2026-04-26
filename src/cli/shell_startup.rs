use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::Command as ProcessCommand;

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

    !current_executable_identity().is_some_and(|current| same_file_identity(&metadata, &current))
}

fn current_executable_identity() -> Option<std::fs::Metadata> {
    std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
}

pub(super) fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    same_file_identity_impl(left, right)
}

#[cfg(unix)]
fn same_file_identity_impl(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(windows)]
fn same_file_identity_impl(_left: &std::fs::Metadata, _right: &std::fs::Metadata) -> bool {
    false
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
    command.arg("/C").arg(shell_command);
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
